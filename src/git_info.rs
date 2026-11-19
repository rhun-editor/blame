//! Local Git inspection for detailed commit information, PR resolution, and web links.

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CommitDetails {
    pub sha: String,
    pub author_name: String,
    pub author_email: String,
    pub author_date: String,
    pub summary: String,
    pub body: String,
    pub pr_number: Option<u64>,
    pub pr_title: Option<String>,
    pub commit_url: Option<String>,
    pub pr_url: Option<String>,
}

/// Normalizes a git remote URL into an HTTPS base web link.
/// E.g.
/// - `git@github.com:owner/repo.git` -> `https://github.com/owner/repo`
/// - `https://github.com/owner/repo.git` -> `https://github.com/owner/repo`
/// - `ssh://git@gitlab.com/owner/repo.git` -> `https://gitlab.com/owner/repo`
pub fn clean_remote_url(raw: &str) -> String {
    let mut url = raw.trim();
    if url.ends_with(".git") {
        url = &url[..url.len() - 4];
    }
    if let Some(rest) = url.strip_prefix("git@") {
        if let Some((host, path)) = rest.split_once(':') {
            return format!("https://{host}/{path}");
        }
    }
    if let Some(rest) = url.strip_prefix("ssh://") {
        let without_user = rest.split_once('@').map(|x| x.1).unwrap_or(rest);
        return format!("https://{without_user}");
    }
    if url.starts_with("http://") || url.starts_with("https://") {
        return url.to_string();
    }
    format!("https://{url}")
}

/// Extracts a PR number and optional title from commit summary or body text.
pub fn extract_pr_from_text(summary: &str, body: &str) -> Option<(u64, Option<String>)> {
    // 1. "Merge pull request #123 from user/branch"
    if let Some(pos) = summary.find("Merge pull request #") {
        let rest = &summary[pos + "Merge pull request #".len()..];
        let num_str: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
        if let Ok(num) = num_str.parse::<u64>() {
            return Some((num, Some(summary.to_string())));
        }
    }
    // 2. Squash merge pattern: "... (#123)" in summary or body
    for text in [summary, body] {
        if let Some(start) = text.find("(#") {
            let rest = &text[start + 2..];
            let num_str: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
            if !num_str.is_empty() && rest.starts_with(&format!("{num_str})")) {
                if let Ok(num) = num_str.parse::<u64>() {
                    return Some((num, Some(summary.to_string())));
                }
            }
        }
        // GitLab: "(!123)"
        if let Some(start) = text.find("(!") {
            let rest = &text[start + 2..];
            let num_str: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
            if !num_str.is_empty() && rest.starts_with(&format!("{num_str})")) {
                if let Ok(num) = num_str.parse::<u64>() {
                    return Some((num, Some(summary.to_string())));
                }
            }
        }
    }
    None
}

/// Finds the merge commit that introduced `sha` into HEAD via ancestry path.
pub fn find_merge_pr(root: &str, sha: &str) -> Option<(u64, Option<String>)> {
    let out = std::process::Command::new("git")
        .args([
            "-C",
            root,
            "log",
            "--merges",
            "--ancestry-path",
            "--reverse",
            "--oneline",
            &format!("{sha}..HEAD"),
        ])
        .output()
        .ok()?;

    if !out.status.success() {
        return None;
    }

    let stdout = String::from_utf8_lossy(&out.stdout);
    for line in stdout.lines() {
        if let Some(pos) = line.find("Merge pull request #") {
            let rest = &line[pos + "Merge pull request #".len()..];
            let num_str: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
            if let Ok(num) = num_str.parse::<u64>() {
                return Some((num, Some(line.to_string())));
            }
        }
        if let Some(pos) = line.find("merge request !") {
            let rest = &line[pos + "merge request !".len()..];
            let num_str: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
            if let Ok(num) = num_str.parse::<u64>() {
                return Some((num, Some(line.to_string())));
            }
        }
        if let Some((num, title)) = extract_pr_from_text(line, "") {
            return Some((num, title));
        }
    }
    None
}

/// Fetches the configured origin remote URL for the git repository.
pub fn get_remote_url(root: &str) -> Option<String> {
    let out = std::process::Command::new("git")
        .args(["-C", root, "config", "--get", "remote.origin.url"])
        .output()
        .ok()?;
    if out.status.success() {
        let raw = String::from_utf8_lossy(&out.stdout).trim().to_string();
        if !raw.is_empty() {
            return Some(clean_remote_url(&raw));
        }
    }
    None
}

/// Resolves complete commit details from the local repository for `sha`.
pub fn resolve_commit_details(root: &str, sha: &str, base_url: Option<&str>) -> CommitDetails {
    let mut details = CommitDetails {
        sha: sha.to_string(),
        ..Default::default()
    };

    let output = std::process::Command::new("git")
        .args([
            "-C",
            root,
            "log",
            "-1",
            "--format=%an%x00%ae%x00%ad%x00%s%x00%b",
            "--date=format:%Y-%m-%d %H:%M:%S %z",
            sha,
        ])
        .output();

    if let Ok(out) = output {
        if out.status.success() {
            let s = String::from_utf8_lossy(&out.stdout);
            let parts: Vec<&str> = s.split('\0').collect();
            if parts.len() >= 4 {
                details.author_name = parts[0].trim().to_string();
                details.author_email = parts[1].trim().to_string();
                details.author_date = parts[2].trim().to_string();
                details.summary = parts[3].trim().to_string();
                if parts.len() >= 5 {
                    details.body = parts[4].trim().to_string();
                }
            }
        }
    }

    let (pr_num, pr_title) = extract_pr_from_text(&details.summary, &details.body)
        .or_else(|| find_merge_pr(root, sha))
        .map(|(num, title)| (Some(num), title))
        .unwrap_or((None, None));

    details.pr_number = pr_num;
    details.pr_title = pr_title;

    if let Some(base) = base_url {
        let is_gitlab = base.contains("gitlab");
        let is_bitbucket = base.contains("bitbucket");
        let is_gitea_or_codeberg =
            base.contains("codeberg") || base.contains("gitea") || base.contains("forgejo");
        if is_gitlab {
            details.commit_url = Some(format!("{base}/-/commit/{sha}"));
            if let Some(pr) = details.pr_number {
                details.pr_url = Some(format!("{base}/-/merge_requests/{pr}"));
            }
        } else if is_bitbucket {
            details.commit_url = Some(format!("{base}/commits/{sha}"));
            if let Some(pr) = details.pr_number {
                details.pr_url = Some(format!("{base}/pull-requests/{pr}"));
            }
        } else if is_gitea_or_codeberg {
            details.commit_url = Some(format!("{base}/commit/{sha}"));
            if let Some(pr) = details.pr_number {
                details.pr_url = Some(format!("{base}/pulls/{pr}"));
            }
        } else {
            // Default (GitHub, GitHub Enterprise, and compatible hosts)
            details.commit_url = Some(format!("{base}/commit/{sha}"));
            if let Some(pr) = details.pr_number {
                details.pr_url = Some(format!("{base}/pull/{pr}"));
            }
        }
    }

    details
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_clean_remote_url() {
        assert_eq!(
            clean_remote_url("git@github.com:rhun-editor/blame.git"),
            "https://github.com/rhun-editor/blame"
        );
        assert_eq!(
            clean_remote_url("https://github.com/rhun-editor/blame.git"),
            "https://github.com/rhun-editor/blame"
        );
        assert_eq!(
            clean_remote_url("ssh://git@gitlab.com/my-org/my-repo.git"),
            "https://gitlab.com/my-org/my-repo"
        );
    }

    #[test]
    fn test_extract_pr() {
        let (num, _) =
            extract_pr_from_text("Merge pull request #123 from feat/branch", "").unwrap();
        assert_eq!(num, 123);

        let (num, _) = extract_pr_from_text("Fix crash in parser (#456)", "").unwrap();
        assert_eq!(num, 456);

        let (num, _) = extract_pr_from_text("Refactor", "Resolves issues (!789)").unwrap();
        assert_eq!(num, 789);
    }

    #[test]
    fn test_remote_url_formats() {
        // Bitbucket
        assert_eq!(
            clean_remote_url("git@bitbucket.org:team/repo.git"),
            "https://bitbucket.org/team/repo"
        );
        // Codeberg / Gitea
        assert_eq!(
            clean_remote_url("https://codeberg.org/user/repo.git"),
            "https://codeberg.org/user/repo"
        );
    }
}
