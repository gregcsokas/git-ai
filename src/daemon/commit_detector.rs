use std::collections::HashMap;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use crate::daemon::trace2_events::{Trace2Event, is_root_sid, root_sid};

/// A detected git operation ready for processing.
#[derive(Debug, Clone)]
pub enum DetectedOperation {
    Commit {
        repo_path: PathBuf,
    },
    Rewrite {
        repo_path: PathBuf,
        kind: RewriteKind,
        argv: Vec<String>,
    },
    Stash {
        repo_path: PathBuf,
        argv: Vec<String>,
    },
    StashPop {
        repo_path: PathBuf,
    },
}

#[derive(Debug, Clone, PartialEq)]
pub enum RewriteKind {
    Rebase,
    CherryPick,
    Amend,
    Reset,
}

/// A detected commit ready for authorship processing.
#[derive(Debug, Clone)]
pub struct DetectedCommit {
    pub repo_path: PathBuf,
}

/// Internal state for a single trace2 session.
struct SessionState {
    cmd_name: Option<String>,
    repo_path: Option<PathBuf>,
    #[allow(dead_code)]
    argv: Vec<String>,
    created_at: Instant,
}

/// Accumulates trace2 events and detects completed git commits.
#[derive(Default)]
pub struct CommitDetector {
    /// Active sessions: root sid -> accumulated state
    sessions: HashMap<String, SessionState>,
}

impl CommitDetector {
    pub fn new() -> Self {
        Self::default()
    }

    /// Feed an event. Returns Some(DetectedCommit) if a successful commit was detected.
    pub fn process_event(&mut self, event: Trace2Event) -> Option<DetectedCommit> {
        match event {
            Trace2Event::Start { sid, argv } => {
                if !is_root_sid(&sid) {
                    return None;
                }
                // Infer cmd_name from argv if possible (argv[0] is "git", argv[1] is the subcommand)
                let inferred_cmd = argv.get(1).cloned();
                let session = self.sessions.entry(sid).or_insert_with(|| SessionState {
                    cmd_name: None,
                    repo_path: None,
                    argv: Vec::new(),
                    created_at: Instant::now(),
                });
                session.argv = argv;
                if session.cmd_name.is_none() {
                    session.cmd_name = inferred_cmd;
                }
                None
            }
            Trace2Event::CmdName { sid, cmd_name } => {
                // Only accept cmd_name from root-level processes. Child processes
                // (e.g. git maintenance) have their own cmd_name that must not
                // overwrite the parent session's command.
                if !is_root_sid(&sid) {
                    return None;
                }
                let session = self.sessions.entry(sid).or_insert_with(|| SessionState {
                    cmd_name: None,
                    repo_path: None,
                    argv: Vec::new(),
                    created_at: Instant::now(),
                });
                session.cmd_name = Some(cmd_name);
                None
            }
            Trace2Event::DefRepo { sid, repo_path } => {
                let root = root_sid(&sid).to_string();
                let session = self.sessions.entry(root).or_insert_with(|| SessionState {
                    cmd_name: None,
                    repo_path: None,
                    argv: Vec::new(),
                    created_at: Instant::now(),
                });
                session.repo_path = Some(repo_path);
                None
            }
            Trace2Event::CommandExit { sid, exit_code } => {
                if !is_root_sid(&sid) {
                    return None;
                }
                let result = if exit_code == 0 {
                    if let Some(session) = self.sessions.get(&sid) {
                        let is_commit = session
                            .cmd_name
                            .as_deref()
                            .is_some_and(|name| name == "commit");
                        if is_commit {
                            session.repo_path.as_ref().map(|path| DetectedCommit {
                                repo_path: path.clone(),
                            })
                        } else {
                            None
                        }
                    } else {
                        None
                    }
                } else {
                    None
                };
                // Clean up the session regardless of outcome
                self.sessions.remove(&sid);
                result
            }
            Trace2Event::Ignored => None,
        }
    }

    /// Feed an event. Returns a detected operation (commit or rewrite) if applicable.
    pub fn process_event_full(&mut self, event: Trace2Event) -> Option<DetectedOperation> {
        match event {
            Trace2Event::Start { sid, argv } => {
                if !is_root_sid(&sid) {
                    return None;
                }
                let inferred_cmd = argv.get(1).cloned();
                let session = self.sessions.entry(sid).or_insert_with(|| SessionState {
                    cmd_name: None,
                    repo_path: None,
                    argv: Vec::new(),
                    created_at: Instant::now(),
                });
                session.argv = argv;
                if session.cmd_name.is_none() {
                    session.cmd_name = inferred_cmd;
                }
                None
            }
            Trace2Event::CmdName { sid, cmd_name } => {
                if !is_root_sid(&sid) {
                    return None;
                }
                let session = self.sessions.entry(sid).or_insert_with(|| SessionState {
                    cmd_name: None,
                    repo_path: None,
                    argv: Vec::new(),
                    created_at: Instant::now(),
                });
                session.cmd_name = Some(cmd_name);
                None
            }
            Trace2Event::DefRepo { sid, repo_path } => {
                let root = root_sid(&sid).to_string();
                let session = self.sessions.entry(root).or_insert_with(|| SessionState {
                    cmd_name: None,
                    repo_path: None,
                    argv: Vec::new(),
                    created_at: Instant::now(),
                });
                session.repo_path = Some(repo_path);
                None
            }
            Trace2Event::CommandExit { sid, exit_code } => {
                if !is_root_sid(&sid) {
                    return None;
                }
                let result =
                    if exit_code == 0 {
                        if let Some(session) = self.sessions.get(&sid) {
                            match session.cmd_name.as_deref() {
                                Some("commit") => {
                                    // Check if this is an amend
                                    let is_amend = session.argv.iter().any(|a| a == "--amend");
                                    if is_amend {
                                        session.repo_path.as_ref().map(|path| {
                                            DetectedOperation::Rewrite {
                                                repo_path: path.clone(),
                                                kind: RewriteKind::Amend,
                                                argv: session.argv.clone(),
                                            }
                                        })
                                    } else {
                                        session.repo_path.as_ref().map(|path| {
                                            DetectedOperation::Commit {
                                                repo_path: path.clone(),
                                            }
                                        })
                                    }
                                }
                                Some("rebase") => session.repo_path.as_ref().map(|path| {
                                    DetectedOperation::Rewrite {
                                        repo_path: path.clone(),
                                        kind: RewriteKind::Rebase,
                                        argv: session.argv.clone(),
                                    }
                                }),
                                Some("cherry-pick") => session.repo_path.as_ref().map(|path| {
                                    DetectedOperation::Rewrite {
                                        repo_path: path.clone(),
                                        kind: RewriteKind::CherryPick,
                                        argv: session.argv.clone(),
                                    }
                                }),
                                Some("reset") => session.repo_path.as_ref().map(|path| {
                                    DetectedOperation::Rewrite {
                                        repo_path: path.clone(),
                                        kind: RewriteKind::Reset,
                                        argv: session.argv.clone(),
                                    }
                                }),
                                Some("stash") => {
                                    let has_pop_or_apply =
                                        session.argv.iter().any(|a| a == "pop" || a == "apply");
                                    let has_push_or_save =
                                        session.argv.iter().any(|a| a == "push" || a == "save");
                                    if has_pop_or_apply {
                                        session.repo_path.as_ref().map(|path| {
                                            DetectedOperation::StashPop {
                                                repo_path: path.clone(),
                                            }
                                        })
                                    } else if has_push_or_save {
                                        session.repo_path.as_ref().map(|path| {
                                            DetectedOperation::Stash {
                                                repo_path: path.clone(),
                                                argv: session.argv.clone(),
                                            }
                                        })
                                    } else {
                                        // Bare `git stash` (no subcommand) is equivalent to push
                                        let is_bare_stash = !session.argv.iter().any(|a| {
                                            a == "list"
                                                || a == "show"
                                                || a == "drop"
                                                || a == "clear"
                                                || a == "branch"
                                                || a == "create"
                                                || a == "store"
                                        });
                                        if is_bare_stash {
                                            session.repo_path.as_ref().map(|path| {
                                                DetectedOperation::Stash {
                                                    repo_path: path.clone(),
                                                    argv: session.argv.clone(),
                                                }
                                            })
                                        } else {
                                            None
                                        }
                                    }
                                }
                                _ => None,
                            }
                        } else {
                            None
                        }
                    } else {
                        None
                    };
                self.sessions.remove(&sid);
                result
            }
            Trace2Event::Ignored => None,
        }
    }

    /// Prune stale sessions older than the given duration.
    /// Prevents memory leaks from orphaned sessions that never received an exit event.
    pub fn prune_stale(&mut self, max_age: Duration) {
        self.sessions
            .retain(|_sid, state| state.created_at.elapsed() < max_age);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detect_simple_commit() {
        let mut detector = CommitDetector::new();

        // Simulate a typical git commit trace2 sequence
        let result = detector.process_event(Trace2Event::Start {
            sid: "123-abc".to_string(),
            argv: vec![
                "git".to_string(),
                "commit".to_string(),
                "-m".to_string(),
                "test".to_string(),
            ],
        });
        assert!(result.is_none());

        let result = detector.process_event(Trace2Event::DefRepo {
            sid: "123-abc".to_string(),
            repo_path: PathBuf::from("/home/user/project"),
        });
        assert!(result.is_none());

        let result = detector.process_event(Trace2Event::CmdName {
            sid: "123-abc".to_string(),
            cmd_name: "commit".to_string(),
        });
        assert!(result.is_none());

        let result = detector.process_event(Trace2Event::CommandExit {
            sid: "123-abc".to_string(),
            exit_code: 0,
        });
        let commit = result.expect("should detect commit");
        assert_eq!(commit.repo_path, PathBuf::from("/home/user/project"));
    }

    #[test]
    fn no_detection_on_non_zero_exit() {
        let mut detector = CommitDetector::new();

        detector.process_event(Trace2Event::Start {
            sid: "123-abc".to_string(),
            argv: vec![
                "git".to_string(),
                "commit".to_string(),
                "-m".to_string(),
                "test".to_string(),
            ],
        });
        detector.process_event(Trace2Event::DefRepo {
            sid: "123-abc".to_string(),
            repo_path: PathBuf::from("/home/user/project"),
        });
        detector.process_event(Trace2Event::CmdName {
            sid: "123-abc".to_string(),
            cmd_name: "commit".to_string(),
        });

        let result = detector.process_event(Trace2Event::CommandExit {
            sid: "123-abc".to_string(),
            exit_code: 1,
        });
        assert!(result.is_none());
    }

    #[test]
    fn no_detection_for_non_commit_command() {
        let mut detector = CommitDetector::new();

        detector.process_event(Trace2Event::Start {
            sid: "123-abc".to_string(),
            argv: vec!["git".to_string(), "status".to_string()],
        });
        detector.process_event(Trace2Event::DefRepo {
            sid: "123-abc".to_string(),
            repo_path: PathBuf::from("/home/user/project"),
        });
        detector.process_event(Trace2Event::CmdName {
            sid: "123-abc".to_string(),
            cmd_name: "status".to_string(),
        });

        let result = detector.process_event(Trace2Event::CommandExit {
            sid: "123-abc".to_string(),
            exit_code: 0,
        });
        assert!(result.is_none());
    }

    #[test]
    fn ignore_child_process_events() {
        let mut detector = CommitDetector::new();

        // Parent process starts
        detector.process_event(Trace2Event::Start {
            sid: "parent-123".to_string(),
            argv: vec![
                "git".to_string(),
                "commit".to_string(),
                "-m".to_string(),
                "msg".to_string(),
            ],
        });
        detector.process_event(Trace2Event::DefRepo {
            sid: "parent-123".to_string(),
            repo_path: PathBuf::from("/repo"),
        });
        detector.process_event(Trace2Event::CmdName {
            sid: "parent-123".to_string(),
            cmd_name: "commit".to_string(),
        });

        // Child process exit should not trigger detection
        let result = detector.process_event(Trace2Event::CommandExit {
            sid: "parent-123/child-456".to_string(),
            exit_code: 0,
        });
        assert!(result.is_none());

        // Parent exit should trigger detection
        let result = detector.process_event(Trace2Event::CommandExit {
            sid: "parent-123".to_string(),
            exit_code: 0,
        });
        assert!(result.is_some());
    }

    #[test]
    fn def_repo_from_child_associates_with_root() {
        let mut detector = CommitDetector::new();

        detector.process_event(Trace2Event::Start {
            sid: "parent-123".to_string(),
            argv: vec![
                "git".to_string(),
                "commit".to_string(),
                "-m".to_string(),
                "msg".to_string(),
            ],
        });
        detector.process_event(Trace2Event::CmdName {
            sid: "parent-123".to_string(),
            cmd_name: "commit".to_string(),
        });

        // def_repo from a child process should associate with root sid
        detector.process_event(Trace2Event::DefRepo {
            sid: "parent-123/child-456".to_string(),
            repo_path: PathBuf::from("/from/child"),
        });

        let result = detector.process_event(Trace2Event::CommandExit {
            sid: "parent-123".to_string(),
            exit_code: 0,
        });
        let commit = result.expect("should detect commit using child's def_repo");
        assert_eq!(commit.repo_path, PathBuf::from("/from/child"));
    }

    #[test]
    fn cmd_name_overrides_argv_inference() {
        let mut detector = CommitDetector::new();

        // argv says "commit" but cmd_name says something else
        detector.process_event(Trace2Event::Start {
            sid: "123-abc".to_string(),
            argv: vec!["git".to_string(), "commit".to_string()],
        });
        detector.process_event(Trace2Event::DefRepo {
            sid: "123-abc".to_string(),
            repo_path: PathBuf::from("/repo"),
        });
        // cmd_name is authoritative
        detector.process_event(Trace2Event::CmdName {
            sid: "123-abc".to_string(),
            cmd_name: "status".to_string(),
        });

        let result = detector.process_event(Trace2Event::CommandExit {
            sid: "123-abc".to_string(),
            exit_code: 0,
        });
        assert!(result.is_none(), "cmd_name should override argv inference");
    }

    #[test]
    fn argv_inference_used_when_no_cmd_name_event() {
        let mut detector = CommitDetector::new();

        detector.process_event(Trace2Event::Start {
            sid: "123-abc".to_string(),
            argv: vec![
                "git".to_string(),
                "commit".to_string(),
                "-m".to_string(),
                "msg".to_string(),
            ],
        });
        detector.process_event(Trace2Event::DefRepo {
            sid: "123-abc".to_string(),
            repo_path: PathBuf::from("/repo"),
        });
        // No CmdName event arrives

        let result = detector.process_event(Trace2Event::CommandExit {
            sid: "123-abc".to_string(),
            exit_code: 0,
        });
        let commit = result.expect("should detect commit from argv inference");
        assert_eq!(commit.repo_path, PathBuf::from("/repo"));
    }

    #[test]
    fn no_detection_without_repo_path() {
        let mut detector = CommitDetector::new();

        detector.process_event(Trace2Event::Start {
            sid: "123-abc".to_string(),
            argv: vec![
                "git".to_string(),
                "commit".to_string(),
                "-m".to_string(),
                "msg".to_string(),
            ],
        });
        detector.process_event(Trace2Event::CmdName {
            sid: "123-abc".to_string(),
            cmd_name: "commit".to_string(),
        });
        // No DefRepo event

        let result = detector.process_event(Trace2Event::CommandExit {
            sid: "123-abc".to_string(),
            exit_code: 0,
        });
        assert!(
            result.is_none(),
            "should not detect commit without repo_path"
        );
    }

    #[test]
    fn session_cleaned_up_after_exit() {
        let mut detector = CommitDetector::new();

        detector.process_event(Trace2Event::Start {
            sid: "123-abc".to_string(),
            argv: vec!["git".to_string(), "commit".to_string()],
        });
        detector.process_event(Trace2Event::DefRepo {
            sid: "123-abc".to_string(),
            repo_path: PathBuf::from("/repo"),
        });
        detector.process_event(Trace2Event::CmdName {
            sid: "123-abc".to_string(),
            cmd_name: "commit".to_string(),
        });
        detector.process_event(Trace2Event::CommandExit {
            sid: "123-abc".to_string(),
            exit_code: 0,
        });

        // A second exit for the same sid should not produce anything (session already cleaned)
        let result = detector.process_event(Trace2Event::CommandExit {
            sid: "123-abc".to_string(),
            exit_code: 0,
        });
        assert!(result.is_none());
    }

    #[test]
    fn prune_stale_removes_old_sessions() {
        let mut detector = CommitDetector::new();

        detector.process_event(Trace2Event::Start {
            sid: "old-session".to_string(),
            argv: vec!["git".to_string(), "commit".to_string()],
        });

        // With a zero duration, everything is "stale"
        detector.prune_stale(Duration::from_secs(0));

        // The session should be gone, so exit produces nothing
        let result = detector.process_event(Trace2Event::CommandExit {
            sid: "old-session".to_string(),
            exit_code: 0,
        });
        assert!(result.is_none());
    }

    #[test]
    fn multiple_concurrent_sessions() {
        let mut detector = CommitDetector::new();

        // Session A: commit
        detector.process_event(Trace2Event::Start {
            sid: "session-a".to_string(),
            argv: vec![
                "git".to_string(),
                "commit".to_string(),
                "-m".to_string(),
                "a".to_string(),
            ],
        });
        // Session B: status
        detector.process_event(Trace2Event::Start {
            sid: "session-b".to_string(),
            argv: vec!["git".to_string(), "status".to_string()],
        });

        detector.process_event(Trace2Event::DefRepo {
            sid: "session-a".to_string(),
            repo_path: PathBuf::from("/repo-a"),
        });
        detector.process_event(Trace2Event::DefRepo {
            sid: "session-b".to_string(),
            repo_path: PathBuf::from("/repo-b"),
        });

        detector.process_event(Trace2Event::CmdName {
            sid: "session-a".to_string(),
            cmd_name: "commit".to_string(),
        });
        detector.process_event(Trace2Event::CmdName {
            sid: "session-b".to_string(),
            cmd_name: "status".to_string(),
        });

        // B exits first - no commit
        let result = detector.process_event(Trace2Event::CommandExit {
            sid: "session-b".to_string(),
            exit_code: 0,
        });
        assert!(result.is_none());

        // A exits - should detect commit
        let result = detector.process_event(Trace2Event::CommandExit {
            sid: "session-a".to_string(),
            exit_code: 0,
        });
        let commit = result.expect("should detect commit for session-a");
        assert_eq!(commit.repo_path, PathBuf::from("/repo-a"));
    }

    #[test]
    fn child_cmd_name_does_not_overwrite_parent() {
        let mut detector = CommitDetector::new();

        detector.process_event(Trace2Event::Start {
            sid: "parent-123".to_string(),
            argv: vec![
                "git".to_string(),
                "commit".to_string(),
                "-m".to_string(),
                "msg".to_string(),
            ],
        });
        detector.process_event(Trace2Event::CmdName {
            sid: "parent-123".to_string(),
            cmd_name: "commit".to_string(),
        });
        detector.process_event(Trace2Event::DefRepo {
            sid: "parent-123".to_string(),
            repo_path: PathBuf::from("/repo"),
        });

        // Child process (e.g. git maintenance) sends its own cmd_name
        detector.process_event(Trace2Event::CmdName {
            sid: "parent-123/child-456".to_string(),
            cmd_name: "maintenance".to_string(),
        });

        // Parent exit should still detect "commit"
        let result = detector.process_event(Trace2Event::CommandExit {
            sid: "parent-123".to_string(),
            exit_code: 0,
        });
        let commit = result.expect("child cmd_name must not corrupt parent session");
        assert_eq!(commit.repo_path, PathBuf::from("/repo"));
    }

    // -----------------------------------------------------------------------
    // process_event_full tests
    // -----------------------------------------------------------------------

    #[test]
    fn full_non_zero_exit_no_operation() {
        let mut detector = CommitDetector::new();

        detector.process_event_full(Trace2Event::Start {
            sid: "sid-fail".to_string(),
            argv: vec![
                "git".to_string(),
                "commit".to_string(),
                "-m".to_string(),
                "msg".to_string(),
            ],
        });
        detector.process_event_full(Trace2Event::DefRepo {
            sid: "sid-fail".to_string(),
            repo_path: PathBuf::from("/repo"),
        });
        detector.process_event_full(Trace2Event::CmdName {
            sid: "sid-fail".to_string(),
            cmd_name: "commit".to_string(),
        });

        let result = detector.process_event_full(Trace2Event::CommandExit {
            sid: "sid-fail".to_string(),
            exit_code: 128,
        });
        assert!(
            result.is_none(),
            "non-zero exit should not emit a DetectedOperation"
        );
    }

    #[test]
    fn full_rebase_emits_rewrite_rebase() {
        let mut detector = CommitDetector::new();

        detector.process_event_full(Trace2Event::Start {
            sid: "sid-rebase".to_string(),
            argv: vec![
                "git".to_string(),
                "rebase".to_string(),
                "master".to_string(),
            ],
        });
        detector.process_event_full(Trace2Event::DefRepo {
            sid: "sid-rebase".to_string(),
            repo_path: PathBuf::from("/repo"),
        });
        detector.process_event_full(Trace2Event::CmdName {
            sid: "sid-rebase".to_string(),
            cmd_name: "rebase".to_string(),
        });

        let result = detector.process_event_full(Trace2Event::CommandExit {
            sid: "sid-rebase".to_string(),
            exit_code: 0,
        });
        match result {
            Some(DetectedOperation::Rewrite {
                repo_path,
                kind,
                argv,
            }) => {
                assert_eq!(repo_path, PathBuf::from("/repo"));
                assert_eq!(kind, RewriteKind::Rebase);
                assert_eq!(argv, vec!["git", "rebase", "master"]);
            }
            other => panic!("expected Rewrite/Rebase, got {:?}", other),
        }
    }

    #[test]
    fn full_amend_emits_rewrite_amend() {
        let mut detector = CommitDetector::new();

        detector.process_event_full(Trace2Event::Start {
            sid: "sid-amend".to_string(),
            argv: vec![
                "git".to_string(),
                "commit".to_string(),
                "--amend".to_string(),
                "-m".to_string(),
                "fix".to_string(),
            ],
        });
        detector.process_event_full(Trace2Event::DefRepo {
            sid: "sid-amend".to_string(),
            repo_path: PathBuf::from("/repo"),
        });
        detector.process_event_full(Trace2Event::CmdName {
            sid: "sid-amend".to_string(),
            cmd_name: "commit".to_string(),
        });

        let result = detector.process_event_full(Trace2Event::CommandExit {
            sid: "sid-amend".to_string(),
            exit_code: 0,
        });
        match result {
            Some(DetectedOperation::Rewrite {
                repo_path, kind, ..
            }) => {
                assert_eq!(repo_path, PathBuf::from("/repo"));
                assert_eq!(kind, RewriteKind::Amend);
            }
            other => panic!("expected Rewrite/Amend, got {:?}", other),
        }
    }

    #[test]
    fn full_stash_push_emits_stash() {
        let mut detector = CommitDetector::new();

        detector.process_event_full(Trace2Event::Start {
            sid: "sid-stash-push".to_string(),
            argv: vec![
                "git".to_string(),
                "stash".to_string(),
                "push".to_string(),
                "-m".to_string(),
                "wip".to_string(),
            ],
        });
        detector.process_event_full(Trace2Event::DefRepo {
            sid: "sid-stash-push".to_string(),
            repo_path: PathBuf::from("/repo"),
        });
        detector.process_event_full(Trace2Event::CmdName {
            sid: "sid-stash-push".to_string(),
            cmd_name: "stash".to_string(),
        });

        let result = detector.process_event_full(Trace2Event::CommandExit {
            sid: "sid-stash-push".to_string(),
            exit_code: 0,
        });
        match result {
            Some(DetectedOperation::Stash { repo_path, argv }) => {
                assert_eq!(repo_path, PathBuf::from("/repo"));
                assert!(argv.contains(&"push".to_string()));
            }
            other => panic!("expected Stash, got {:?}", other),
        }
    }

    #[test]
    fn full_stash_pop_emits_stash_pop() {
        let mut detector = CommitDetector::new();

        detector.process_event_full(Trace2Event::Start {
            sid: "sid-stash-pop".to_string(),
            argv: vec!["git".to_string(), "stash".to_string(), "pop".to_string()],
        });
        detector.process_event_full(Trace2Event::DefRepo {
            sid: "sid-stash-pop".to_string(),
            repo_path: PathBuf::from("/repo"),
        });
        detector.process_event_full(Trace2Event::CmdName {
            sid: "sid-stash-pop".to_string(),
            cmd_name: "stash".to_string(),
        });

        let result = detector.process_event_full(Trace2Event::CommandExit {
            sid: "sid-stash-pop".to_string(),
            exit_code: 0,
        });
        match result {
            Some(DetectedOperation::StashPop { repo_path }) => {
                assert_eq!(repo_path, PathBuf::from("/repo"));
            }
            other => panic!("expected StashPop, got {:?}", other),
        }
    }

    #[test]
    fn full_prune_stale_removes_old_sessions() {
        let mut detector = CommitDetector::new();

        detector.process_event_full(Trace2Event::Start {
            sid: "sid-old".to_string(),
            argv: vec!["git".to_string(), "commit".to_string()],
        });
        detector.process_event_full(Trace2Event::DefRepo {
            sid: "sid-old".to_string(),
            repo_path: PathBuf::from("/repo"),
        });
        detector.process_event_full(Trace2Event::CmdName {
            sid: "sid-old".to_string(),
            cmd_name: "commit".to_string(),
        });

        // Prune with zero duration makes everything stale
        detector.prune_stale(Duration::from_secs(0));

        // The session is gone, so exit produces nothing
        let result = detector.process_event_full(Trace2Event::CommandExit {
            sid: "sid-old".to_string(),
            exit_code: 0,
        });
        assert!(
            result.is_none(),
            "pruned session should not emit an operation"
        );
    }

    #[test]
    fn full_child_process_event_does_not_emit_operation() {
        let mut detector = CommitDetector::new();

        // Start a parent session
        detector.process_event_full(Trace2Event::Start {
            sid: "parent-abc".to_string(),
            argv: vec![
                "git".to_string(),
                "commit".to_string(),
                "-m".to_string(),
                "msg".to_string(),
            ],
        });
        detector.process_event_full(Trace2Event::DefRepo {
            sid: "parent-abc".to_string(),
            repo_path: PathBuf::from("/repo"),
        });
        detector.process_event_full(Trace2Event::CmdName {
            sid: "parent-abc".to_string(),
            cmd_name: "commit".to_string(),
        });

        // Child process exit (non-root SID) should NOT emit an operation
        let result = detector.process_event_full(Trace2Event::CommandExit {
            sid: "parent-abc/child-def".to_string(),
            exit_code: 0,
        });
        assert!(
            result.is_none(),
            "child process exit should not emit an operation"
        );

        // Parent session should still be intact and detect on its own exit
        let result = detector.process_event_full(Trace2Event::CommandExit {
            sid: "parent-abc".to_string(),
            exit_code: 0,
        });
        assert!(
            result.is_some(),
            "parent should still emit operation after child exit"
        );
    }

    #[test]
    fn full_two_concurrent_sessions_independent() {
        let mut detector = CommitDetector::new();

        // Session 1: rebase
        detector.process_event_full(Trace2Event::Start {
            sid: "sid-1".to_string(),
            argv: vec!["git".to_string(), "rebase".to_string(), "main".to_string()],
        });
        // Session 2: commit
        detector.process_event_full(Trace2Event::Start {
            sid: "sid-2".to_string(),
            argv: vec![
                "git".to_string(),
                "commit".to_string(),
                "-m".to_string(),
                "feat".to_string(),
            ],
        });

        detector.process_event_full(Trace2Event::DefRepo {
            sid: "sid-1".to_string(),
            repo_path: PathBuf::from("/repo-1"),
        });
        detector.process_event_full(Trace2Event::DefRepo {
            sid: "sid-2".to_string(),
            repo_path: PathBuf::from("/repo-2"),
        });

        detector.process_event_full(Trace2Event::CmdName {
            sid: "sid-1".to_string(),
            cmd_name: "rebase".to_string(),
        });
        detector.process_event_full(Trace2Event::CmdName {
            sid: "sid-2".to_string(),
            cmd_name: "commit".to_string(),
        });

        // Session 2 exits first - should get Commit
        let result = detector.process_event_full(Trace2Event::CommandExit {
            sid: "sid-2".to_string(),
            exit_code: 0,
        });
        match result {
            Some(DetectedOperation::Commit { repo_path }) => {
                assert_eq!(repo_path, PathBuf::from("/repo-2"));
            }
            other => panic!("expected Commit for session 2, got {:?}", other),
        }

        // Session 1 exits - should get Rewrite/Rebase
        let result = detector.process_event_full(Trace2Event::CommandExit {
            sid: "sid-1".to_string(),
            exit_code: 0,
        });
        match result {
            Some(DetectedOperation::Rewrite {
                repo_path, kind, ..
            }) => {
                assert_eq!(repo_path, PathBuf::from("/repo-1"));
                assert_eq!(kind, RewriteKind::Rebase);
            }
            other => panic!("expected Rewrite/Rebase for session 1, got {:?}", other),
        }
    }
}
