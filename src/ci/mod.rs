//! CI integration module for git-ai.
//!
//! Detects CI environments (GitHub Actions, GitLab CI), computes attribution
//! reports for pull request diffs, and optionally posts PR comments.

use std::collections::HashMap;
use std::path::Path;
use std::process::{Command, Stdio};

use crate::core::authorship_log::{AuthorshipLog, LineRange};

// ---------------------------------------------------------------------------
// CI Environment Detection
// ---------------------------------------------------------------------------

/// The CI provider that is running the current build.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CiProvider {
    GitHubActions,
    GitLabCi,
    Unknown,
}

/// Context about the CI environment: provider, repository, PR, and commit info.
#[derive(Debug, Clone)]
pub struct CiContext {
    pub provider: CiProvider,
    pub repo_owner: String,
    pub repo_name: String,
    pub pr_number: Option<u64>,
    pub commit_sha: String,
    pub base_ref: Option<String>,
    pub head_ref: Option<String>,
}

/// Detect the current CI environment from environment variables.
///
/// Returns `None` if we are not running inside a recognized CI system.
pub fn detect_ci() -> Option<CiContext> {
    if let Some(ctx) = detect_github_actions() {
        return Some(ctx);
    }
    if let Some(ctx) = detect_gitlab_ci() {
        return Some(ctx);
    }
    None
}

fn detect_github_actions() -> Option<CiContext> {
    if std::env::var("GITHUB_ACTIONS").as_deref() != Ok("true") {
        return None;
    }

    let repository = std::env::var("GITHUB_REPOSITORY").unwrap_or_default();
    let (owner, name) = split_owner_repo(&repository);
    let commit_sha = std::env::var("GITHUB_SHA").unwrap_or_default();
    let github_ref = std::env::var("GITHUB_REF").unwrap_or_default();
    let base_ref = std::env::var("GITHUB_BASE_REF").ok().filter(|s| !s.is_empty());
    let head_ref = std::env::var("GITHUB_HEAD_REF").ok().filter(|s| !s.is_empty());

    // Try to extract PR number from GITHUB_REF (refs/pull/123/merge)
    let mut pr_number = parse_pr_number_from_ref(&github_ref);

    // Fallback: try to read the event payload JSON
    if pr_number.is_none() {
        if let Ok(event_path) = std::env::var("GITHUB_EVENT_PATH") {
            pr_number = parse_pr_number_from_event_file(&event_path);
        }
    }

    Some(CiContext {
        provider: CiProvider::GitHubActions,
        repo_owner: owner,
        repo_name: name,
        pr_number,
        commit_sha,
        base_ref,
        head_ref,
    })
}

fn detect_gitlab_ci() -> Option<CiContext> {
    if std::env::var("GITLAB_CI").as_deref() != Ok("true") {
        return None;
    }

    let project_path = std::env::var("CI_PROJECT_PATH").unwrap_or_default();
    let (owner, name) = split_owner_repo(&project_path);
    let commit_sha = std::env::var("CI_COMMIT_SHA").unwrap_or_default();
    let pr_number = std::env::var("CI_MERGE_REQUEST_IID")
        .ok()
        .and_then(|s| s.parse::<u64>().ok());
    let base_ref = std::env::var("CI_MERGE_REQUEST_TARGET_BRANCH_NAME")
        .ok()
        .filter(|s| !s.is_empty());
    let head_ref = std::env::var("CI_MERGE_REQUEST_SOURCE_BRANCH_NAME")
        .ok()
        .filter(|s| !s.is_empty());

    Some(CiContext {
        provider: CiProvider::GitLabCi,
        repo_owner: owner,
        repo_name: name,
        pr_number,
        commit_sha,
        base_ref,
        head_ref,
    })
}

/// Split "owner/repo" into (owner, repo). Handles nested GitLab groups by using
/// the first path segment as owner and the rest as repo name.
fn split_owner_repo(s: &str) -> (String, String) {
    if let Some(slash_pos) = s.find('/') {
        (s[..slash_pos].to_string(), s[slash_pos + 1..].to_string())
    } else {
        (String::new(), s.to_string())
    }
}

/// Parse PR number from a GitHub ref like "refs/pull/123/merge".
fn parse_pr_number_from_ref(github_ref: &str) -> Option<u64> {
    let parts: Vec<&str> = github_ref.split('/').collect();
    // Expected: ["refs", "pull", "123", "merge"]
    if parts.len() >= 4 && parts[0] == "refs" && parts[1] == "pull" {
        parts[2].parse::<u64>().ok()
    } else {
        None
    }
}

/// Parse PR number from the GitHub event JSON file (e.g., pull_request.number).
fn parse_pr_number_from_event_file(path: &str) -> Option<u64> {
    let content = std::fs::read_to_string(path).ok()?;
    let value: serde_json::Value = serde_json::from_str(&content).ok()?;
    // pull_request events have .pull_request.number or .number at top level
    value
        .get("pull_request")
        .and_then(|pr| pr.get("number"))
        .and_then(|n| n.as_u64())
        .or_else(|| value.get("number").and_then(|n| n.as_u64()))
}

// ---------------------------------------------------------------------------
// Attribution Report
// ---------------------------------------------------------------------------

/// Per-file attribution breakdown.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileAttribution {
    pub path: String,
    pub ai_lines: usize,
    pub human_lines: usize,
    pub untracked_lines: usize,
}

/// Aggregate attribution report across all files in a PR diff.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AttributionReport {
    pub total_lines: usize,
    pub ai_lines: usize,
    pub human_lines: usize,
    pub untracked_lines: usize,
    pub files: Vec<FileAttribution>,
}

/// Compute an attribution report for the diff between `base` and `head`.
///
/// `repo_path` must point to the root of a git repository. This function shells
/// out to git to determine changed files, parse diffs, and read authorship notes.
pub fn compute_report(repo_path: &Path, base: &str, head: &str) -> Result<AttributionReport, String> {
    // 1. Get list of changed files
    let changed_files = git_in(repo_path, &["diff", "--name-only", format!("{}..{}", base, head).as_str()])?;
    let file_paths: Vec<&str> = changed_files.lines().filter(|l| !l.is_empty()).collect();

    if file_paths.is_empty() {
        return Ok(AttributionReport {
            total_lines: 0,
            ai_lines: 0,
            human_lines: 0,
            untracked_lines: 0,
            files: Vec::new(),
        });
    }

    // 2. Get the added/modified line numbers per file from the diff
    let diff_output = git_in(
        repo_path,
        &["diff", "-U0", format!("{}..{}", base, head).as_str()],
    )?;
    let added_lines_by_file = parse_diff_added_lines(&diff_output);

    // 3. Read authorship note for the head commit
    let note_content = git_in(repo_path, &["notes", "--ref=ai", "show", head]);
    let authorship = note_content
        .ok()
        .and_then(|s| AuthorshipLog::deserialize_from_string(&s).ok());

    // Build a lookup: file_path -> { hash -> set of line numbers }
    let mut file_attestations: HashMap<&str, Vec<(&str, Vec<u32>)>> = HashMap::new();
    if let Some(ref log) = authorship {
        for file_att in &log.attestations {
            let mut entries_expanded = Vec::new();
            for entry in &file_att.entries {
                let lines: Vec<u32> = entry.line_ranges.iter().flat_map(LineRange::expand).collect();
                entries_expanded.push((entry.hash.as_str(), lines));
            }
            file_attestations.insert(&file_att.file_path, entries_expanded);
        }
    }

    // Determine which hashes are AI (prompt/session) vs human
    let ai_hashes: std::collections::HashSet<&str> = authorship
        .as_ref()
        .map(|log| {
            let mut set = std::collections::HashSet::new();
            for key in log.metadata.prompts.keys() {
                set.insert(key.as_str());
            }
            for key in log.metadata.sessions.keys() {
                set.insert(key.as_str());
            }
            set
        })
        .unwrap_or_default();

    let human_hashes: std::collections::HashSet<&str> = authorship
        .as_ref()
        .map(|log| {
            let mut set = std::collections::HashSet::new();
            for key in log.metadata.humans.keys() {
                set.insert(key.as_str());
            }
            set
        })
        .unwrap_or_default();

    // 4. For each file, classify the added lines
    let mut report = AttributionReport {
        total_lines: 0,
        ai_lines: 0,
        human_lines: 0,
        untracked_lines: 0,
        files: Vec::new(),
    };

    for file_path in &file_paths {
        let added_lines = match added_lines_by_file.get(*file_path) {
            Some(lines) => lines,
            None => continue,
        };

        if added_lines.is_empty() {
            continue;
        }

        let mut file_ai = 0usize;
        let mut file_human = 0usize;
        let mut file_untracked = 0usize;

        let attestation_entries = file_attestations.get(*file_path);

        for &line_num in added_lines {
            let mut classified = false;

            if let Some(entries) = attestation_entries {
                for (hash, lines) in entries {
                    if lines.contains(&line_num) {
                        if ai_hashes.contains(hash) {
                            file_ai += 1;
                        } else if human_hashes.contains(hash) {
                            file_human += 1;
                        } else {
                            file_untracked += 1;
                        }
                        classified = true;
                        break;
                    }
                }
            }

            if !classified {
                file_untracked += 1;
            }
        }

        report.files.push(FileAttribution {
            path: file_path.to_string(),
            ai_lines: file_ai,
            human_lines: file_human,
            untracked_lines: file_untracked,
        });

        report.ai_lines += file_ai;
        report.human_lines += file_human;
        report.untracked_lines += file_untracked;
    }

    report.total_lines = report.ai_lines + report.human_lines + report.untracked_lines;
    Ok(report)
}

/// Parse unified diff output (with -U0) to extract which line numbers were added
/// in each file. Returns a map of file_path -> sorted vec of added line numbers.
fn parse_diff_added_lines(diff: &str) -> HashMap<String, Vec<u32>> {
    let mut result: HashMap<String, Vec<u32>> = HashMap::new();
    let mut current_file: Option<String> = None;

    for line in diff.lines() {
        if line.starts_with("+++ b/") {
            current_file = Some(line[6..].to_string());
        } else if line.starts_with("@@ ") {
            // Parse hunk header like "@@ -a,b +c,d @@" -- we want +c,d (new file side)
            if let Some(ref file) = current_file {
                if let Some((start, count)) = parse_hunk_header_new_side(line) {
                    let lines = result.entry(file.clone()).or_default();
                    for i in 0..count {
                        lines.push(start + i);
                    }
                }
            }
        }
    }

    result
}

/// Parse the new-side ('+' side) of a hunk header.
/// Handles: "@@ -a,b +c,d @@", "@@ -a +c @@", "@@ -a,b +c @@"
fn parse_hunk_header_new_side(line: &str) -> Option<(u32, u32)> {
    let plus_idx = line.find(" +")?;
    let after_plus = &line[plus_idx + 2..];
    let end_idx = after_plus.find(' ').unwrap_or(after_plus.len());
    let range_str = &after_plus[..end_idx];

    if let Some(comma_idx) = range_str.find(',') {
        let start: u32 = range_str[..comma_idx].parse().ok()?;
        let count: u32 = range_str[comma_idx + 1..].parse().ok()?;
        Some((start, count))
    } else {
        let start: u32 = range_str.parse().ok()?;
        // No comma means a single line was added
        Some((start, 1))
    }
}

// ---------------------------------------------------------------------------
// Report Formatting
// ---------------------------------------------------------------------------

/// Format an attribution report as a markdown table suitable for a PR comment.
pub fn format_markdown(report: &AttributionReport) -> String {
    let mut out = String::new();

    out.push_str("## git-ai Attribution Report\n\n");

    // Summary table
    out.push_str("| Metric | Lines | % |\n");
    out.push_str("|--------|-------|---|\n");

    let total = report.total_lines;
    out.push_str(&format!(
        "| AI-authored | {} | {}% |\n",
        report.ai_lines,
        percentage(report.ai_lines, total)
    ));
    out.push_str(&format!(
        "| Human-authored | {} | {}% |\n",
        report.human_lines,
        percentage(report.human_lines, total)
    ));
    out.push_str(&format!(
        "| Untracked | {} | {}% |\n",
        report.untracked_lines,
        percentage(report.untracked_lines, total)
    ));

    // Per-file breakdown (only if there are files)
    if !report.files.is_empty() {
        out.push_str("\n### Per-file breakdown\n\n");
        out.push_str("| File | AI | Human | Untracked |\n");
        out.push_str("|------|-----|-------|-----------|\n");

        for file in &report.files {
            out.push_str(&format!(
                "| {} | {} | {} | {} |\n",
                file.path, file.ai_lines, file.human_lines, file.untracked_lines
            ));
        }
    }

    out
}

fn percentage(part: usize, total: usize) -> usize {
    if total == 0 {
        0
    } else {
        (part * 100 + total / 2) / total
    }
}

// ---------------------------------------------------------------------------
// GitHub PR Comment
// ---------------------------------------------------------------------------

/// Post (or print) a report comment on a GitHub PR.
///
/// If the `gh` CLI is available, posts the comment to the PR. Otherwise,
/// prints the formatted body to stdout so it appears in CI logs.
pub fn post_github_comment(context: &CiContext, body: &str) -> Result<(), String> {
    let pr_number = context
        .pr_number
        .ok_or_else(|| "no PR number available in CI context".to_string())?;

    let repo_slug = format!("{}/{}", context.repo_owner, context.repo_name);

    // Check if `gh` is available
    let gh_available = Command::new("gh")
        .arg("--version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|s| s.success());

    if !gh_available {
        println!("{}", body);
        return Ok(());
    }

    let status = Command::new("gh")
        .args([
            "pr",
            "comment",
            &pr_number.to_string(),
            "--body",
            body,
            "--repo",
            &repo_slug,
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .status()
        .map_err(|e| format!("failed to run gh: {}", e))?;

    if status.success() {
        Ok(())
    } else {
        // Fallback: print to stdout
        println!("{}", body);
        Err("gh pr comment failed; report printed to stdout instead".to_string())
    }
}

// ---------------------------------------------------------------------------
// Internal git helper
// ---------------------------------------------------------------------------

fn git_in(repo_path: &Path, args: &[&str]) -> Result<String, String> {
    let output = Command::new("/usr/bin/git")
        .arg("-C")
        .arg(repo_path)
        .args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .map_err(|e| format!("failed to run git: {}", e))?;

    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout)
            .trim_end()
            .to_string())
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        Err(format!("git {} failed: {}", args.join(" "), stderr))
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    // -- CI Detection Tests --

    /// Global mutex to serialize tests that manipulate environment variables.
    static ENV_MUTEX: Mutex<()> = Mutex::new(());

    /// Helper to temporarily set env vars for a test, then restore originals.
    struct EnvGuard {
        vars: Vec<(String, Option<String>)>,
    }

    impl EnvGuard {
        /// Set env vars. Caller MUST hold ENV_MUTEX.
        fn set(vars: &[(&str, &str)]) -> Self {
            let mut originals = Vec::new();
            for (key, value) in vars {
                originals.push((key.to_string(), std::env::var(key).ok()));
                // SAFETY: caller holds ENV_MUTEX, ensuring exclusive access.
                unsafe { std::env::set_var(key, value) };
            }
            EnvGuard { vars: originals }
        }
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            for (key, original) in &self.vars {
                match original {
                    // SAFETY: caller holds ENV_MUTEX, ensuring exclusive access.
                    Some(val) => unsafe { std::env::set_var(key, val) },
                    None => unsafe { std::env::remove_var(key) },
                }
            }
        }
    }

    #[test]
    fn test_detect_ci_not_in_ci() {
        let _lock = ENV_MUTEX.lock().unwrap();
        // Make sure relevant env vars are unset
        let _guard = EnvGuard::set(&[]);
        // SAFETY: we hold ENV_MUTEX.
        unsafe {
            std::env::remove_var("GITHUB_ACTIONS");
            std::env::remove_var("GITLAB_CI");
        }
        assert!(detect_ci().is_none());
    }

    #[test]
    fn test_detect_github_actions() {
        let _lock = ENV_MUTEX.lock().unwrap();
        let _guard = EnvGuard::set(&[
            ("GITHUB_ACTIONS", "true"),
            ("GITHUB_REPOSITORY", "octocat/hello-world"),
            ("GITHUB_SHA", "abc123def456"),
            ("GITHUB_REF", "refs/pull/42/merge"),
            ("GITHUB_BASE_REF", "main"),
            ("GITHUB_HEAD_REF", "feature-branch"),
        ]);

        let ctx = detect_ci().expect("should detect GitHub Actions");
        assert_eq!(ctx.provider, CiProvider::GitHubActions);
        assert_eq!(ctx.repo_owner, "octocat");
        assert_eq!(ctx.repo_name, "hello-world");
        assert_eq!(ctx.pr_number, Some(42));
        assert_eq!(ctx.commit_sha, "abc123def456");
        assert_eq!(ctx.base_ref.as_deref(), Some("main"));
        assert_eq!(ctx.head_ref.as_deref(), Some("feature-branch"));
    }

    #[test]
    fn test_detect_gitlab_ci() {
        let _lock = ENV_MUTEX.lock().unwrap();
        // Clear GitHub vars to prevent interference
        // SAFETY: we hold ENV_MUTEX.
        unsafe { std::env::remove_var("GITHUB_ACTIONS") };
        let _guard = EnvGuard::set(&[
            ("GITLAB_CI", "true"),
            ("CI_PROJECT_PATH", "mygroup/myproject"),
            ("CI_COMMIT_SHA", "deadbeef1234"),
            ("CI_MERGE_REQUEST_IID", "99"),
            ("CI_MERGE_REQUEST_TARGET_BRANCH_NAME", "main"),
            ("CI_MERGE_REQUEST_SOURCE_BRANCH_NAME", "fix/thing"),
        ]);

        let ctx = detect_ci().expect("should detect GitLab CI");
        assert_eq!(ctx.provider, CiProvider::GitLabCi);
        assert_eq!(ctx.repo_owner, "mygroup");
        assert_eq!(ctx.repo_name, "myproject");
        assert_eq!(ctx.pr_number, Some(99));
        assert_eq!(ctx.commit_sha, "deadbeef1234");
        assert_eq!(ctx.base_ref.as_deref(), Some("main"));
        assert_eq!(ctx.head_ref.as_deref(), Some("fix/thing"));
    }

    #[test]
    fn test_parse_pr_number_from_ref() {
        assert_eq!(parse_pr_number_from_ref("refs/pull/123/merge"), Some(123));
        assert_eq!(parse_pr_number_from_ref("refs/pull/1/head"), Some(1));
        assert_eq!(parse_pr_number_from_ref("refs/heads/main"), None);
        assert_eq!(parse_pr_number_from_ref(""), None);
    }

    #[test]
    fn test_split_owner_repo() {
        assert_eq!(
            split_owner_repo("octocat/hello-world"),
            ("octocat".to_string(), "hello-world".to_string())
        );
        assert_eq!(
            split_owner_repo("group/subgroup/project"),
            ("group".to_string(), "subgroup/project".to_string())
        );
        assert_eq!(
            split_owner_repo("standalone"),
            (String::new(), "standalone".to_string())
        );
    }

    // -- Diff Parsing Tests --

    #[test]
    fn test_parse_diff_added_lines() {
        let diff = "\
diff --git a/src/main.rs b/src/main.rs
index abc..def 100644
--- a/src/main.rs
+++ b/src/main.rs
@@ -1,3 +1,5 @@
+new line 1
 existing
+new line 3
 existing
 existing
diff --git a/src/lib.rs b/src/lib.rs
new file mode 100644
--- /dev/null
+++ b/src/lib.rs
@@ -0,0 +1,3 @@
+line 1
+line 2
+line 3
";
        let result = parse_diff_added_lines(diff);
        assert_eq!(result.get("src/main.rs"), Some(&vec![1, 2, 3, 4, 5]));
        assert_eq!(result.get("src/lib.rs"), Some(&vec![1, 2, 3]));
    }

    #[test]
    fn test_parse_hunk_header_new_side() {
        assert_eq!(parse_hunk_header_new_side("@@ -1,3 +1,5 @@"), Some((1, 5)));
        assert_eq!(parse_hunk_header_new_side("@@ -0,0 +1,3 @@"), Some((1, 3)));
        assert_eq!(
            parse_hunk_header_new_side("@@ -10,2 +15 @@ context"),
            Some((15, 1))
        );
        assert_eq!(parse_hunk_header_new_side("not a hunk"), None);
    }

    // -- Markdown Formatting Tests --

    #[test]
    fn test_format_markdown_basic() {
        let report = AttributionReport {
            total_lines: 120,
            ai_lines: 42,
            human_lines: 65,
            untracked_lines: 13,
            files: vec![
                FileAttribution {
                    path: "src/main.rs".to_string(),
                    ai_lines: 20,
                    human_lines: 30,
                    untracked_lines: 5,
                },
                FileAttribution {
                    path: "src/lib.rs".to_string(),
                    ai_lines: 22,
                    human_lines: 35,
                    untracked_lines: 8,
                },
            ],
        };

        let md = format_markdown(&report);
        assert!(md.contains("## git-ai Attribution Report"));
        assert!(md.contains("| AI-authored | 42 | 35% |"));
        assert!(md.contains("| Human-authored | 65 | 54% |"));
        assert!(md.contains("| Untracked | 13 | 11% |"));
        assert!(md.contains("| src/main.rs | 20 | 30 | 5 |"));
        assert!(md.contains("| src/lib.rs | 22 | 35 | 8 |"));
    }

    #[test]
    fn test_format_markdown_empty() {
        let report = AttributionReport {
            total_lines: 0,
            ai_lines: 0,
            human_lines: 0,
            untracked_lines: 0,
            files: Vec::new(),
        };

        let md = format_markdown(&report);
        assert!(md.contains("## git-ai Attribution Report"));
        assert!(md.contains("| AI-authored | 0 | 0% |"));
        // No per-file section for empty reports
        assert!(!md.contains("### Per-file breakdown"));
    }

    #[test]
    fn test_percentage() {
        assert_eq!(percentage(0, 0), 0);
        assert_eq!(percentage(50, 100), 50);
        assert_eq!(percentage(1, 3), 33);
        assert_eq!(percentage(2, 3), 67);
        assert_eq!(percentage(100, 100), 100);
    }

    // -- Report Struct Construction Test --

    #[test]
    fn test_attribution_report_aggregation() {
        let files = vec![
            FileAttribution {
                path: "a.rs".to_string(),
                ai_lines: 10,
                human_lines: 5,
                untracked_lines: 2,
            },
            FileAttribution {
                path: "b.rs".to_string(),
                ai_lines: 3,
                human_lines: 7,
                untracked_lines: 1,
            },
        ];

        let total_ai: usize = files.iter().map(|f| f.ai_lines).sum();
        let total_human: usize = files.iter().map(|f| f.human_lines).sum();
        let total_untracked: usize = files.iter().map(|f| f.untracked_lines).sum();
        let total = total_ai + total_human + total_untracked;

        let report = AttributionReport {
            total_lines: total,
            ai_lines: total_ai,
            human_lines: total_human,
            untracked_lines: total_untracked,
            files,
        };

        assert_eq!(report.total_lines, 28);
        assert_eq!(report.ai_lines, 13);
        assert_eq!(report.human_lines, 12);
        assert_eq!(report.untracked_lines, 3);
    }
}
