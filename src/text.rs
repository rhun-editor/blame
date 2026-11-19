//! The words Blame shows: ages, the after-line text, the hover card, history rows.

use rhun_ext::scm::is_uncommitted;
use rhun_ext::{BlameCommit, LogEntry};

use crate::git_info::CommitDetails;

/// Git's empty tree: what a root commit's change is compared with.
pub const EMPTY_TREE: &str = "4b825dc642cb6eb9a060e54bf8d69288fbee4904";

/// "just now", "5 minutes ago", "3 days ago", "2 years ago": the time since `t` (seconds).
pub fn ago(now: u64, t: u64) -> String {
    let s = now.saturating_sub(t);
    let (n, unit) = match s {
        0..=59 => return "just now".into(),
        60..=3599 => (s / 60, "minute"),
        3600..=86_399 => (s / 3600, "hour"),
        86_400..=2_591_999 => (s / 86_400, "day"),
        2_592_000..=31_535_999 => (s / 2_592_000, "month"),
        _ => (s / 31_536_000, "year"),
    };
    format!("{n} {unit}{} ago", if n == 1 { "" } else { "s" })
}

/// 2026-10-16: a date from seconds since 1970 (UTC).
pub fn date(t: u64) -> String {
    // days to civil date (Howard Hinnant's algorithm)
    let z = (t / 86_400) as i64 + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!("{y:04}-{m:02}-{d:02}")
}

/// At most `max` bytes of `s` on one line, cut at a character with "…".
pub fn one_line(s: &str, max: usize) -> String {
    let s: String = s
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();
    if s.len() <= max {
        return s;
    }
    let mut cut = max.saturating_sub(3);
    while !s.is_char_boundary(cut) {
        cut -= 1;
    }
    format!("{}…", &s[..cut])
}

/// The text after the cursor's line: "    Ada Lovelace, 3 days ago • Fix the parser".
pub fn after_text(c: &BlameCommit, now: u64) -> String {
    if is_uncommitted(&c.sha) {
        return "    You, not committed yet".into();
    }
    one_line(
        &format!("    {}, {} • {}", c.author, ago(now, c.time), c.summary),
        200,
    )
}

/// The hover card of a line's commit.
pub fn hover_text(c: &BlameCommit, now: u64) -> String {
    if is_uncommitted(&c.sha) {
        return "Not committed yet: the line changed since the last commit (or is not saved)."
            .into();
    }
    let short = &c.sha[..c.sha.len().min(7)];
    let mail = if c.mail.is_empty() {
        String::new()
    } else {
        format!(" <{}>", c.mail)
    };
    let card = format!(
        "{}{mail}, {} ({})\n{short} {}",
        c.author,
        ago(now, c.time),
        date(c.time),
        c.summary
    );
    // the protocol's hover body: at most 4000 bytes
    if card.len() > 3900 {
        one_line(&card, 3900)
    } else {
        card
    }
}

/// The enhanced hover card of a line's commit with full message, author date, PR and web links.
pub fn enhanced_hover_text(c: &BlameCommit, now: u64, details: Option<&CommitDetails>) -> String {
    if is_uncommitted(&c.sha) {
        return "Not committed yet: the line changed since the last commit (or is not saved)."
            .into();
    }

    let Some(d) = details else {
        return hover_text(c, now);
    };

    let author_name = if !d.author_name.is_empty() {
        &d.author_name
    } else {
        &c.author
    };
    let author_email = if !d.author_email.is_empty() {
        &d.author_email
    } else {
        &c.mail
    };
    let mail = if author_email.is_empty() {
        String::new()
    } else {
        format!(" <{author_email}>")
    };

    let mut card = format!("{author_name}{mail}  \n");

    let date_str = if !d.author_date.is_empty() {
        format!("{} ({})", d.author_date, ago(now, c.time))
    } else {
        format!("{} ({})", date(c.time), ago(now, c.time))
    };
    card.push_str(&format!("Date:   {date_str}  \n"));

    let short = &c.sha[..c.sha.len().min(8)];
    if let Some(url) = &d.commit_url {
        card.push_str(&format!("Commit: [{short}]({url})  \n"));
    } else {
        card.push_str(&format!("Commit: {short}  \n"));
    }

    if let Some(pr_num) = d.pr_number {
        if let Some(url) = &d.pr_url {
            card.push_str(&format!("PR:     [#{pr_num}]({url})  \n"));
        } else {
            card.push_str(&format!("PR:     #{pr_num}  \n"));
        }
    }

    card.push('\n');

    let summary = if !d.summary.is_empty() {
        &d.summary
    } else {
        &c.summary
    };
    card.push_str(summary);
    card.push('\n');

    if !d.body.is_empty() {
        card.push('\n');
        let lines: Vec<&str> = d.body.lines().take(15).collect();
        card.push_str(&lines.join("\n"));
        if d.body.lines().count() > 15 {
            card.push_str("\n…");
        }
        card.push('\n');
    }

    if card.len() > 3900 {
        one_line(&card, 3900)
    } else {
        card.trim_end().to_string()
    }
}

/// A row of a history list: "1a2b3c4  3 days ago  Ada Lovelace: Fix the parser".
pub fn history_item(e: &LogEntry, now: u64) -> String {
    let short = &e.sha[..e.sha.len().min(7)];
    one_line(
        &format!("{short}  {}  {}: {}", ago(now, e.time), e.author, e.subject),
        160,
    )
}

/// The byte length of each line of a text (without its line feed).
pub fn line_lengths(text: &[u8]) -> Vec<usize> {
    text.split(|&b| b == b'\n').map(|l| l.len()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn commit(sha: &str, time: u64) -> BlameCommit {
        BlameCommit {
            sha: sha.into(),
            time,
            author: "Ada Lovelace".into(),
            mail: "ada@example.invalid".into(),
            summary: "Fix the parser".into(),
        }
    }

    #[test]
    fn ages() {
        assert_eq!(ago(1000, 1000), "just now");
        assert_eq!(ago(1000 + 60, 1000), "1 minute ago");
        assert_eq!(ago(1000 + 7200, 1000), "2 hours ago");
        assert_eq!(ago(1000 + 3 * 86_400, 1000), "3 days ago");
        assert_eq!(ago(1000 + 70 * 86_400, 1000), "2 months ago");
        assert_eq!(ago(1000 + 800 * 86_400, 1000), "2 years ago");
        assert_eq!(ago(5, 10), "just now"); // a clock behind the commit
    }

    #[test]
    fn dates() {
        assert_eq!(date(0), "1970-01-01");
        assert_eq!(date(1_700_000_000), "2023-11-14");
        assert_eq!(date(951_782_400), "2000-02-29");
    }

    #[test]
    fn texts() {
        let now = 1_700_000_000 + 3 * 86_400;
        let c = commit(&"a".repeat(40), 1_700_000_000);
        assert_eq!(
            after_text(&c, now),
            "    Ada Lovelace, 3 days ago • Fix the parser"
        );
        assert_eq!(
            hover_text(&c, now),
            "Ada Lovelace <ada@example.invalid>, 3 days ago (2023-11-14)\naaaaaaa Fix the parser"
        );
        let z = commit(&"0".repeat(40), 0);
        assert_eq!(after_text(&z, now), "    You, not committed yet");
        let long = BlameCommit {
            summary: "é".repeat(200),
            ..c.clone()
        };
        let a = after_text(&long, now);
        assert!(a.len() <= 200 && a.ends_with('…'), "{a}");
        let e = LogEntry {
            sha: "b".repeat(40),
            parents: vec![],
            time: 1_700_000_000,
            author: "Ada".into(),
            subject: "First".into(),
        };
        assert_eq!(history_item(&e, now), "bbbbbbb  3 days ago  Ada: First");

        let details = CommitDetails {
            sha: "a".repeat(40),
            author_name: "Ada Lovelace".into(),
            author_email: "ada@example.invalid".into(),
            author_date: "2023-11-14 10:00:00 +0000".into(),
            summary: "Fix the parser".into(),
            body: "Resolves a critical edge case.\nSee issue description.".into(),
            pr_number: Some(42),
            pr_title: Some("Fix parser (#42)".into()),
            commit_url: Some("https://github.com/rhun-editor/blame/commit/aaaaaaaa".into()),
            pr_url: Some("https://github.com/rhun-editor/blame/pull/42".into()),
        };
        let eh = enhanced_hover_text(&c, now, Some(&details));
        assert!(eh.contains("Ada Lovelace <ada@example.invalid>"));
        assert!(eh.contains("Date:   2023-11-14 10:00:00 +0000 (3 days ago)"));
        assert!(
            eh.contains("Commit: [aaaaaaaa](https://github.com/rhun-editor/blame/commit/aaaaaaaa)")
        );
        assert!(eh.contains("PR:     [#42](https://github.com/rhun-editor/blame/pull/42)"));
        assert!(eh.contains("Fix the parser"));
        assert!(eh.contains("Resolves a critical edge case."));
    }

    #[test]
    fn lengths() {
        assert_eq!(line_lengths("ab\n\ncdé\n".as_bytes()), vec![2, 0, 4, 0]);
        assert_eq!(line_lengths(b""), vec![0]);
    }
}
