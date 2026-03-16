use crate::daemon::analyzers::{AnalysisView, CommandAnalyzer};
use crate::daemon::domain::{
    AnalysisResult, CommandClass, Confidence, NormalizedCommand, ResetKind, SemanticEvent,
};
use crate::error::GitAiError;

#[derive(Default)]
pub struct HistoryAnalyzer;

impl CommandAnalyzer for HistoryAnalyzer {
    fn analyze(
        &self,
        cmd: &NormalizedCommand,
        _state: AnalysisView<'_>,
    ) -> Result<AnalysisResult, GitAiError> {
        let name = cmd.primary_command.as_deref().unwrap_or_default();
        let args = normalized_args(&cmd.raw_argv);

        let mut events = Vec::new();
        match name {
            "commit" => {
                if let Some(change) = cmd.ref_changes.first() {
                    if args.iter().any(|arg| arg == "--amend") {
                        events.push(SemanticEvent::CommitAmended {
                            old_head: change.old.clone(),
                            new_head: change.new.clone(),
                        });
                    } else {
                        events.push(SemanticEvent::CommitCreated {
                            base: non_empty(change.old.clone()),
                            new_head: change.new.clone(),
                        });
                    }
                }
            }
            "reset" => {
                if let Some(change) = cmd.ref_changes.first() {
                    events.push(SemanticEvent::Reset {
                        kind: infer_reset_kind(&args),
                        old_head: change.old.clone(),
                        new_head: change.new.clone(),
                    });
                }
            }
            "rebase" => {
                if args.iter().any(|arg| arg == "--abort") {
                    events.push(SemanticEvent::RebaseAbort {
                        head: cmd
                            .post_repo
                            .as_ref()
                            .and_then(|repo| repo.head.clone())
                            .unwrap_or_default(),
                    });
                } else if let Some(change) = cmd.ref_changes.first() {
                    events.push(SemanticEvent::RebaseComplete {
                        old_head: change.old.clone(),
                        new_head: change.new.clone(),
                        interactive: args.iter().any(|arg| arg == "-i" || arg == "--interactive"),
                    });
                }
            }
            "cherry-pick" => {
                if args.iter().any(|arg| arg == "--abort") {
                    events.push(SemanticEvent::CherryPickAbort {
                        head: cmd
                            .post_repo
                            .as_ref()
                            .and_then(|repo| repo.head.clone())
                            .unwrap_or_default(),
                    });
                } else if let Some(change) = cmd.ref_changes.first() {
                    events.push(SemanticEvent::CherryPickComplete {
                        original_head: change.old.clone(),
                        new_head: change.new.clone(),
                    });
                }
            }
            "merge" => {
                if args.iter().any(|arg| arg == "--squash") {
                    events.push(SemanticEvent::MergeSquash {
                        base_branch: cmd.pre_repo.as_ref().and_then(|repo| repo.branch.clone()),
                        base_head: cmd
                            .pre_repo
                            .as_ref()
                            .and_then(|repo| repo.head.clone())
                            .unwrap_or_default(),
                        source: args.last().cloned().unwrap_or_default(),
                    });
                } else if let Some(change) = cmd.ref_changes.first() {
                    events.push(SemanticEvent::RefUpdated {
                        reference: change.reference.clone(),
                        old: change.old.clone(),
                        new: change.new.clone(),
                    });
                }
            }
            _ => {
                return Err(GitAiError::Generic(format!(
                    "history analyzer does not support command '{}'",
                    name
                )));
            }
        }

        if events.is_empty() {
            events.push(SemanticEvent::OpaqueCommand);
        }

        Ok(AnalysisResult {
            class: CommandClass::HistoryRewrite,
            events,
            confidence: if cmd.exit_code == 0 {
                Confidence::High
            } else {
                Confidence::Low
            },
        })
    }
}

fn normalized_args(argv: &[String]) -> Vec<String> {
    if argv.first().map(|a| a == "git").unwrap_or(false) {
        argv[1..].to_vec()
    } else {
        argv.to_vec()
    }
}

fn non_empty(value: String) -> Option<String> {
    if value.trim().is_empty() {
        None
    } else {
        Some(value)
    }
}

fn infer_reset_kind(args: &[String]) -> ResetKind {
    if args.iter().any(|arg| arg == "--soft") {
        return ResetKind::Soft;
    }
    if args.iter().any(|arg| arg == "--mixed") {
        return ResetKind::Mixed;
    }
    if args.iter().any(|arg| arg == "--hard") {
        return ResetKind::Hard;
    }
    if args.iter().any(|arg| arg == "--merge") {
        return ResetKind::Merge;
    }
    if args.iter().any(|arg| arg == "--keep") {
        return ResetKind::Keep;
    }
    ResetKind::Mixed
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::daemon::domain::{AliasResolution, CommandScope, RefChange};

    fn command(primary: &str, argv: &[&str]) -> NormalizedCommand {
        NormalizedCommand {
            scope: CommandScope::Global,
            family_key: None,
            worktree: None,
            root_sid: "r".to_string(),
            raw_argv: argv.iter().map(|s| s.to_string()).collect(),
            primary_command: Some(primary.to_string()),
            alias_resolution: AliasResolution::None,
            observed_child_commands: Vec::new(),
            exit_code: 0,
            started_at_ns: 1,
            finished_at_ns: 2,
            pre_repo: None,
            post_repo: None,
            ref_changes: vec![RefChange {
                reference: "HEAD".to_string(),
                old: "a".to_string(),
                new: "b".to_string(),
            }],
            confidence: Confidence::Low,
            wrapper_mirror: false,
        }
    }

    #[test]
    fn commit_without_amend_emits_commit_created() {
        let analyzer = HistoryAnalyzer;
        let result = analyzer
            .analyze(
                &command("commit", &["git", "commit", "-m", "x"]),
                AnalysisView { refs: &Default::default() },
            )
            .unwrap();
        assert!(result
            .events
            .iter()
            .any(|event| matches!(event, SemanticEvent::CommitCreated { .. })));
    }

    #[test]
    fn reset_emits_reset_kind() {
        let analyzer = HistoryAnalyzer;
        let result = analyzer
            .analyze(
                &command("reset", &["git", "reset", "--hard", "HEAD~1"]),
                AnalysisView { refs: &Default::default() },
            )
            .unwrap();
        assert!(result.events.iter().any(|event| matches!(
            event,
            SemanticEvent::Reset {
                kind: ResetKind::Hard,
                ..
            }
        )));
    }
}
