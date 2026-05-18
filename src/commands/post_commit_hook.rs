//! `git-ai post-commit-hook` — entry point for a user-managed
//! `.git/hooks/post-commit` script that displays the AI-vs-human authorship
//! graph after a commit.
//!
//! The hook itself is strictly a reader. All authorship-note writes are
//! performed by the daemon (via either its trace2 path or the
//! `commit.ensure_processed` control RPC sent from this command). This keeps
//! the daemon as the single writer and guarantees no double-processing.
//!
//! Flow:
//!   1. Resolve HEAD → commit SHA.
//!   2. Apply skip rules (quiet flags, non-TTY, config).
//!   3. Best-effort: send a `commit.ensure_processed` RPC to the daemon. If
//!      the daemon is up, this acts as a safety net for dropped trace2 events
//!      and also ensures the note exists before we poll.
//!   4. Poll `refs/notes/ai/<sha>` (same loop as the wrapper used).
//!   5. If found and not a merge/expensive commit, compute stats + render.
//!   6. On timeout / daemon-down / errors, print the same friendly
//!      "still processing" message and exit 0. **Never** fail the commit.

use crate::authorship::ignore::effective_ignore_patterns;
use crate::authorship::post_commit::estimate_stats_cost_for_head;
use crate::authorship::stats::{stats_for_commit_stats, write_stats_to_terminal};
use crate::config;
use crate::daemon::{ControlRequest, DaemonConfig, send_control_request};
use crate::git::find_repository;
use crate::git::notes_api::read_note;
use crate::git::repository::Repository;
use std::io::IsTerminal;
use std::time::{Duration, Instant};

pub fn handle_post_commit_hook(args: &[String]) {
    // Post-commit hooks must never fail the commit. Every failure path here
    // exits with status 0.
    let exit_code = match run(args) {
        Ok(()) => 0,
        Err(reason) => {
            // Quiet: only log internally; don't bother the user.
            tracing::debug!("post-commit-hook skipped: {}", reason);
            0
        }
    };
    std::process::exit(exit_code);
}

fn run(args: &[String]) -> Result<(), String> {
    // Suppression flags: take both git-style flags (--quiet/-q) and the env
    // var the wrapper used. Real `.git/hooks/post-commit` invocations receive
    // no arguments, so this is mostly for explicit user wiring and tests.
    let quiet_flag = args.iter().any(|a| a == "--quiet" || a == "-q");
    if quiet_flag {
        return Err("--quiet".to_string());
    }
    if std::env::var_os("GIT_AI_POST_COMMIT_HOOK_SKIP").is_some() {
        return Err("GIT_AI_POST_COMMIT_HOOK_SKIP".to_string());
    }
    if config::Config::get().is_quiet() {
        return Err("config quiet".to_string());
    }

    let is_interactive =
        std::io::stdout().is_terminal() || std::env::var_os("GIT_AI_TEST_FORCE_TTY").is_some();
    if !is_interactive {
        return Err("non-interactive stdout".to_string());
    }

    let repo = find_repository(&Vec::<String>::new())
        .map_err(|e| format!("repository not found: {}", e))?;

    let commit_sha = repo
        .head()
        .map_err(|e| format!("head: {}", e))?
        .target()
        .map_err(|e| format!("head target: {}", e))?;

    // Best-effort: prod the daemon to produce the note. Failures here are
    // expected (daemon down, slow response, etc.) and we fall through to a
    // plain poll either way.
    let _ = ensure_processed_via_daemon(&repo, &commit_sha);

    let timeout = poll_timeout();
    let poll_interval = Duration::from_millis(25);
    let start = Instant::now();
    let note_found = loop {
        if read_note(&repo, &commit_sha).is_some() {
            break true;
        }
        if start.elapsed() >= timeout {
            break false;
        }
        std::thread::sleep(poll_interval);
    };

    if !note_found {
        eprintln!(
            "[git-ai] still processing commit {}... run `git ai stats` to see stats.",
            short_sha(&commit_sha)
        );
        return Ok(());
    }

    // Skip stats output for merge commits — matches the wrapper path.
    let is_merge = repo
        .find_commit(commit_sha.clone())
        .map(|c| c.parent_count().unwrap_or(0) > 1)
        .unwrap_or(false);
    if is_merge {
        eprintln!(
            "[git-ai] Skipped git-ai stats for merge commit {}.",
            commit_sha
        );
        return Ok(());
    }

    let ignore_patterns = effective_ignore_patterns(&repo, &[], &[]);
    if let Ok(estimate) = estimate_stats_cost_for_head(&repo, &commit_sha, &ignore_patterns)
        && estimate.should_skip()
    {
        eprintln!(
            "[git-ai] Skipped git-ai stats for large commit. Run `git ai stats {}` to compute stats on demand.",
            commit_sha
        );
        return Ok(());
    }

    match stats_for_commit_stats(&repo, &commit_sha, &ignore_patterns) {
        Ok(stats) => {
            write_stats_to_terminal(&stats, true);
            Ok(())
        }
        Err(e) => Err(format!("stats_for_commit_stats: {}", e)),
    }
}

fn ensure_processed_via_daemon(repo: &Repository, commit_sha: &str) -> Result<(), String> {
    let worktree = repo
        .workdir()
        .map_err(|e| format!("workdir: {}", e))?
        .to_string_lossy()
        .to_string();

    let daemon_config =
        DaemonConfig::from_env_or_default_paths().map_err(|e| format!("daemon config: {}", e))?;
    if !daemon_config.control_socket_path.exists() {
        return Err("daemon socket missing".to_string());
    }

    let request = ControlRequest::CommitEnsureProcessed {
        repo_working_dir: worktree,
        commit_sha: commit_sha.to_string(),
    };
    // `send_control_request` derives its read timeout from
    // `control_request_response_timeout`, which handles `CommitEnsureProcessed`
    // with the longer checkpoint budget under CI/test.
    send_control_request(&daemon_config.control_socket_path, &request)
        .map(|_| ())
        .map_err(|e| format!("send_control_request: {}", e))
}

fn poll_timeout() -> Duration {
    if let Some(ms) = std::env::var("GIT_AI_POST_COMMIT_TIMEOUT_MS")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
    {
        Duration::from_millis(ms)
    } else if std::env::var_os("GIT_AI_TEST_DB_PATH").is_some() {
        Duration::from_secs(20)
    } else {
        // After the RPC returns, the note should already be present. Keep the
        // poll fast for the rare cases where it isn't (daemon down, dropped
        // trace2 event, etc.).
        Duration::from_millis(500)
    }
}

fn short_sha(sha: &str) -> &str {
    &sha[..std::cmp::min(8, sha.len())]
}
