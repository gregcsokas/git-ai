use crate::daemon::domain::{
    AliasResolution, CommandScope, Confidence, FamilyKey, NormalizedCommand, RefChange, RepoContext,
};
use crate::daemon::git_backend::{GitBackend, ReflogCut};
use crate::error::GitAiError;
use crate::observability;
use serde_json::Value;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

#[derive(Debug, Clone)]
pub struct PendingTraceCommand {
    pub root_sid: String,
    pub raw_argv: Vec<String>,
    pub root_cmd_name: Option<String>,
    pub observed_child_commands: Vec<String>,
    pub worktree: Option<PathBuf>,
    pub family_key: Option<FamilyKey>,
    pub started_at_ns: u128,
    pub exit_code: Option<i32>,
    pub finished_at_ns: Option<u128>,
    pub pre_repo: Option<RepoContext>,
    pub post_repo: Option<RepoContext>,
    pub reflog_start_cut: Option<ReflogCut>,
    pub reflog_end_cut: Option<ReflogCut>,
    pub pre_ref_snapshot: Option<HashMap<String, String>>,
    pub alias_resolution: AliasResolution,
    pub wrapper_mirror: bool,
}

#[derive(Debug, Clone)]
pub struct RawExitFrame {
    pub exit_code: i32,
    pub finished_at_ns: u128,
}

#[derive(Default)]
pub struct TraceNormalizerState {
    pub pending: HashMap<String, PendingTraceCommand>,
    pub deferred_exits: HashMap<String, RawExitFrame>,
    pub sid_to_worktree: HashMap<String, PathBuf>,
    pub sid_to_family: HashMap<String, FamilyKey>,
}

pub struct TraceNormalizer<B: GitBackend> {
    backend: Arc<B>,
    state: TraceNormalizerState,
}

impl<B: GitBackend> TraceNormalizer<B> {
    pub fn new(backend: Arc<B>) -> Self {
        Self {
            backend,
            state: TraceNormalizerState::default(),
        }
    }

    pub fn state(&self) -> &TraceNormalizerState {
        &self.state
    }

    pub fn ingest_payload(&mut self, payload: &Value) -> Result<Option<NormalizedCommand>, GitAiError> {
        let event = payload
            .get("event")
            .and_then(Value::as_str)
            .ok_or_else(|| GitAiError::Generic("trace payload missing event".to_string()))?;
        let sid = payload
            .get("sid")
            .and_then(Value::as_str)
            .ok_or_else(|| GitAiError::Generic("trace payload missing sid".to_string()))?;
        let root_sid = root_sid(sid).to_string();
        let ts = payload_timestamp_ns(payload)?;

        match event {
            "start" => self.handle_start(payload, sid, &root_sid, ts),
            "def_repo" => self.handle_def_repo(payload, sid, &root_sid),
            "cmd_name" => self.handle_cmd_name(payload, sid, &root_sid),
            "exit" => self.handle_exit(payload, sid, &root_sid, ts),
            _ => Ok(None),
        }
    }

    fn handle_start(
        &mut self,
        payload: &Value,
        sid: &str,
        root_sid: &str,
        started_at_ns: u128,
    ) -> Result<Option<NormalizedCommand>, GitAiError> {
        if sid != root_sid {
            return Ok(None);
        }

        let raw_argv = payload_argv(payload);
        let mut worktree = payload_worktree(payload)
            .or_else(|| worktree_from_argv(&raw_argv))
            .or_else(|| self.state.sid_to_worktree.get(root_sid).cloned());

        if worktree.is_none()
            && let Some(cwd) = payload.get("cwd").and_then(Value::as_str)
        {
            worktree = Some(PathBuf::from(cwd));
        }

        let family_key = if let Some(worktree) = worktree.as_deref() {
            match self.backend.resolve_family(worktree) {
                Ok(family) => {
                    self.state
                        .sid_to_family
                        .insert(root_sid.to_string(), family.clone());
                    Some(family)
                }
                Err(_) => self.state.sid_to_family.get(root_sid).cloned(),
            }
        } else {
            self.state.sid_to_family.get(root_sid).cloned()
        };

        let pre_repo = if let Some(worktree) = worktree.as_deref() {
            self.backend.repo_context(worktree).ok()
        } else {
            None
        };

        let reflog_start_cut = if let Some(family) = family_key.as_ref() {
            self.backend.reflog_cut(family).ok()
        } else {
            None
        };
        let pre_ref_snapshot = if let Some(family) = family_key.as_ref() {
            self.backend.ref_snapshot(family).ok()
        } else {
            None
        };

        let alias_resolution = self
            .backend
            .resolve_alias(worktree.as_deref(), &raw_argv)
            .unwrap_or_else(|e| {
                observability::log_error(
                    &e,
                    Some(serde_json::json!({
                        "component": "trace_normalizer",
                        "phase": "alias_resolution",
                        "root_sid": root_sid,
                    })),
                );
                AliasResolution::Unknown {
                    reason: "alias resolution failed".to_string(),
                }
            });

        let wrapper_mirror = payload
            .get("wrapper_mirror")
            .and_then(Value::as_bool)
            .unwrap_or(false);

        let pending = PendingTraceCommand {
            root_sid: root_sid.to_string(),
            raw_argv,
            root_cmd_name: None,
            observed_child_commands: Vec::new(),
            worktree,
            family_key,
            started_at_ns,
            exit_code: None,
            finished_at_ns: None,
            pre_repo,
            post_repo: None,
            reflog_start_cut,
            reflog_end_cut: None,
            pre_ref_snapshot,
            alias_resolution,
            wrapper_mirror,
        };
        self.state.pending.insert(root_sid.to_string(), pending);

        if let Some(exit) = self.state.deferred_exits.remove(root_sid) {
            return self.finalize_root_exit(root_sid, exit.exit_code, exit.finished_at_ns);
        }

        Ok(None)
    }

    fn handle_def_repo(
        &mut self,
        payload: &Value,
        _sid: &str,
        root_sid: &str,
    ) -> Result<Option<NormalizedCommand>, GitAiError> {
        let repo = payload
            .get("repo")
            .and_then(Value::as_str)
            .map(PathBuf::from)
            .ok_or_else(|| GitAiError::Generic("def_repo missing repo path".to_string()))?;

        self.state
            .sid_to_worktree
            .insert(root_sid.to_string(), repo.clone());

        let family = self.backend.resolve_family(&repo).ok();
        if let Some(family) = family.as_ref() {
            self.state
                .sid_to_family
                .insert(root_sid.to_string(), family.clone());
        }

        if let Some(pending) = self.state.pending.get_mut(root_sid) {
            pending.worktree = Some(repo);
            if let Some(family) = family {
                pending.family_key = Some(family.clone());
                pending.reflog_start_cut = self.backend.reflog_cut(&family).ok();
                pending.pre_ref_snapshot = self.backend.ref_snapshot(&family).ok();
            }
        }
        Ok(None)
    }

    fn handle_cmd_name(
        &mut self,
        payload: &Value,
        sid: &str,
        root_sid: &str,
    ) -> Result<Option<NormalizedCommand>, GitAiError> {
        let cmd = payload
            .get("name")
            .and_then(Value::as_str)
            .ok_or_else(|| GitAiError::Generic("cmd_name missing name".to_string()))?
            .to_string();

        if is_internal_cmd_name(&cmd) {
            return Ok(None);
        }

        if sid == root_sid {
            if let Some(pending) = self.state.pending.get_mut(root_sid) {
                pending.root_cmd_name = Some(cmd);
            }
            return Ok(None);
        }

        if let Some(pending) = self.state.pending.get_mut(root_sid) {
            pending.observed_child_commands.push(cmd);
        }
        Ok(None)
    }

    fn handle_exit(
        &mut self,
        payload: &Value,
        sid: &str,
        root_sid: &str,
        finished_at_ns: u128,
    ) -> Result<Option<NormalizedCommand>, GitAiError> {
        if sid != root_sid {
            return Ok(None);
        }

        let exit_code = payload
            .get("code")
            .or_else(|| payload.get("exit_code"))
            .and_then(Value::as_i64)
            .unwrap_or(0) as i32;

        if !self.state.pending.contains_key(root_sid) {
            self.state.deferred_exits.insert(
                root_sid.to_string(),
                RawExitFrame {
                    exit_code,
                    finished_at_ns,
                },
            );
            return Ok(None);
        }

        self.finalize_root_exit(root_sid, exit_code, finished_at_ns)
    }

    fn finalize_root_exit(
        &mut self,
        root_sid: &str,
        exit_code: i32,
        finished_at_ns: u128,
    ) -> Result<Option<NormalizedCommand>, GitAiError> {
        let mut pending = self
            .state
            .pending
            .remove(root_sid)
            .ok_or_else(|| GitAiError::Generic("missing pending command at finalize".to_string()))?;

        pending.exit_code = Some(exit_code);
        pending.finished_at_ns = Some(finished_at_ns);

        if pending.worktree.is_none()
            && let Some(worktree) = self.state.sid_to_worktree.get(root_sid)
        {
            pending.worktree = Some(worktree.clone());
        }
        if pending.family_key.is_none()
            && let Some(family) = self.state.sid_to_family.get(root_sid)
        {
            pending.family_key = Some(family.clone());
        }

        if let Some(worktree) = pending.worktree.as_deref() {
            pending.post_repo = self.backend.repo_context(worktree).ok();
        }

        if let Some(family) = pending.family_key.as_ref() {
            pending.reflog_end_cut = self.backend.reflog_cut(family).ok();
        }

        let mut confidence = Confidence::Low;
        let mut ref_changes = Vec::new();
        if let Some(family) = pending.family_key.as_ref() {
            ref_changes = match (pending.reflog_start_cut.as_ref(), pending.reflog_end_cut.as_ref()) {
                (Some(start), Some(end)) => match self.backend.reflog_delta(family, start, end) {
                    Ok(changes) => {
                        confidence = Confidence::High;
                        changes
                    }
                    Err(reflog_err) => {
                        observability::log_error(
                            &reflog_err,
                            Some(serde_json::json!({
                                "component": "trace_normalizer",
                                "phase": "reflog_delta",
                                "root_sid": pending.root_sid,
                            })),
                        );
                        snapshot_diff(
                            pending.pre_ref_snapshot.clone().unwrap_or_default(),
                            self.backend.ref_snapshot(family).unwrap_or_default(),
                        )
                    }
                },
                _ => snapshot_diff(
                    pending.pre_ref_snapshot.clone().unwrap_or_default(),
                    self.backend.ref_snapshot(family).unwrap_or_default(),
                ),
            };
            if confidence != Confidence::High {
                confidence = Confidence::Medium;
            }
        }

        let mut primary_command = select_primary_command(
            pending.root_cmd_name.as_deref(),
            &pending.alias_resolution,
            &pending.observed_child_commands,
            &pending.raw_argv,
        );

        let mut family_key = pending.family_key.clone();
        let mut scope = if let Some(key) = family_key.clone() {
            CommandScope::Family(key)
        } else {
            CommandScope::Global
        };

        if exit_code == 0
            && matches!(primary_command.as_deref(), Some("clone" | "init"))
            && family_key.is_none()
        {
            let target = if primary_command.as_deref() == Some("clone") {
                self.backend.clone_target(
                    &pending.raw_argv,
                    pending.worktree.as_deref().and_then(Path::parent),
                )
            } else {
                self.backend.init_target(
                    &pending.raw_argv,
                    pending.worktree.as_deref().and_then(Path::parent),
                )
            };
            if let Some(target) = target
                && let Ok(resolved_family) = self.backend.resolve_family(&target)
            {
                family_key = Some(resolved_family.clone());
                scope = CommandScope::Family(resolved_family);
                if pending.worktree.is_none() {
                    pending.worktree = Some(target);
                }
            }
        }

        if primary_command.is_none() {
            primary_command = argv_primary_command(&pending.raw_argv);
        }

        let normalized = NormalizedCommand {
            scope,
            family_key,
            worktree: pending.worktree,
            root_sid: pending.root_sid,
            raw_argv: pending.raw_argv,
            primary_command,
            alias_resolution: pending.alias_resolution,
            observed_child_commands: pending.observed_child_commands,
            exit_code,
            started_at_ns: pending.started_at_ns,
            finished_at_ns,
            pre_repo: pending.pre_repo,
            post_repo: pending.post_repo,
            ref_changes,
            confidence,
            wrapper_mirror: pending.wrapper_mirror,
        };

        Ok(Some(normalized))
    }
}

fn snapshot_diff(
    before: HashMap<String, String>,
    after: HashMap<String, String>,
) -> Vec<RefChange> {
    let mut refs = before
        .keys()
        .chain(after.keys())
        .cloned()
        .collect::<Vec<_>>();
    refs.sort();
    refs.dedup();
    refs.into_iter()
        .filter_map(|reference| {
            let old = before.get(&reference).cloned().unwrap_or_default();
            let new = after.get(&reference).cloned().unwrap_or_default();
            if old == new {
                None
            } else {
                Some(RefChange {
                    reference,
                    old,
                    new,
                })
            }
        })
        .collect()
}

fn payload_timestamp_ns(payload: &Value) -> Result<u128, GitAiError> {
    let time = payload
        .get("ts")
        .or_else(|| payload.get("time"))
        .or_else(|| payload.get("time_ns"))
        .and_then(Value::as_u64)
        .ok_or_else(|| GitAiError::Generic("trace payload missing timestamp".to_string()))?;
    Ok(time as u128)
}

fn payload_argv(payload: &Value) -> Vec<String> {
    payload
        .get("argv")
        .and_then(Value::as_array)
        .map(|argv| {
            argv.iter()
                .filter_map(Value::as_str)
                .map(ToString::to_string)
                .collect()
        })
        .unwrap_or_default()
}

fn payload_worktree(payload: &Value) -> Option<PathBuf> {
    payload
        .get("worktree")
        .or_else(|| payload.get("repo"))
        .and_then(Value::as_str)
        .map(PathBuf::from)
}

fn root_sid(sid: &str) -> &str {
    sid.split('/').next().unwrap_or(sid)
}

fn is_internal_cmd_name(name: &str) -> bool {
    name.starts_with("_run_")
}

fn worktree_from_argv(argv: &[String]) -> Option<PathBuf> {
    let mut idx = 0;
    while idx < argv.len() {
        if argv[idx] == "-C" && idx + 1 < argv.len() {
            return Some(PathBuf::from(argv[idx + 1].clone()));
        }
        idx += 1;
    }
    None
}

fn argv_primary_command(argv: &[String]) -> Option<String> {
    let mut iter = argv.iter().map(|s| s.as_str()).peekable();
    if matches!(iter.peek(), Some(&"git")) {
        iter.next();
    }
    while let Some(token) = iter.next() {
        if token == "-C" {
            iter.next();
            continue;
        }
        if token.starts_with('-') {
            continue;
        }
        return Some(token.to_string());
    }
    None
}

fn select_primary_command(
    root_cmd_name: Option<&str>,
    alias_resolution: &AliasResolution,
    observed_child_commands: &[String],
    argv: &[String],
) -> Option<String> {
    if let Some(name) = root_cmd_name
        && !is_internal_cmd_name(name)
    {
        return Some(name.to_string());
    }

    match alias_resolution {
        AliasResolution::DirectAlias { expansion, .. } => {
            if !expansion.trim().is_empty() {
                return Some(expansion.clone());
            }
        }
        AliasResolution::ShellAlias { expansion, .. } => {
            if !expansion.trim().is_empty() {
                return Some(expansion.clone());
            }
        }
        AliasResolution::None | AliasResolution::Unknown { .. } => {}
    }

    for child in observed_child_commands {
        if !is_internal_cmd_name(child) {
            return Some(child.clone());
        }
    }

    argv_primary_command(argv)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    #[derive(Default)]
    struct MockBackend {
        family_by_worktree: Mutex<HashMap<String, FamilyKey>>,
        context_by_worktree: Mutex<HashMap<String, RepoContext>>,
        refs_by_family: Mutex<HashMap<String, HashMap<String, String>>>,
        reflog_ordinal: Mutex<u64>,
        alias: Mutex<Option<AliasResolution>>,
    }

    impl MockBackend {
        fn set_family(&self, worktree: &str, family: &str) {
            self.family_by_worktree
                .lock()
                .unwrap()
                .insert(worktree.to_string(), FamilyKey::new(family.to_string()));
        }

        fn set_context(&self, worktree: &str, head: &str) {
            self.context_by_worktree.lock().unwrap().insert(
                worktree.to_string(),
                RepoContext {
                    head: Some(head.to_string()),
                    branch: Some("main".to_string()),
                    detached: false,
                    workspace_fingerprint: None,
                },
            );
        }
    }

    impl GitBackend for MockBackend {
        fn resolve_family(&self, worktree: &Path) -> Result<FamilyKey, GitAiError> {
            self.family_by_worktree
                .lock()
                .unwrap()
                .get(worktree.to_string_lossy().as_ref())
                .cloned()
                .ok_or_else(|| GitAiError::Generic("family not found".to_string()))
        }

        fn repo_context(&self, worktree: &Path) -> Result<RepoContext, GitAiError> {
            self.context_by_worktree
                .lock()
                .unwrap()
                .get(worktree.to_string_lossy().as_ref())
                .cloned()
                .ok_or_else(|| GitAiError::Generic("context not found".to_string()))
        }

        fn ref_snapshot(&self, family: &FamilyKey) -> Result<HashMap<String, String>, GitAiError> {
            Ok(self
                .refs_by_family
                .lock()
                .unwrap()
                .get(&family.0)
                .cloned()
                .unwrap_or_default())
        }

        fn reflog_cut(&self, _family: &FamilyKey) -> Result<ReflogCut, GitAiError> {
            let mut ordinal = self.reflog_ordinal.lock().unwrap();
            *ordinal += 1;
            Ok(ReflogCut {
                ordinal: *ordinal,
                hash: None,
            })
        }

        fn reflog_delta(
            &self,
            _family: &FamilyKey,
            _start: &ReflogCut,
            _end: &ReflogCut,
        ) -> Result<Vec<RefChange>, GitAiError> {
            Ok(vec![])
        }

        fn resolve_alias(
            &self,
            _worktree: Option<&Path>,
            _argv: &[String],
        ) -> Result<AliasResolution, GitAiError> {
            Ok(self.alias.lock().unwrap().clone().unwrap_or(AliasResolution::None))
        }

        fn clone_target(&self, _argv: &[String], _cwd_hint: Option<&Path>) -> Option<PathBuf> {
            None
        }

        fn init_target(&self, _argv: &[String], _cwd_hint: Option<&Path>) -> Option<PathBuf> {
            None
        }
    }

    fn payload(event: &str, sid: &str, ts: u64) -> Value {
        serde_json::json!({
            "event": event,
            "sid": sid,
            "ts": ts,
        })
    }

    #[test]
    fn normalizer_emits_one_command_for_start_exit() {
        let backend = Arc::new(MockBackend::default());
        backend.set_family("/repo", "/repo/.git");
        backend.set_context("/repo", "head-a");
        let mut normalizer = TraceNormalizer::new(backend);

        let start = serde_json::json!({
            "event":"start",
            "sid":"s1",
            "ts":1,
            "argv":["git","status"],
            "worktree":"/repo"
        });
        let exit = serde_json::json!({
            "event":"exit",
            "sid":"s1",
            "ts":2,
            "code":0
        });

        assert!(normalizer.ingest_payload(&start).unwrap().is_none());
        let cmd = normalizer.ingest_payload(&exit).unwrap().unwrap();
        assert_eq!(cmd.root_sid, "s1");
        assert_eq!(cmd.primary_command.as_deref(), Some("status"));
        assert_eq!(cmd.exit_code, 0);
    }

    #[test]
    fn normalizer_handles_exit_before_start() {
        let backend = Arc::new(MockBackend::default());
        backend.set_family("/repo", "/repo/.git");
        backend.set_context("/repo", "head-a");
        let mut normalizer = TraceNormalizer::new(backend);

        let exit = serde_json::json!({
            "event":"exit",
            "sid":"s2",
            "ts":10,
            "code":0
        });
        let start = serde_json::json!({
            "event":"start",
            "sid":"s2",
            "ts":1,
            "argv":["git","status"],
            "worktree":"/repo"
        });

        assert!(normalizer.ingest_payload(&exit).unwrap().is_none());
        let cmd = normalizer.ingest_payload(&start).unwrap().unwrap();
        assert_eq!(cmd.root_sid, "s2");
        assert_eq!(cmd.finished_at_ns, 10);
    }

    #[test]
    fn child_cmd_name_enriches_root() {
        let backend = Arc::new(MockBackend::default());
        backend.set_family("/repo", "/repo/.git");
        backend.set_context("/repo", "head-a");
        let mut normalizer = TraceNormalizer::new(backend);

        let start = serde_json::json!({
            "event":"start",
            "sid":"s3",
            "ts":1,
            "argv":["git","foo"],
            "worktree":"/repo"
        });
        let child = serde_json::json!({
            "event":"cmd_name",
            "sid":"s3/child1",
            "ts":2,
            "name":"status"
        });
        let exit = serde_json::json!({
            "event":"exit",
            "sid":"s3",
            "ts":3,
            "code":0
        });

        normalizer.ingest_payload(&start).unwrap();
        normalizer.ingest_payload(&child).unwrap();
        let cmd = normalizer.ingest_payload(&exit).unwrap().unwrap();
        assert_eq!(cmd.observed_child_commands, vec!["status".to_string()]);
        assert_eq!(cmd.primary_command.as_deref(), Some("status"));
    }

    #[test]
    fn no_repo_routes_to_global_scope() {
        let backend = Arc::new(MockBackend::default());
        let mut normalizer = TraceNormalizer::new(backend);

        let start = serde_json::json!({
            "event":"start",
            "sid":"s4",
            "ts":1,
            "argv":["git","version"]
        });
        let exit = serde_json::json!({
            "event":"exit",
            "sid":"s4",
            "ts":2,
            "code":0
        });

        normalizer.ingest_payload(&start).unwrap();
        let cmd = normalizer.ingest_payload(&exit).unwrap().unwrap();
        assert!(matches!(cmd.scope, CommandScope::Global));
    }

    #[test]
    fn ignores_non_supported_trace_events() {
        let backend = Arc::new(MockBackend::default());
        let mut normalizer = TraceNormalizer::new(backend);
        let p = payload("region_enter", "s5", 1);
        assert!(normalizer.ingest_payload(&p).unwrap().is_none());
    }
}

