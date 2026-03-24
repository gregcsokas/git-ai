use crate::daemon::DaemonConfig;
use crate::daemon::domain::{RefChange, RepoContext};
use crate::daemon::open_local_socket_stream_with_timeout;
use crate::error::GitAiError;
use crate::git::cli_parser::{ParsedGitInvocation, parse_git_cli_args};
use crate::git::repo_state::{
    common_dir_for_worktree, git_dir_for_worktree, is_valid_git_oid, read_head_state_for_worktree,
    worktree_root_for_path,
};
use interprocess::local_socket::prelude::*;
use serde_json::{Value, json};
use std::collections::{HashMap, HashSet};
use std::fs::{self, File};
use std::io::{BufRead, BufReader, BufWriter, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

pub const WRAPPER_AUTHORITATIVE_BOUNDARY_FIELD: &str = "git_ai_authoritative_boundary";
const WRAPPER_TRACE_NESTING_VALUE: &str = "10";
const WRAPPER_TRACE_CONNECT_TIMEOUT: Duration = Duration::from_secs(2);
static WRAPPER_TRACE_COUNTER: AtomicU64 = AtomicU64::new(1);

#[derive(Debug)]
pub struct WrapperDaemonTraceSession {
    root_sid: String,
    cwd: PathBuf,
    raw_argv: Vec<String>,
    worktree: Option<PathBuf>,
    pre_repo: Option<RepoContext>,
    tracked_refs: Vec<String>,
    worktree_head_start_offset: Option<u64>,
    family_reflog_start_offsets: Option<HashMap<String, u64>>,
    started_at_ns: u128,
    trace_event_target: String,
    stream: BufWriter<LocalSocketStream>,
}

impl WrapperDaemonTraceSession {
    pub fn start(
        config: &DaemonConfig,
        cwd: &Path,
        raw_argv: &[String],
        primary_command: Option<&str>,
    ) -> Result<Self, GitAiError> {
        let started_at_ns = now_unix_nanos();
        let root_sid = new_wrapper_root_sid(started_at_ns);
        let cwd = cwd.canonicalize().unwrap_or_else(|_| cwd.to_path_buf());
        let worktree = resolve_worktree_for_command(&cwd, raw_argv);
        let pre_repo = worktree
            .as_deref()
            .and_then(read_head_state_for_worktree)
            .map(repo_context_from_head_state);
        let primary_command = primary_command
            .map(ToString::to_string)
            .or_else(|| parsed_invocation(raw_argv).command);
        let tracked_refs = worktree
            .as_deref()
            .map(|worktree| {
                tracked_reflog_refs_for_command(
                    primary_command.as_deref(),
                    pre_repo.as_ref(),
                    worktree,
                    raw_argv,
                )
            })
            .unwrap_or_default();
        let worktree_head_start_offset = worktree.as_deref().and_then(worktree_head_reflog_offset);
        let family_reflog_start_offsets = worktree
            .as_deref()
            .and_then(|worktree| reflog_offsets_for_refs(worktree, &tracked_refs));
        let trace_event_target = config.trace2_event_target();
        let socket = open_local_socket_stream_with_timeout(
            &config.trace_socket_path,
            WRAPPER_TRACE_CONNECT_TIMEOUT,
        )?;
        let mut session = Self {
            root_sid,
            cwd,
            raw_argv: raw_argv.to_vec(),
            worktree,
            pre_repo,
            tracked_refs,
            worktree_head_start_offset,
            family_reflog_start_offsets,
            started_at_ns,
            trace_event_target,
            stream: BufWriter::new(socket),
        };
        let payload = session.start_payload();
        session.send_payload(&payload)?;
        Ok(session)
    }

    pub fn child_trace_event_target(&self) -> &str {
        &self.trace_event_target
    }

    pub fn child_trace_parent_sid(&self) -> &str {
        &self.root_sid
    }

    pub fn child_trace_nesting_value(&self) -> &'static str {
        WRAPPER_TRACE_NESTING_VALUE
    }

    pub fn finish(mut self, exit_code: i32) -> Result<(), GitAiError> {
        let finished_at_ns = now_unix_nanos();
        let worktree = self
            .worktree
            .clone()
            .or_else(|| resolve_created_repo_worktree(&self.cwd, &self.raw_argv, exit_code));
        let post_repo = worktree
            .as_deref()
            .and_then(read_head_state_for_worktree)
            .map(repo_context_from_head_state);
        let worktree_head_end_offset = worktree.as_deref().and_then(worktree_head_reflog_offset);
        let family_reflog_end_offsets = worktree
            .as_deref()
            .and_then(|worktree| reflog_offsets_for_refs(worktree, &self.tracked_refs));
        let mut ref_changes = Vec::new();
        if let Some(worktree) = worktree.as_deref() {
            if let (Some(start), Some(end)) = (
                self.family_reflog_start_offsets.as_ref(),
                family_reflog_end_offsets.as_ref(),
            ) {
                ref_changes.extend(reflog_delta_from_offsets(worktree, start, end)?);
            }
            if let (Some(start), Some(end)) =
                (self.worktree_head_start_offset, worktree_head_end_offset)
            {
                push_unique_ref_changes(
                    &mut ref_changes,
                    worktree_head_reflog_delta(worktree, start, end)?,
                );
            }
        }

        let payload = json!({
            "event": "exit",
            "sid": &self.root_sid,
            "time_ns": ns_to_json_u64(finished_at_ns),
            "code": exit_code,
            "argv": &self.raw_argv,
            "cwd": self.cwd.to_string_lossy().to_string(),
            WRAPPER_AUTHORITATIVE_BOUNDARY_FIELD: true,
            "worktree": worktree.as_ref().map(|path| path.to_string_lossy().to_string()),
            "git_ai_pre_repo": &self.pre_repo,
            "git_ai_post_repo": post_repo,
            "git_ai_worktree_head_reflog_start": self.worktree_head_start_offset,
            "git_ai_worktree_head_reflog_end": worktree_head_end_offset,
            "git_ai_family_reflog_start": self.family_reflog_start_offsets,
            "git_ai_family_reflog_end": family_reflog_end_offsets,
            "git_ai_family_reflog_changes": ref_changes,
        });
        self.send_payload(&payload)
    }

    fn start_payload(&self) -> Value {
        json!({
            "event": "start",
            "sid": &self.root_sid,
            "time_ns": ns_to_json_u64(self.started_at_ns),
            "argv": &self.raw_argv,
            "cwd": self.cwd.to_string_lossy().to_string(),
            WRAPPER_AUTHORITATIVE_BOUNDARY_FIELD: true,
            "worktree": self.worktree.as_ref().map(|path| path.to_string_lossy().to_string()),
            "git_ai_pre_repo": &self.pre_repo,
            "git_ai_worktree_head_reflog_start": self.worktree_head_start_offset,
            "git_ai_family_reflog_start": &self.family_reflog_start_offsets,
        })
    }

    fn send_payload(&mut self, payload: &Value) -> Result<(), GitAiError> {
        serde_json::to_writer(&mut self.stream, payload)?;
        self.stream.write_all(b"\n")?;
        self.stream.flush()?;
        Ok(())
    }
}

fn now_unix_nanos() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or(0)
}

fn ns_to_json_u64(value: u128) -> u64 {
    u64::try_from(value).unwrap_or(u64::MAX)
}

fn new_wrapper_root_sid(now_ns: u128) -> String {
    let ordinal = WRAPPER_TRACE_COUNTER.fetch_add(1, Ordering::SeqCst);
    format!(
        "git-ai-wrapper-{}-{}-{}",
        std::process::id(),
        now_ns,
        ordinal
    )
}

fn parsed_invocation(raw_argv: &[String]) -> ParsedGitInvocation {
    parse_git_cli_args(git_invocation_tokens(raw_argv))
}

fn git_invocation_tokens(raw_argv: &[String]) -> &[String] {
    if raw_argv
        .first()
        .and_then(|token| Path::new(token).file_name().and_then(|name| name.to_str()))
        .is_some_and(|name| name == "git" || name == "git.exe")
    {
        &raw_argv[1..]
    } else {
        raw_argv
    }
}

fn resolve_command_base_dir(global_args: &[String], cwd: &Path) -> PathBuf {
    let mut base = cwd.to_path_buf();
    let mut idx = 0usize;
    while idx < global_args.len() {
        let token = &global_args[idx];
        if token == "-C" {
            let Some(path_arg) = global_args.get(idx + 1) else {
                break;
            };
            let candidate = PathBuf::from(path_arg);
            base = if candidate.is_absolute() {
                candidate
            } else {
                base.join(candidate)
            };
            idx += 2;
            continue;
        }
        if token.starts_with("-C") && token.len() > 2 {
            let candidate = PathBuf::from(&token[2..]);
            base = if candidate.is_absolute() {
                candidate
            } else {
                base.join(candidate)
            };
            idx += 1;
            continue;
        }
        idx += 1;
    }
    base
}

fn resolve_worktree_for_command(cwd: &Path, raw_argv: &[String]) -> Option<PathBuf> {
    let parsed = parsed_invocation(raw_argv);
    let base_dir = resolve_command_base_dir(&parsed.global_args, cwd);
    worktree_root_for_path(&base_dir).or_else(|| {
        if base_dir.join(".git").exists() {
            Some(base_dir)
        } else {
            None
        }
    })
}

fn resolve_created_repo_worktree(
    cwd: &Path,
    raw_argv: &[String],
    exit_code: i32,
) -> Option<PathBuf> {
    if exit_code != 0 {
        return None;
    }
    let parsed = parsed_invocation(raw_argv);
    match parsed.command.as_deref() {
        Some("clone") => infer_clone_target(&parsed, cwd),
        Some("init") => infer_init_target(&parsed, cwd),
        _ => resolve_worktree_for_command(cwd, raw_argv),
    }
}

fn infer_clone_target(parsed: &ParsedGitInvocation, cwd: &Path) -> Option<PathBuf> {
    let args = parsed.command_args.as_slice();
    let positional = clone_or_init_positionals(args);
    if positional.is_empty() {
        return None;
    }
    let target = if positional.len() >= 2 {
        PathBuf::from(&positional[1])
    } else {
        default_clone_target_from_source(&positional[0])?
    };
    let target = resolve_target(target, Some(cwd));
    worktree_root_for_path(&target).or(Some(target))
}

fn infer_init_target(parsed: &ParsedGitInvocation, cwd: &Path) -> Option<PathBuf> {
    let positional = clone_or_init_positionals(&parsed.command_args);
    let target = positional
        .first()
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."));
    let target = resolve_target(target, Some(cwd));
    worktree_root_for_path(&target).or(Some(target))
}

fn clone_or_init_positionals(args: &[String]) -> Vec<String> {
    let mut out = Vec::new();
    let mut idx = 0usize;
    while idx < args.len() {
        let arg = &args[idx];
        if arg == "--" {
            out.extend(args[idx + 1..].iter().cloned());
            break;
        }
        if arg.starts_with('-') {
            if clone_or_init_arg_takes_value(arg) && idx + 1 < args.len() {
                idx += 2;
                continue;
            }
            idx += 1;
            continue;
        }
        out.push(arg.clone());
        idx += 1;
    }
    out
}

fn clone_or_init_arg_takes_value(arg: &str) -> bool {
    matches!(
        arg,
        "-b" | "--branch"
            | "--origin"
            | "--upload-pack"
            | "--template"
            | "--separate-git-dir"
            | "--reference"
            | "--dissociate"
            | "--config"
            | "--object-format"
    )
}

fn default_clone_target_from_source(source: &str) -> Option<PathBuf> {
    let source = source.trim_end_matches('/');
    let source = source.strip_suffix(".git").unwrap_or(source);
    let name = source.rsplit('/').next()?.rsplit(':').next()?.to_string();
    if name.is_empty() {
        return None;
    }
    Some(PathBuf::from(name))
}

fn resolve_target(target: PathBuf, cwd_hint: Option<&Path>) -> PathBuf {
    if target.is_absolute() {
        return target;
    }
    cwd_hint.map(|cwd| cwd.join(&target)).unwrap_or(target)
}

fn repo_context_from_head_state(state: crate::git::repo_state::HeadState) -> RepoContext {
    RepoContext {
        head: state.head,
        branch: state.branch,
        detached: state.detached,
    }
}

fn tracked_reflog_refs_for_command(
    command: Option<&str>,
    repo: Option<&RepoContext>,
    worktree: &Path,
    argv: &[String],
) -> Vec<String> {
    let mut refs = Vec::new();
    if let Some(branch) = repo.and_then(|repo| repo.branch.as_deref()) {
        refs.push(format!("refs/heads/{}", branch));
    }
    if command == Some("rebase")
        && let Some(branch_ref) = resolve_explicit_rebase_branch_ref(worktree, argv)
    {
        refs.push(branch_ref);
    }
    if matches!(
        command,
        Some("reset" | "merge" | "pull" | "rebase" | "cherry-pick" | "checkout" | "switch")
    ) {
        refs.push("ORIG_HEAD".to_string());
    }
    if command == Some("stash") {
        refs.push("refs/stash".to_string());
    }
    refs.sort();
    refs.dedup();
    refs
}

fn resolve_explicit_rebase_branch_ref(worktree: &Path, argv: &[String]) -> Option<String> {
    let parsed = parsed_invocation(argv);
    if parsed.command.as_deref() != Some("rebase") {
        return None;
    }
    let branch_spec = crate::git::cli_parser::explicit_rebase_branch_arg(&parsed.command_args)?;
    let branch_ref = explicit_rebase_branch_ref_name(&branch_spec)?;
    crate::git::repo_state::read_ref_oid_for_worktree(worktree, &branch_ref).map(|_| branch_ref)
}

fn explicit_rebase_branch_ref_name(branch_spec: &str) -> Option<String> {
    if branch_spec.starts_with("refs/") {
        return Some(branch_spec.to_string());
    }
    if is_valid_git_oid(branch_spec) || branch_spec == "HEAD" || branch_spec.starts_with("@{") {
        return None;
    }
    Some(format!("refs/heads/{}", branch_spec))
}

fn worktree_head_reflog_offset(worktree: &Path) -> Option<u64> {
    let git_dir = git_dir_for_worktree(worktree)?;
    fs::metadata(git_dir.join("logs").join("HEAD"))
        .ok()
        .map(|metadata| metadata.len())
}

fn reflog_offsets_for_refs(worktree: &Path, refs: &[String]) -> Option<HashMap<String, u64>> {
    let common_dir = common_dir_for_worktree(worktree)?;
    let logs_dir = common_dir.join("logs");
    let mut offsets = HashMap::new();
    for reference in refs {
        let len = fs::metadata(logs_dir.join(reference))
            .ok()
            .map(|metadata| metadata.len())
            .unwrap_or(0);
        offsets.insert(reference.clone(), len);
    }
    Some(offsets)
}

fn reflog_delta_from_offsets(
    worktree: &Path,
    start_offsets: &HashMap<String, u64>,
    end_offsets: &HashMap<String, u64>,
) -> Result<Vec<RefChange>, GitAiError> {
    let common_dir = common_dir_for_worktree(worktree).ok_or_else(|| {
        GitAiError::Generic(format!(
            "failed to resolve common dir for worktree {}",
            worktree.display()
        ))
    })?;
    let refs = start_offsets
        .keys()
        .chain(end_offsets.keys())
        .cloned()
        .collect::<HashSet<_>>();
    let mut out = Vec::new();
    for reference in refs {
        let start_offset = start_offsets.get(&reference).copied().unwrap_or(0);
        let end_offset = end_offsets.get(&reference).copied().unwrap_or(start_offset);
        if end_offset < start_offset {
            return Err(GitAiError::Generic(format!(
                "reflog cut regressed for {} ({} < {})",
                reference, end_offset, start_offset
            )));
        }
        if end_offset == start_offset {
            continue;
        }
        let path = common_dir.join("logs").join(&reference);
        if !path.exists() {
            return Err(GitAiError::Generic(format!(
                "reflog path missing for {}: {}",
                reference,
                path.display()
            )));
        }
        let mut file = File::open(&path)?;
        file.seek(SeekFrom::Start(start_offset))?;
        let reader = BufReader::new(file.take(end_offset.saturating_sub(start_offset)));
        for line in reader.lines() {
            let line = line?;
            if let Some(change) = parse_reflog_line(&reference, &line) {
                out.push(change);
            }
        }
    }
    Ok(out)
}

fn worktree_head_reflog_delta(
    worktree: &Path,
    start_offset: u64,
    end_offset: u64,
) -> Result<Vec<RefChange>, GitAiError> {
    if end_offset < start_offset {
        return Err(GitAiError::Generic(format!(
            "worktree HEAD reflog cut regressed ({} < {})",
            end_offset, start_offset
        )));
    }
    if end_offset == start_offset {
        return Ok(Vec::new());
    }
    let git_dir = git_dir_for_worktree(worktree).ok_or_else(|| {
        GitAiError::Generic(format!(
            "missing gitdir for worktree while reading HEAD reflog: {}",
            worktree.display()
        ))
    })?;
    let path = git_dir.join("logs").join("HEAD");
    let metadata = fs::metadata(&path)?;
    if metadata.len() < end_offset {
        return Err(GitAiError::Generic(format!(
            "worktree HEAD reflog shorter than cut ({} < {}) at {}",
            metadata.len(),
            end_offset,
            path.display()
        )));
    }
    let mut file = File::open(&path)?;
    file.seek(SeekFrom::Start(start_offset))?;
    let reader = BufReader::new(file.take(end_offset.saturating_sub(start_offset)));
    let mut out = Vec::new();
    for line in reader.lines() {
        let line = line?;
        let head = line.split('\t').next().unwrap_or_default();
        let mut parts = head.split_whitespace();
        let Some(old) = parts.next().map(str::trim) else {
            continue;
        };
        let Some(new) = parts.next().map(str::trim) else {
            continue;
        };
        if !is_valid_git_oid(old) || !is_valid_git_oid(new) || old == new {
            continue;
        }
        out.push(RefChange {
            reference: "HEAD".to_string(),
            old: old.to_string(),
            new: new.to_string(),
        });
    }
    Ok(out)
}

fn parse_reflog_line(reference: &str, line: &str) -> Option<RefChange> {
    let head = line.split('\t').next().unwrap_or_default();
    let mut parts = head.split_whitespace();
    let old = parts.next()?.trim();
    let new = parts.next()?.trim();
    if !is_valid_git_oid(old) || !is_valid_git_oid(new) || old == new {
        return None;
    }
    Some(RefChange {
        reference: reference.to_string(),
        old: old.to_string(),
        new: new.to_string(),
    })
}

fn push_unique_ref_changes(out: &mut Vec<RefChange>, incoming: Vec<RefChange>) {
    for change in incoming {
        let duplicate = out.iter().any(|existing| {
            existing.reference == change.reference
                && existing.old == change.old
                && existing.new == change.new
        });
        if !duplicate {
            out.push(change);
        }
    }
}
