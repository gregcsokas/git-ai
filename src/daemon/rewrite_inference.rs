use crate::daemon::domain::NormalizedCommand;
use crate::error::GitAiError;
use crate::git::rewrite_log::RewriteLogEvent;

pub(crate) fn fallback_commit_rewrite_event(cmd: &NormalizedCommand) -> Option<RewriteLogEvent> {
    if cmd.exit_code != 0 {
        return None;
    }
    let worktree = cmd.worktree.as_ref()?.to_string_lossy().to_string();
    let command = cmd
        .invoked_command
        .as_deref()
        .or(cmd.primary_command.as_deref())?;
    if command != "commit" {
        return None;
    }

    let new_head = run_git_capture(&worktree, &["rev-parse", "HEAD"])
        .ok()
        .filter(|sha| is_valid_oid(sha) && !is_zero_oid(sha))?;
    if cmd.invoked_args.iter().any(|arg| arg == "--amend") {
        let old_head = run_git_capture(&worktree, &["rev-parse", "HEAD@{1}"])
            .ok()
            .filter(|sha| is_valid_oid(sha) && !is_zero_oid(sha));
        if let Some(old_head) = old_head
            && old_head != new_head
        {
            return Some(RewriteLogEvent::commit_amend(old_head, new_head));
        }
        return None;
    }

    let base = cmd
        .pre_repo
        .as_ref()
        .and_then(|repo| repo.head.clone())
        .filter(|sha| is_valid_oid(sha) && !is_zero_oid(sha) && sha != &new_head)
        .or_else(|| {
            run_git_capture(&worktree, &["rev-parse", "HEAD@{1}"])
                .ok()
                .filter(|sha| is_valid_oid(sha) && !is_zero_oid(sha) && sha != &new_head)
        })
        .or_else(|| {
            run_git_capture(&worktree, &["rev-parse", "HEAD^"])
                .ok()
                .filter(|sha| is_valid_oid(sha) && !is_zero_oid(sha) && sha != &new_head)
        });

    // Root commits on fresh branches can lack both `HEAD@{1}` and `HEAD^`.
    // Preserve the rewrite event with `base_commit = None` so replay treats
    // the commit as based on `initial`.
    Some(RewriteLogEvent::commit(base, new_head))
}

fn run_git_capture(worktree: &str, args: &[&str]) -> Result<String, GitAiError> {
    let mut command = std::process::Command::new("/opt/homebrew/bin/git");
    command.arg("-C").arg(worktree);
    command.args(args);
    let output = command.output()?;
    if !output.status.success() {
        return Err(GitAiError::Generic(format!(
            "git {:?} failed in {}: {}",
            args,
            worktree,
            String::from_utf8_lossy(&output.stderr)
        )));
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

fn is_valid_oid(oid: &str) -> bool {
    matches!(oid.len(), 40 | 64) && oid.chars().all(|c| c.is_ascii_hexdigit())
}

fn is_zero_oid(oid: &str) -> bool {
    is_valid_oid(oid) && oid.chars().all(|c| c == '0')
}
