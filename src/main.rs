//! Blame: who changed each line and when. The line under the cursor gets its author and age after
//! its text (a decoration's after-text, protocol 1.1 `decorations`) and in the status bar; resting
//! the pointer on a line shows its commit; commands list the history of the file or of the
//! selected lines and open a commit's changes in rhun's diff view. Everything comes from rhun's
//! Git data (`git-data`, `[needs] git = read`): rhun runs git, so a folder opened from a `rhun://`
//! link stays paused until the user trusts it, and the blame covers unsaved edits.
//!
//! The protocol's handler forwards events to a worker thread that owns the state and talks to
//! rhun, so typing never waits for git and blame requests are coalesced while the text changes.

mod git_info;
mod text;

use std::collections::HashMap;
use std::sync::mpsc::{channel, Receiver, RecvTimeoutError, Sender};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use git_info::{get_remote_url, resolve_commit_details, CommitDetails};
use rhun_ext::scm::{is_git_changed, is_uncommitted};
use rhun_ext::{
    run, Blame, Doc, Event, Ext, Feature, Handler, Hover, HoverRequest, LogEntry, LogOf, Opened,
    Pos, Range,
};

use text::{after_text, ago, enhanced_hover_text, history_item, line_lengths, EMPTY_TREE};

/// What the handler tells the worker.
enum Msg {
    Opened(Doc, String),
    Closed(Doc),
    Changed(Doc, i64),
    Cursor(Doc, u32),
    Selection(Doc, u32, u32),
    Hover(Doc, u32, u32),
    HoverQuery(HoverRequest, Sender<Option<Hover>>),
    Command(String, Doc),
    Inline(bool),
    GitChanged,
}

struct Blamer {
    tx: Option<Sender<Msg>>,
}

impl Blamer {
    fn send(&self, m: Msg) {
        if let Some(tx) = &self.tx {
            let _ = tx.send(m);
        }
    }
}

impl Handler for Blamer {
    fn ready(&mut self, ext: &Ext, _project: Option<&str>) {
        let (tx, rx) = channel();
        let ext = ext.clone();
        thread::spawn(move || Worker::new(ext).run(rx));
        self.tx = Some(tx);
    }

    fn event(&mut self, _ext: &Ext, ev: &Event) {
        if std::env::var_os("BLAME_DEBUG").is_some() {
            eprintln!("event {} {:?}", ev.name, ev.fields);
        }
        if let Some(o) = Opened::parse(ev) {
            if o.path != "-" {
                self.send(Msg::Opened(o.doc, o.path));
            }
            return;
        }
        if is_git_changed(ev) {
            self.send(Msg::GitChanged);
            return;
        }
        let n = |i: usize| ev.num(i);
        match ev.name.as_str() {
            "closed" => self.send(Msg::Closed(n(0))),
            "changed" => self.send(Msg::Changed(n(0), n(1))),
            "cursor" => self.send(Msg::Cursor(n(0), n(1) as u32)),
            "selection" => self.send(Msg::Selection(
                n(0),
                n(1).min(n(3)) as u32,
                n(1).max(n(3)) as u32,
            )),
            "hover" => self.send(Msg::Hover(n(0), n(1) as u32, n(2) as u32)),
            _ => {}
        }
    }

    fn command(&mut self, _ext: &Ext, name: &str, doc: Doc) {
        self.send(Msg::Command(name.to_string(), doc));
    }

    fn hover(&mut self, _ext: &Ext, req: &HoverRequest) -> Option<Hover> {
        let (tx, rx) = channel();
        self.send(Msg::HoverQuery(req.clone(), tx));
        rx.recv_timeout(Duration::from_millis(1000)).ok().flatten()
    }

    fn setting(&mut self, _ext: &Ext, key: &str, value: &str) {
        if key == "inline" {
            // a bool setting arrives as yes or no
            self.send(Msg::Inline(value == "yes" || value == "true"));
        }
    }
}

/// An open document as the worker knows it.
#[derive(Default)]
struct DocState {
    path: String,
    /// Its version as the last `changed` said (0 until one does).
    version: i64,
    /// The blame and the text's version and line lengths it was taken with.
    blame: Option<Blame>,
    blame_version: i64,
    lens: Vec<usize>,
    /// Why the last blame failed (until the text or the repository changes).
    error: Option<String>,
    /// Lines of the last selection (`l1 == l2` for a caret).
    selection: (u32, u32),
}

impl DocState {
    fn fresh(&self) -> bool {
        (self.blame.is_some() || self.error.is_some()) && self.blame_version >= self.version
    }
}

struct Worker {
    ext: Ext,
    docs: HashMap<Doc, DocState>,
    cursor: Option<(Doc, u32)>,
    inline: bool,
    /// When to blame the cursor's document again (after typing settles).
    due: Option<Instant>,
    /// The document whose decoration shows, and its version.
    shown: Option<Doc>,
    typed: bool,
    /// rhun agreed decorations and took the type.
    decorations: bool,
    repo_root: Option<String>,
    remote_url: Option<String>,
    commit_cache: HashMap<String, CommitDetails>,
}

const SETTLE: Duration = Duration::from_millis(500);

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

impl Worker {
    fn new(ext: Ext) -> Worker {
        Worker {
            ext,
            docs: HashMap::new(),
            cursor: None,
            inline: true,
            due: None,
            shown: None,
            typed: false,
            decorations: false,
            repo_root: None,
            remote_url: None,
            commit_cache: HashMap::new(),
        }
    }

    fn run(mut self, rx: Receiver<Msg>) {
        // without decorations (an older rhun) the status bar still says it
        self.decorations = self.ext.has(Feature::Named("decorations"))
            && self
                .ext
                .request("decoration-type blame", Some(b"after muted -\nwidth 0\n"))
                .is_ok();
        loop {
            let msg = match self.due {
                Some(t) => match rx.recv_timeout(t.saturating_duration_since(Instant::now())) {
                    Ok(m) => Some(m),
                    Err(RecvTimeoutError::Timeout) => None,
                    Err(RecvTimeoutError::Disconnected) => return,
                },
                None => match rx.recv() {
                    Ok(m) => Some(m),
                    Err(_) => return,
                },
            };
            match msg {
                Some(m) => self.handle(m),
                None => {
                    self.due = None;
                    self.show_cursor();
                }
            }
        }
    }

    fn handle(&mut self, m: Msg) {
        match m {
            Msg::Opened(doc, path) => {
                self.docs.insert(
                    doc,
                    DocState {
                        path,
                        ..Default::default()
                    },
                );
            }
            Msg::Closed(doc) => {
                self.docs.remove(&doc);
                if self.shown == Some(doc) {
                    self.shown = None;
                }
            }
            Msg::Changed(doc, version) => {
                if let Some(d) = self.docs.get_mut(&doc) {
                    d.version = version;
                }
                if self.cursor.map(|c| c.0) == Some(doc) {
                    self.typed = true;
                    self.due = Some(Instant::now() + SETTLE);
                }
            }
            Msg::Cursor(doc, line) => {
                self.cursor = Some((doc, line));
                if let Some(d) = self.docs.get_mut(&doc) {
                    d.selection = (line, line);
                }
                match self.docs.get(&doc) {
                    Some(_) if self.typed => {} // blamed when typing settles
                    Some(_) => self.show_cursor(),
                    None => self.clear(),
                }
            }
            Msg::Selection(doc, l1, l2) => {
                if let Some(d) = self.docs.get_mut(&doc) {
                    d.selection = (l1.max(1), l2.max(1));
                }
            }
            Msg::Hover(doc, line, col) => self.hover(doc, line, col),
            Msg::HoverQuery(req, tx) => {
                let hover = self.handle_hover_query(&req);
                let _ = tx.send(hover);
            }
            Msg::Command(name, doc) => self.command(&name, doc),
            Msg::Inline(on) => {
                self.inline = on;
                self.show_cursor();
            }
            Msg::GitChanged => {
                for d in self.docs.values_mut() {
                    d.blame = None;
                    d.error = None;
                    d.blame_version = 0;
                }
                self.repo_root = None;
                self.remote_url = None;
                self.commit_cache.clear();
                self.show_cursor();
            }
        }
    }

    /// Blames a document's current text (with its version and line lengths) unless it is fresh.
    fn ensure(&mut self, doc: Doc) {
        let Some(d) = self.docs.get(&doc) else { return };
        if d.fresh() {
            return;
        }
        let Ok((text, version)) = self.ext.text(doc) else {
            return;
        };
        let result = self.ext.git_blame(doc);
        let Some(d) = self.docs.get_mut(&doc) else {
            return;
        };
        d.version = d.version.max(version);
        d.blame_version = version;
        d.lens = line_lengths(&text);
        match result {
            Ok(b) => {
                d.blame = Some(b);
                d.error = None;
            }
            Err(e) => {
                d.blame = None;
                d.error = Some(format!("{} {}", e.code, e.text));
            }
        }
    }

    /// The cursor line's blame after its text and in the status bar.
    fn show_cursor(&mut self) {
        self.typed = false;
        let Some((doc, line)) = self.cursor else {
            return;
        };
        if !self.docs.contains_key(&doc) || !self.inline {
            self.clear();
            return;
        }
        self.ensure(doc);
        let now = now_secs();
        let d = &self.docs[&doc];
        let commit = d.blame.as_ref().and_then(|b| b.line(line));
        let (after, status) = match (commit, &d.error) {
            (Some(c), _) => {
                let s = if is_uncommitted(&c.sha) {
                    "Blame: not committed yet".to_string()
                } else {
                    format!("Blame: {}, {}", c.author, ago(now, c.time))
                };
                (Some(after_text(c, now)), s)
            }
            (None, Some(e)) if e.contains("paused") => {
                (None, "Blame: git is paused in this folder".to_string())
            }
            _ => (None, String::new()),
        };
        let version = d.blame_version;
        let len = d.lens.get((line as usize).wrapping_sub(1)).copied();
        let _ = self.ext.status(&status);
        match (after, len) {
            (Some(a), Some(len)) if self.decorations => {
                let body = format!("{line} 1 {line} {} 0 {} 0 0\n{a}", len + 1, a.len());
                let r = self.ext.request(
                    &format!("decorate {doc} {version} blame 0 0 1"),
                    Some(body.as_bytes()),
                );
                if r.is_ok() {
                    if let Some(old) = self.shown.filter(|&o| o != doc) {
                        self.undecorate(old);
                    }
                    self.shown = Some(doc);
                }
            }
            _ => {
                if let Some(old) = self.shown.take() {
                    self.undecorate(old);
                }
            }
        }
    }

    fn undecorate(&self, doc: Doc) {
        if !self.decorations {
            return;
        }
        let v = self
            .docs
            .get(&doc)
            .map(|d| d.blame_version.max(d.version))
            .unwrap_or(0);
        let _ = self
            .ext
            .request(&format!("decorate {doc} {v} blame 0 0 0"), Some(b""));
    }

    fn clear(&mut self) {
        if let Some(doc) = self.shown.take() {
            self.undecorate(doc);
        }
        let _ = self.ext.status("");
    }

    fn ensure_repo(&mut self) {
        if self.repo_root.is_none() {
            if let Ok(repo) = self.ext.git_repo() {
                self.remote_url = get_remote_url(&repo.root);
                self.repo_root = Some(repo.root);
            }
        }
    }

    fn get_commit_details(&mut self, sha: &str) -> Option<CommitDetails> {
        if is_uncommitted(sha) {
            return None;
        }
        if let Some(cached) = self.commit_cache.get(sha) {
            return Some(cached.clone());
        }
        let root = self.repo_root.as_ref()?;
        let details = resolve_commit_details(root, sha, self.remote_url.as_deref());
        self.commit_cache.insert(sha.to_string(), details.clone());
        Some(details)
    }

    fn hover(&mut self, doc: Doc, line: u32, col: u32) {
        if !self.docs.contains_key(&doc) {
            return;
        }
        self.ensure(doc);
        self.ensure_repo();
        let commit = self
            .docs
            .get(&doc)
            .and_then(|d| d.blame.as_ref())
            .and_then(|b| b.line(line))
            .cloned();
        if let Some(c) = commit {
            let details = self.get_commit_details(&c.sha);
            let text = enhanced_hover_text(&c, now_secs(), details.as_ref());
            let _ = self.ext.hover(doc, Pos::new(line, col), &text);
        }
    }

    fn handle_hover_query(&mut self, req: &HoverRequest) -> Option<Hover> {
        if !self.inline {
            return None;
        }
        if !self.docs.contains_key(&req.doc) {
            return None;
        }
        // Only show if the hover is on the active document and line where inline blame is shown
        let (cursor_doc, cursor_line) = self.cursor?;
        if req.doc != cursor_doc || req.at.line != cursor_line {
            return None;
        }
        let len = self
            .docs
            .get(&req.doc)
            .and_then(|d| d.lens.get((req.at.line as usize).wrapping_sub(1)).copied())
            .unwrap_or(0);
        // Only show if hovering past the code text on that line (i.e. on the blame annotation)
        if req.at.col <= len as u32 {
            return None;
        }
        self.ensure(req.doc);
        self.ensure_repo();
        let commit = self
            .docs
            .get(&req.doc)
            .and_then(|d| d.blame.as_ref())
            .and_then(|b| b.line(req.at.line))
            .cloned()?;
        let details = self.get_commit_details(&commit.sha);
        let text = enhanced_hover_text(&commit, now_secs(), details.as_ref());
        let end = if let Some(d) = self.docs.get(&req.doc) {
            if (req.at.line as usize) < d.lens.len() {
                Pos::new(req.at.line + 1, 1)
            } else {
                Pos::new(req.at.line, (len + 1) as u32)
            }
        } else {
            Pos::new(req.at.line, (len + 1) as u32)
        };
        let range = Range::new(Pos::new(req.at.line, (len + 1) as u32), end);
        Some(Hover::new(range, &text))
    }

    fn command(&mut self, name: &str, doc: Doc) {
        let doc = if self.docs.contains_key(&doc) {
            doc
        } else {
            self.cursor.map(|c| c.0).unwrap_or(0)
        };
        if name == "toggle" {
            self.inline = !self.inline;
            self.show_cursor();
            if !self.inline {
                let _ = self
                    .ext
                    .toast("Inline blame is off (Toggle Inline Blame turns it on)");
            }
            return;
        }
        let Some(d) = self.docs.get(&doc) else {
            let _ = self.ext.toast("Blame: open a file of the repository first");
            return;
        };
        let path = d.path.clone();
        let (l1, l2) = d.selection;
        match name {
            "file_history" => self.history(&path, LogOf::File(&path), "History of the file"),
            "line_history" => {
                let (a, b) = if l1 == 0 { (1, 1) } else { (l1, l2) };
                let title = if a == b {
                    format!("History of line {a}")
                } else {
                    format!("History of lines {a}-{b}")
                };
                self.history(&path, LogOf::Lines(&path, a, b), &title)
            }
            "open_commit" => self.open_commit(doc, &path),
            _ => {}
        }
    }

    fn history(&mut self, path: &str, of: LogOf, title: &str) {
        let entries = match self.ext.git_log(of, 200) {
            Ok(e) if !e.is_empty() => e,
            Ok(_) => {
                let _ = self
                    .ext
                    .toast("Blame: no commits touched it (is it committed?)");
                return;
            }
            Err(e) => {
                let _ = self.ext.toast(&format!("Blame: {}", e.text));
                return;
            }
        };
        let now = now_secs();
        let items: Vec<String> = entries.iter().map(|e| history_item(e, now)).collect();
        let refs: Vec<&str> = items.iter().map(String::as_str).collect();
        if let Ok(Some(i)) = self
            .ext
            .choose(&format!("{title}: open a commit's changes"), &refs)
        {
            self.open_diff(&entries[i], path);
        }
    }

    /// rhun's diff view of a commit's change to the file: from its first parent (the empty tree
    /// for a root commit) to it.
    fn open_diff(&self, e: &LogEntry, path: &str) {
        let from = e
            .parents
            .first()
            .cloned()
            .unwrap_or_else(|| EMPTY_TREE.to_string());
        if let Err(err) = self.ext.git_diff_open(&from, Some(&e.sha), path) {
            let _ = self.ext.toast(&format!("Blame: {}", err.text));
        }
    }

    fn open_commit(&mut self, doc: Doc, path: &str) {
        self.ensure(doc);
        let line = self.cursor.filter(|c| c.0 == doc).map(|c| c.1).unwrap_or(1);
        let Some(sha) = self.docs[&doc]
            .blame
            .as_ref()
            .and_then(|b| b.line(line))
            .map(|c| c.sha.clone())
        else {
            let _ = self
                .ext
                .toast("Blame: this line has no commit (is the file committed?)");
            return;
        };
        if is_uncommitted(&sha) {
            // not committed: what changed since HEAD
            if let Err(e) = self.ext.git_diff_open("HEAD", None, path) {
                let _ = self.ext.toast(&format!("Blame: {}", e.text));
            }
            return;
        }
        // the commit's parents come from the file's history
        let entry = self
            .ext
            .git_log(LogOf::File(path), 1000)
            .ok()
            .and_then(|log| log.into_iter().find(|e| e.sha == sha))
            .unwrap_or(LogEntry {
                sha: sha.clone(),
                parents: vec![format!("{sha}^")],
                time: 0,
                author: String::new(),
                subject: String::new(),
            });
        self.open_diff(&entry, path);
    }
}

fn main() {
    let features = [
        Feature::GitData,
        Feature::Named("decorations"),
        Feature::HoverQuery,
        Feature::Named("markdown"),
    ];
    if let Err(e) = run(&features, Blamer { tx: None }) {
        eprintln!("blame: {e}");
        std::process::exit(1);
    }
}
