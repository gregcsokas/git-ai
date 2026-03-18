use crate::daemon::domain::NormalizedCommand;
use crate::git::rewrite_log::RewriteLogEvent;

pub(crate) fn fallback_commit_rewrite_event(cmd: &NormalizedCommand) -> Option<RewriteLogEvent> {
    if cmd.exit_code != 0 {
        return None;
    }
    let command = cmd
        .primary_command
        .as_deref()
        .or(cmd.invoked_command.as_deref())?;
    if command != "commit" {
        return None;
    }

    let new_head = cmd
        .post_repo
        .as_ref()
        .and_then(|repo| repo.head.clone())
        .or_else(|| {
            cmd.ref_changes
                .iter()
                .rfind(|change| change.reference == "HEAD")
                .map(|change| change.new.clone())
        })
        .filter(|sha| is_valid_oid(sha) && !is_zero_oid(sha))?;
    if cmd.invoked_args.iter().any(|arg| arg == "--amend") {
        let old_head = cmd
            .pre_repo
            .as_ref()
            .and_then(|repo| repo.head.clone())
            .or_else(|| {
                cmd.ref_changes
                    .iter()
                    .find(|change| change.reference == "HEAD")
                    .map(|change| change.old.clone())
            })
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
        .or_else(|| {
            cmd.ref_changes
                .iter()
                .find(|change| change.reference == "HEAD")
                .map(|change| change.old.clone())
        })
        .filter(|sha| is_valid_oid(sha) && !is_zero_oid(sha) && sha != &new_head);

    // Root commits on fresh branches do not have a parent commit.
    // Preserve the rewrite event with `base_commit = None` so replay treats
    // the commit as based on `initial`.
    Some(RewriteLogEvent::commit(base, new_head))
}

fn is_valid_oid(oid: &str) -> bool {
    matches!(oid.len(), 40 | 64) && oid.chars().all(|c| c.is_ascii_hexdigit())
}

fn is_zero_oid(oid: &str) -> bool {
    is_valid_oid(oid) && oid.chars().all(|c| c == '0')
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::daemon::domain::{
        CommandScope, Confidence, FamilyKey, NormalizedCommand, RepoContext,
    };

    #[test]
    fn fallback_prefers_primary_commit_over_invoked_alias() {
        let base = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".to_string();
        let head = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb".to_string();

        let cmd = NormalizedCommand {
            scope: CommandScope::Family(FamilyKey::new("family:/repo")),
            family_key: Some(FamilyKey::new("family:/repo")),
            worktree: Some(std::path::PathBuf::from("/repo")),
            root_sid: "sid".to_string(),
            raw_argv: vec![
                "git".to_string(),
                "ci".to_string(),
                "-m".to_string(),
                "next".to_string(),
            ],
            primary_command: Some("commit".to_string()),
            invoked_command: Some("ci".to_string()),
            invoked_args: vec!["-m".to_string(), "next".to_string()],
            observed_child_commands: Vec::new(),
            exit_code: 0,
            started_at_ns: 1,
            finished_at_ns: 2,
            pre_repo: Some(RepoContext {
                head: Some(base.clone()),
                branch: Some("main".to_string()),
                detached: false,
                cherry_pick_head: None,
            }),
            post_repo: Some(RepoContext {
                head: Some(head.clone()),
                branch: Some("main".to_string()),
                detached: false,
                cherry_pick_head: None,
            }),
            ref_changes: Vec::new(),
            confidence: Confidence::Low,
            wrapper_mirror: false,
        };

        let event = fallback_commit_rewrite_event(&cmd).expect("fallback commit event");
        match event {
            RewriteLogEvent::Commit { commit } => {
                assert_eq!(commit.commit_sha, head);
                assert_eq!(commit.base_commit, Some(base));
            }
            other => panic!("expected commit event, got {:?}", other),
        }
    }
}
