use git_ai::core::attribution::{
    Attribution, attributions_to_line_attributions, update_attributions,
};
use git_ai::core::working_log::{AgentId, Checkpoint, CheckpointKind, WorkingLogEntry};

use std::collections::HashMap;
use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::commands::helpers::{debug_log, find_repo_root_for_path, git_cmd, git_cmd_in};

type CheckpointEventData = (
    CheckpointKind,
    PathBuf,
    Vec<PathBuf>,
    Option<AgentId>,
    Option<HashMap<PathBuf, String>>,
);

/// Try to route the checkpoint through the daemon's control socket.
/// Returns true if the daemon handled it, false if we need to fall back to local processing.
#[cfg(not(unix))]
fn try_checkpoint_via_daemon(_args: &[String]) -> bool {
    false
}

#[cfg(unix)]
fn try_checkpoint_via_daemon(args: &[String]) -> bool {
    // Don't route to daemon if explicitly disabled
    if env::var("GIT_AI_NO_DAEMON").as_deref() == Ok("1") {
        return false;
    }

    // Agent presets (claude, cursor, etc.) require local processing with hook input parsing;
    // the daemon control socket doesn't support preset checkpoint requests.
    if let Some(first_arg) = args.first() {
        let name = first_arg.as_str();
        if git_ai::presets::known_presets().contains(&name)
            && !matches!(name, "human" | "mock_ai" | "mock_known_human")
        {
            return false;
        }
    }

    let paths = git_ai::daemon::DaemonPaths::resolve();
    if !paths.control_sock.exists() {
        return false;
    }

    // Parse args to build request
    let mut kind_str: Option<&str> = None;
    let mut file_args: Vec<&str> = Vec::new();
    let mut past_separator = false;

    let mut i = 0;
    while i < args.len() {
        let arg = args[i].as_str();
        if arg == "--" {
            past_separator = true;
            i += 1;
            continue;
        }
        if past_separator {
            file_args.push(arg);
        } else if kind_str.is_none() && matches!(arg, "human" | "mock_ai" | "mock_known_human") {
            kind_str = Some(arg);
        } else {
            file_args.push(arg);
        }
        i += 1;
    }

    let kind = match kind_str {
        Some("mock_ai") => "ai",
        Some("mock_known_human") => "known_human",
        Some("human") | None => "human",
        _ => "human",
    };

    // Check if any file args are absolute paths (cross-repo scenario)
    let has_absolute_paths = file_args.iter().any(|f| PathBuf::from(f).is_absolute());

    if has_absolute_paths && !file_args.is_empty() {
        // Cross-repo mode: group files by their containing repository and send
        // separate daemon requests per repo
        let mut repo_groups: HashMap<PathBuf, Vec<&str>> = HashMap::new();
        for f in &file_args {
            let p = PathBuf::from(f);
            if !p.is_absolute() {
                // Mix of absolute and relative -- fall back to local processing
                return false;
            }
            if let Some(repo_root) = find_repo_root_for_path(&p) {
                repo_groups.entry(repo_root).or_default().push(f);
            }
        }

        if repo_groups.is_empty() {
            println!("0");
            return true;
        }

        let mut total_processed: u64 = 0;
        for (repo_root, files) in &repo_groups {
            let repo_root_str = repo_root.to_string_lossy().to_string();
            let file_values: Vec<serde_json::Value> = files
                .iter()
                .map(|f| {
                    // Make path relative to this repo root for the daemon
                    let p = PathBuf::from(f);
                    let rel = p
                        .strip_prefix(repo_root)
                        .unwrap_or(&p)
                        .to_string_lossy()
                        .replace('\\', "/");
                    serde_json::json!({"path": rel})
                })
                .collect();

            let mut request = serde_json::json!({
                "type": "checkpoint",
                "repo_dir": repo_root_str,
                "kind": kind,
                "files": file_values,
            });

            if kind == "ai" {
                let ts = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_nanos())
                    .unwrap_or(0);
                request["agent"] = serde_json::json!({
                    "tool": kind_str.unwrap_or("mock_ai"),
                    "id": format!("ai-thread-{}", ts),
                    "model": "unknown"
                });
            }

            let request_str = serde_json::to_string(&request).unwrap_or_default();

            match git_ai::daemon::control_client::send_request(&paths.control_sock, &request_str) {
                Ok(resp) if resp.ok => {
                    total_processed += resp.processed.unwrap_or(0) as u64;
                }
                _ => return false,
            }
        }

        println!("{}", total_processed);
        write_checkpoint_debug_log(kind_str.unwrap_or("human"), total_processed as usize);
        return true;
    }

    // Standard mode: single repo from CWD — resolve repo root without extra git spawn
    // by walking up the directory tree (matching find_repo_root_for_path logic)
    let cwd = env::current_dir().unwrap_or_default();
    let repo_root = match find_repo_root_for_path(&cwd) {
        Some(root) => root.to_string_lossy().to_string(),
        None => return false,
    };

    let repo_root_path = PathBuf::from(&repo_root);
    let files: Vec<serde_json::Value> = if file_args.is_empty() {
        let status_output = git_cmd(&["status", "--porcelain", "-u"]).unwrap_or_default();
        status_output
            .lines()
            .filter(|l| l.len() > 3)
            .map(|l| serde_json::json!({"path": l[3..].trim()}))
            .collect()
    } else {
        file_args
            .iter()
            .map(|f| {
                let p = PathBuf::from(f);
                let abs = if p.is_absolute() { p } else { cwd.join(f) };
                let rel = abs
                    .strip_prefix(&repo_root_path)
                    .unwrap_or(&abs)
                    .to_string_lossy()
                    .replace('\\', "/");
                serde_json::json!({"path": rel})
            })
            .collect()
    };

    if files.is_empty() {
        println!("0");
        return true;
    }

    let mut request = serde_json::json!({
        "type": "checkpoint",
        "repo_dir": repo_root,
        "kind": kind,
        "files": files,
    });

    if kind == "ai" {
        let ts = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        request["agent"] = serde_json::json!({
            "tool": kind_str.unwrap_or("mock_ai"),
            "id": format!("ai-thread-{}", ts),
            "model": "unknown"
        });
    }

    let request_str = serde_json::to_string(&request).unwrap_or_default();

    match git_ai::daemon::control_client::send_request(&paths.control_sock, &request_str) {
        Ok(resp) if resp.ok => {
            let processed = resp.processed.unwrap_or(0);
            println!("{}", processed);
            write_checkpoint_debug_log(kind_str.unwrap_or("human"), processed as usize);
            true
        }
        _ => false,
    }
}

pub fn handle_checkpoint(args: &[String]) {
    git_ai::daemon::run::ensure_daemon_running();

    // Determine if this is an agent preset that must be processed locally
    let mut kind_str: Option<&str> = None;
    let mut file_args: Vec<&str> = Vec::new();
    let mut past_separator = false;

    let mut i = 0;
    while i < args.len() {
        let arg = args[i].as_str();
        if arg == "--" {
            past_separator = true;
            i += 1;
            continue;
        }
        if past_separator {
            file_args.push(arg);
        } else if kind_str.is_none() {
            kind_str = Some(arg);
        } else {
            file_args.push(arg);
        }
        i += 1;
    }

    let agent_name = kind_str.unwrap_or("human");

    // Check if this is a real agent preset (not a simple built-in kind)
    let is_agent_preset = git_ai::presets::known_presets().contains(&agent_name)
        && !matches!(agent_name, "human" | "mock_ai" | "mock_known_human");

    if is_agent_preset {
        handle_agent_checkpoint(agent_name, &file_args);
        return;
    }

    // Non-preset checkpoints MUST go through the daemon
    if !try_checkpoint_via_daemon(args) {
        eprintln!("git-ai: checkpoint failed — could not connect to daemon");
        eprintln!("Try restarting: git-ai bg restart");
        std::process::exit(1);
    }
}

/// Write a checkpoint debug log entry if the checkpoint_debug_log feature flag is enabled.
fn write_checkpoint_debug_log(preset_name: &str, event_count: usize) {
    // Check if the feature flag is enabled via GIT_AI_TEST_CONFIG_PATCH
    let enabled = if let Ok(patch_json) = env::var("GIT_AI_TEST_CONFIG_PATCH") {
        if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(&patch_json) {
            parsed["feature_flags"]["checkpoint_debug_log"]
                .as_bool()
                .unwrap_or(false)
        } else {
            false
        }
    } else {
        false
    };

    if !enabled {
        return;
    }

    let home = env::var("HOME").unwrap_or_else(|_| "/tmp".to_string());
    let log_dir = PathBuf::from(&home)
        .join(".git-ai")
        .join("internal")
        .join("checkpoint-debug-logs");
    if let Err(e) = fs::create_dir_all(&log_dir) {
        debug_log(&format!("failed to create checkpoint debug log dir: {}", e));
        return;
    }

    // Generate today's date for the filename
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    let secs = now.as_secs();
    // Simple date calculation (days since epoch)
    let days = secs / 86400;
    let filename = format!("{}.jsonl", days);
    let log_file = log_dir.join(&filename);

    let trace_id = format!("trace-{}", now.as_nanos());
    let timestamp = format!("{}Z", secs);

    let entry = serde_json::json!({
        "preset_name": preset_name,
        "trace_id": trace_id,
        "timestamp": timestamp,
        "event_count": event_count,
        "requests": [],
    });

    use std::io::Write;
    let mut file = match fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_file)
    {
        Ok(f) => f,
        Err(e) => {
            debug_log(&format!("failed to open checkpoint debug log: {}", e));
            return;
        }
    };
    let _ = writeln!(file, "{}", entry);
}


/// Handle checkpoint for real agent presets (cursor, claude, agent-v1, etc.).
/// Reads hook payload from stdin or --hook-input arg, parses it, and processes the resulting events.
fn handle_agent_checkpoint(agent_name: &str, file_args: &[&str]) {
    use git_ai::presets::ParsedHookEvent;

    // Check if --hook-input is provided as a flag in file_args
    let hook_input = {
        let mut input: Option<String> = None;
        let mut i = 0;
        while i < file_args.len() {
            if file_args[i] == "--hook-input" {
                if i + 1 < file_args.len() {
                    let value = file_args[i + 1];
                    if value == "stdin" {
                        break; // fall through to read from stdin
                    }
                    input = Some(value.to_string());
                }
                break;
            }
            i += 1;
        }
        input.unwrap_or_else(git_ai::presets::read_stdin)
    };

    let events = match git_ai::presets::parse_hook_input(agent_name, &hook_input) {
        Ok(events) => events,
        Err(e) => {
            debug_log(&format!("preset parse error: {}", e));
            println!("0");
            return;
        }
    };

    let mut processed = 0;

    for event in events {
        let is_pre_file_edit = matches!(&event, ParsedHookEvent::PreFileEdit(_));
        let is_post_file_edit = matches!(&event, ParsedHookEvent::PostFileEdit(_));

        let (kind, cwd, file_paths, agent_id, dirty_files): CheckpointEventData = match event {
            ParsedHookEvent::PreFileEdit(e) => (
                CheckpointKind::Human,
                e.context.cwd,
                e.file_paths,
                None,
                e.dirty_files,
            ),
            ParsedHookEvent::PostFileEdit(e) => {
                let aid = AgentId {
                    tool: e.context.agent_tool.clone(),
                    id: e.context.agent_session_id.clone(),
                    model: e.context.agent_model.clone(),
                };
                (
                    CheckpointKind::AiAgent,
                    e.context.cwd,
                    e.file_paths,
                    Some(aid),
                    e.dirty_files,
                )
            }
            ParsedHookEvent::PreBashCall(e) => {
                (CheckpointKind::Human, e.context.cwd, vec![], None, None)
            }
            ParsedHookEvent::PostBashCall(e) => {
                let aid = AgentId {
                    tool: e.context.agent_tool.clone(),
                    id: e.context.agent_session_id.clone(),
                    model: e.context.agent_model.clone(),
                };
                (
                    CheckpointKind::AiAgent,
                    e.context.cwd,
                    vec![],
                    Some(aid),
                    None,
                )
            }
            ParsedHookEvent::KnownHumanEdit(e) => (
                CheckpointKind::KnownHuman,
                e.cwd,
                e.file_paths,
                None,
                e.dirty_files,
            ),
            ParsedHookEvent::UntrackedEdit(e) => {
                (CheckpointKind::Human, e.cwd, e.file_paths, None, None)
            }
        };

        // Filter out --hook-input and its value from file_args
        let actual_file_args: Vec<&str> = {
            let mut result = Vec::new();
            let mut skip_next = false;
            for arg in file_args {
                if skip_next {
                    skip_next = false;
                    continue;
                }
                if *arg == "--hook-input" {
                    skip_next = true;
                    continue;
                }
                result.push(*arg);
            }
            result
        };

        // If preset provided file paths, use those. Otherwise use file_args or scan.
        let raw_files: Vec<PathBuf> = if !file_paths.is_empty() {
            file_paths.clone()
        } else if !actual_file_args.is_empty() {
            actual_file_args
                .iter()
                .map(|f| {
                    let p = PathBuf::from(f);
                    if p.is_absolute() { p } else { cwd.join(f) }
                })
                .collect()
        } else {
            // For bash tools, scan for all modified files from CWD
            let status_output =
                git_cmd_in(&cwd, &["status", "--porcelain", "-u"]).unwrap_or_default();
            let cwd_repo_root = git_cmd_in(&cwd, &["rev-parse", "--show-toplevel"])
                .unwrap_or_else(|_| cwd.to_string_lossy().to_string());
            let cwd_root = PathBuf::from(&cwd_repo_root);
            status_output
                .lines()
                .filter(|l| l.len() > 3)
                .map(|l| cwd_root.join(l[3..].trim()))
                .filter(|p| p.exists())
                .collect()
        };

        // Check if files contain absolute paths that might belong to different repos
        let has_absolute = raw_files.iter().any(|f| f.is_absolute());

        if has_absolute {
            // Cross-repo mode: group files by their containing repository
            let mut repo_groups: HashMap<PathBuf, Vec<PathBuf>> = HashMap::new();
            for fp in &raw_files {
                if !fp.exists() {
                    continue;
                }
                let resolved = if fp.is_absolute() {
                    find_repo_root_for_path(fp)
                } else {
                    find_repo_root_for_path(&cwd.join(fp))
                };
                if let Some(repo_root) = resolved {
                    repo_groups.entry(repo_root).or_default().push(fp.clone());
                }
            }

            for (repo_root_path, files) in &repo_groups {
                let git_dir = match git_cmd_in(repo_root_path, &["rev-parse", "--git-dir"]) {
                    Ok(d) => {
                        let p = PathBuf::from(&d);
                        if p.is_relative() {
                            repo_root_path.join(p)
                        } else {
                            p
                        }
                    }
                    Err(_) => continue,
                };
                let base_commit = git_cmd_in(repo_root_path, &["rev-parse", "HEAD"])
                    .unwrap_or_else(|_| "initial".to_string());

                // For PreFileEdit events, register pending AI edit markers
                if is_pre_file_edit {
                    for fp in files {
                        let rel = fp
                            .strip_prefix(repo_root_path)
                            .unwrap_or(fp)
                            .to_string_lossy()
                            .replace('\\', "/");
                        write_pending_ai_edit(&git_dir, &rel);
                    }
                }

                // For PostFileEdit (AI) events, clear pending AI edit markers
                if is_post_file_edit {
                    for fp in files {
                        let rel = fp
                            .strip_prefix(repo_root_path)
                            .unwrap_or(fp)
                            .to_string_lossy()
                            .replace('\\', "/");
                        clear_pending_ai_edit(&git_dir, &rel);
                    }
                }

                for file_path in files {
                    // Allow processing even if file doesn't exist on disk
                    // when dirty_files provides content (e.g., create_file pre-edit with empty content)
                    let dirty_content = dirty_files.as_ref().and_then(|df| df.get(file_path));
                    if !file_path.exists() && dirty_content.is_none() {
                        continue;
                    }

                    let relative_path = file_path
                        .strip_prefix(repo_root_path)
                        .unwrap_or(file_path)
                        .to_string_lossy()
                        .replace('\\', "/");

                    // Suppression: skip KnownHuman checkpoints for files with a pending AI edit
                    if kind == CheckpointKind::KnownHuman
                        && has_pending_ai_edit(&git_dir, &relative_path)
                    {
                        debug_log(&format!(
                            "suppressing KnownHuman checkpoint for '{}' (pending AI edit)",
                            relative_path
                        ));
                        continue;
                    }

                    // Use dirty_files content if available, otherwise read from disk
                    let content = if let Some(dc) = dirty_content {
                        dc.clone()
                    } else {
                        match fs::read_to_string(file_path) {
                            Ok(c) => c,
                            Err(_) => continue,
                        }
                    };

                    let blob_sha = git_ai::core::working_log::save_blob(
                        &git_dir,
                        &base_commit,
                        content.as_bytes(),
                    );

                    let existing_checkpoints =
                        git_ai::core::working_log::read_checkpoints(&git_dir, &base_commit);
                    let previous_attributions =
                        find_latest_attributions(&existing_checkpoints, &relative_path);
                    let previous_content = find_latest_content(
                        &existing_checkpoints,
                        &relative_path,
                        &git_dir,
                        &base_commit,
                    );

                    // For KnownHuman, resolve the full git identity (Name <email>)
                    let known_human_identity = if kind == CheckpointKind::KnownHuman {
                        let name = git_cmd_in(repo_root_path, &["config", "user.name"])
                            .unwrap_or_else(|_| "Unknown".to_string());
                        let email = git_cmd_in(repo_root_path, &["config", "user.email"])
                            .unwrap_or_else(|_| "unknown".to_string());
                        Some(format!("{} <{}>", name, email))
                    } else {
                        None
                    };

                    let trace_value = if kind == CheckpointKind::AiAgent {
                        Some(format!(
                            "trace-{}",
                            SystemTime::now()
                                .duration_since(UNIX_EPOCH)
                                .map(|d| d.as_nanos())
                                .unwrap_or(0)
                        ))
                    } else {
                        None
                    };

                    let author_id = match (&kind, &agent_id) {
                        (CheckpointKind::AiAgent, Some(aid)) => {
                            let session_id = git_ai::core::authorship_log::generate_session_id(
                                &aid.tool, &aid.id,
                            );
                            let trace_hash = git_ai::core::authorship_log::generate_trace_hash(
                                trace_value.as_deref().unwrap(),
                            );
                            format!("{}::{}", session_id, trace_hash)
                        }
                        (CheckpointKind::KnownHuman, _) => {
                            git_ai::core::authorship_log::generate_human_hash(
                                known_human_identity.as_deref().unwrap(),
                            )
                        }
                        _ => "human".to_string(),
                    };

                    let enable_move_detection =
                        kind == CheckpointKind::Human || kind == CheckpointKind::KnownHuman;
                    let new_attributions = update_attributions(
                        &previous_content,
                        &content,
                        &previous_attributions,
                        &author_id,
                        enable_move_detection,
                    );
                    let line_attributions =
                        attributions_to_line_attributions(&content, &new_attributions);

                    let entry = WorkingLogEntry {
                        file: relative_path,
                        blob_sha,
                        attributions: new_attributions,
                        line_attributions,
                    };

                    let checkpoint_author = if let Some(ref aid) = agent_id {
                        aid.tool.clone()
                    } else if let Some(ref identity) = known_human_identity {
                        identity.clone()
                    } else {
                        agent_name.to_string()
                    };

                    let mut checkpoint = Checkpoint::new(kind, checkpoint_author, vec![entry]);
                    checkpoint.agent_id = agent_id.clone();
                    checkpoint.trace_id = trace_value;

                    git_ai::core::working_log::append_checkpoint(
                        &git_dir,
                        &base_commit,
                        &checkpoint,
                    );
                    processed += 1;
                }
            }
        } else {
            // Standard mode: all files relative to CWD repo
            let repo_root_path = cwd;
            let git_dir = match git_cmd_in(&repo_root_path, &["rev-parse", "--git-dir"]) {
                Ok(d) => {
                    let p = PathBuf::from(&d);
                    if p.is_relative() {
                        repo_root_path.join(p)
                    } else {
                        p
                    }
                }
                Err(_) => continue,
            };

            let base_commit = git_cmd_in(&repo_root_path, &["rev-parse", "HEAD"])
                .unwrap_or_else(|_| "initial".to_string());

            let files_to_process = &raw_files;

            // For PreFileEdit events, register pending AI edit markers
            if is_pre_file_edit {
                for fp in files_to_process {
                    let rel = fp
                        .strip_prefix(&repo_root_path)
                        .unwrap_or(fp)
                        .to_string_lossy()
                        .replace('\\', "/");
                    write_pending_ai_edit(&git_dir, &rel);
                }
            }

            // For PostFileEdit (AI) events, clear pending AI edit markers
            if is_post_file_edit {
                for fp in files_to_process {
                    let rel = fp
                        .strip_prefix(&repo_root_path)
                        .unwrap_or(fp)
                        .to_string_lossy()
                        .replace('\\', "/");
                    clear_pending_ai_edit(&git_dir, &rel);
                }
            }

            for file_path in files_to_process {
                // Allow processing even if file doesn't exist on disk
                // when dirty_files provides content (e.g., create_file pre-edit with empty content)
                let dirty_content = dirty_files.as_ref().and_then(|df| df.get(file_path));
                if !file_path.exists() && dirty_content.is_none() {
                    continue;
                }

                let relative_path = file_path
                    .strip_prefix(&repo_root_path)
                    .unwrap_or(file_path)
                    .to_string_lossy()
                    .replace('\\', "/");

                // Suppression: skip KnownHuman checkpoints for files with a pending AI edit
                if kind == CheckpointKind::KnownHuman
                    && has_pending_ai_edit(&git_dir, &relative_path)
                {
                    debug_log(&format!(
                        "suppressing KnownHuman checkpoint for '{}' (pending AI edit)",
                        relative_path
                    ));
                    continue;
                }

                // Use dirty_files content if available, otherwise read from disk
                let content = if let Some(dc) = dirty_content {
                    dc.clone()
                } else {
                    match fs::read_to_string(file_path) {
                        Ok(c) => c,
                        Err(_) => continue,
                    }
                };

                let blob_sha = git_ai::core::working_log::save_blob(
                    &git_dir,
                    &base_commit,
                    content.as_bytes(),
                );

                let existing_checkpoints =
                    git_ai::core::working_log::read_checkpoints(&git_dir, &base_commit);
                let previous_attributions =
                    find_latest_attributions(&existing_checkpoints, &relative_path);
                let previous_content = find_latest_content(
                    &existing_checkpoints,
                    &relative_path,
                    &git_dir,
                    &base_commit,
                );

                // For KnownHuman, resolve the full git identity (Name <email>)
                let known_human_identity = if kind == CheckpointKind::KnownHuman {
                    let name = git_cmd_in(&repo_root_path, &["config", "user.name"])
                        .unwrap_or_else(|_| "Unknown".to_string());
                    let email = git_cmd_in(&repo_root_path, &["config", "user.email"])
                        .unwrap_or_else(|_| "unknown".to_string());
                    Some(format!("{} <{}>", name, email))
                } else {
                    None
                };

                let trace_value = if kind == CheckpointKind::AiAgent {
                    Some(format!(
                        "trace-{}",
                        SystemTime::now()
                            .duration_since(UNIX_EPOCH)
                            .map(|d| d.as_nanos())
                            .unwrap_or(0)
                    ))
                } else {
                    None
                };

                let author_id = match (&kind, &agent_id) {
                    (CheckpointKind::AiAgent, Some(aid)) => {
                        let session_id =
                            git_ai::core::authorship_log::generate_session_id(&aid.tool, &aid.id);
                        let trace_hash = git_ai::core::authorship_log::generate_trace_hash(
                            trace_value.as_deref().unwrap(),
                        );
                        format!("{}::{}", session_id, trace_hash)
                    }
                    (CheckpointKind::KnownHuman, _) => {
                        git_ai::core::authorship_log::generate_human_hash(
                            known_human_identity.as_deref().unwrap(),
                        )
                    }
                    _ => "human".to_string(),
                };

                let enable_move_detection =
                    kind == CheckpointKind::Human || kind == CheckpointKind::KnownHuman;
                let new_attributions = update_attributions(
                    &previous_content,
                    &content,
                    &previous_attributions,
                    &author_id,
                    enable_move_detection,
                );
                let line_attributions =
                    attributions_to_line_attributions(&content, &new_attributions);

                let entry = WorkingLogEntry {
                    file: relative_path,
                    blob_sha,
                    attributions: new_attributions,
                    line_attributions,
                };

                let checkpoint_author = if let Some(ref aid) = agent_id {
                    aid.tool.clone()
                } else if let Some(ref identity) = known_human_identity {
                    identity.clone()
                } else {
                    agent_name.to_string()
                };

                let mut checkpoint = Checkpoint::new(kind, checkpoint_author, vec![entry]);
                checkpoint.agent_id = agent_id.clone();
                checkpoint.trace_id = trace_value;

                git_ai::core::working_log::append_checkpoint(&git_dir, &base_commit, &checkpoint);
                processed += 1;
            }
        }
    }

    println!("{}", processed);
}

// ---------------------------------------------------------------------------
// Pending AI edit markers
// ---------------------------------------------------------------------------

/// Directory for pending AI edit markers: .git/ai/pending_ai_edits/
fn pending_ai_edits_dir(git_dir: &Path) -> PathBuf {
    git_dir.join("ai").join("pending_ai_edits")
}

/// Convert a relative file path to a safe marker filename (replace / and \ with __)
fn marker_filename(relative_path: &str) -> String {
    relative_path.replace(['/', '\\'], "__")
}

/// Write a pending AI edit marker for the given file.
fn write_pending_ai_edit(git_dir: &Path, relative_path: &str) {
    let dir = pending_ai_edits_dir(git_dir);
    let _ = fs::create_dir_all(&dir);
    let marker_path = dir.join(marker_filename(relative_path));
    let _ = fs::write(&marker_path, "");
}

/// Check if a file has a pending AI edit marker.
fn has_pending_ai_edit(git_dir: &Path, relative_path: &str) -> bool {
    let marker_path = pending_ai_edits_dir(git_dir).join(marker_filename(relative_path));
    marker_path.exists()
}

/// Clear the pending AI edit marker for the given file.
fn clear_pending_ai_edit(git_dir: &Path, relative_path: &str) {
    let marker_path = pending_ai_edits_dir(git_dir).join(marker_filename(relative_path));
    let _ = fs::remove_file(&marker_path);
}

fn find_latest_attributions(checkpoints: &[Checkpoint], relative_path: &str) -> Vec<Attribution> {
    for cp in checkpoints.iter().rev() {
        for entry in &cp.entries {
            if entry.file == relative_path && !entry.attributions.is_empty() {
                return entry.attributions.clone();
            }
        }
    }
    Vec::new()
}

fn find_latest_content(
    checkpoints: &[Checkpoint],
    relative_path: &str,
    git_dir: &Path,
    base_commit: &str,
) -> String {
    for cp in checkpoints.iter().rev() {
        for entry in &cp.entries {
            if entry.file == relative_path
                && !entry.blob_sha.is_empty()
                && let Some(content) =
                    git_ai::core::working_log::read_blob(git_dir, base_commit, &entry.blob_sha)
            {
                return content;
            }
        }
    }

    if base_commit != "initial"
        && let Ok(content) = git_cmd(&["show", &format!("{}:{}", base_commit, relative_path)])
    {
        return content;
    }

    String::new()
}
