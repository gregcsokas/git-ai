use git_ai::core::attribution::{
    attributions_to_line_attributions, update_attributions, Attribution,
};
use git_ai::core::authorship_log::AuthorshipLog;
use git_ai::core::post_commit::generate_authorship_for_commit;
use git_ai::core::working_log::{
    AgentId, Checkpoint, CheckpointKind, WorkingLogEntry,
};

use std::collections::HashMap;
use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{self, Command, Stdio};
use std::time::{SystemTime, UNIX_EPOCH};

fn debug_log(msg: &str) {
    if cfg!(debug_assertions) || env::var("GIT_AI_DEBUG").as_deref() == Ok("1") {
        eprintln!("[git-ai] {}", msg);
    }
}

fn git_cmd(args: &[&str]) -> Result<String, String> {
    let output = Command::new("/usr/bin/git")
        .args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .map_err(|e| format!("failed to run git: {}", e))?;

    if output.status.success() {
        // Use trim_end (not trim) to preserve leading whitespace in porcelain output
        Ok(String::from_utf8_lossy(&output.stdout)
            .trim_end()
            .to_string())
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        Err(format!("git {} failed: {}", args.join(" "), stderr))
    }
}

// ---------------------------------------------------------------------------
// Checkpoint command
// ---------------------------------------------------------------------------

fn handle_checkpoint(args: &[String]) {
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
        } else if kind_str.is_none()
            && matches!(arg, "human" | "mock_ai" | "mock_known_human")
        {
            kind_str = Some(arg);
        } else {
            file_args.push(arg);
        }
        i += 1;
    }

    let kind = match kind_str {
        Some("mock_ai") => CheckpointKind::AiAgent,
        Some("mock_known_human") => CheckpointKind::KnownHuman,
        Some("human") | None => CheckpointKind::Human,
        _ => CheckpointKind::Human,
    };

    let git_dir_str = match git_cmd(&["rev-parse", "--git-dir"]) {
        Ok(d) => d,
        Err(e) => {
            eprintln!("git-ai: {}", e);
            process::exit(1);
        }
    };
    let git_dir = PathBuf::from(&git_dir_str);

    let base_commit = git_cmd(&["rev-parse", "HEAD"]).unwrap_or_else(|_| "initial".to_string());

    let repo_root = git_cmd(&["rev-parse", "--show-toplevel"]).unwrap_or_else(|_| ".".to_string());
    let repo_root_path = PathBuf::from(&repo_root);

    let files_to_process: Vec<PathBuf> = if file_args.is_empty() {
        let status_output = git_cmd(&["status", "--porcelain", "-u"]).unwrap_or_default();
        status_output
            .lines()
            .filter(|l| !l.is_empty())
            .filter_map(|l| {
                if l.len() > 3 {
                    Some(repo_root_path.join(l[3..].trim()))
                } else {
                    None
                }
            })
            .filter(|p| p.exists())
            .collect()
    } else {
        file_args
            .iter()
            .map(|f| {
                let p = PathBuf::from(f);
                if p.is_absolute() {
                    p
                } else {
                    repo_root_path.join(f)
                }
            })
            .filter(|p| p.exists())
            .collect()
    };

    let mut processed = 0;

    for file_path in &files_to_process {
        let content = match fs::read_to_string(file_path) {
            Ok(c) => c,
            Err(_) => continue,
        };

        let blob_sha =
            git_ai::core::working_log::save_blob(&git_dir, &base_commit, content.as_bytes());

        let relative_path = file_path
            .strip_prefix(&repo_root_path)
            .unwrap_or(file_path)
            .to_string_lossy()
            .replace('\\', "/");

        let existing_checkpoints =
            git_ai::core::working_log::read_checkpoints(&git_dir, &base_commit);
        let previous_attributions = find_latest_attributions(&existing_checkpoints, &relative_path);

        let previous_content =
            find_latest_content(&existing_checkpoints, &relative_path, &git_dir, &base_commit);

        let checkpoint_agent_id = if kind == CheckpointKind::AiAgent {
            let ts = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0);
            Some(AgentId {
                tool: kind_str.unwrap_or("mock_ai").to_string(),
                id: format!("ai-thread-{}", ts),
                model: "unknown".to_string(),
            })
        } else {
            None
        };

        // For KnownHuman, resolve the git user identity for both the author_id hash
        // and the checkpoint.author field — they must be consistent.
        let known_human_identity = if kind == CheckpointKind::KnownHuman {
            let name = git_cmd(&["config", "user.name"]).unwrap_or_else(|_| "Unknown".to_string());
            let email = git_cmd(&["config", "user.email"]).unwrap_or_else(|_| "unknown".to_string());
            Some(format!("{} <{}>", name, email))
        } else {
            None
        };

        let author_id = match &kind {
            CheckpointKind::AiAgent => {
                let aid = checkpoint_agent_id.as_ref().unwrap();
                git_ai::core::authorship_log::generate_session_id(&aid.tool, &aid.id)
            }
            CheckpointKind::KnownHuman => {
                git_ai::core::authorship_log::generate_human_hash(
                    known_human_identity.as_deref().unwrap(),
                )
            }
            CheckpointKind::Human => "human".to_string(),
        };
        let enable_move_detection = kind == CheckpointKind::Human || kind == CheckpointKind::KnownHuman;
        let new_attributions =
            update_attributions(&previous_content, &content, &previous_attributions, &author_id, enable_move_detection);

        let line_attributions = attributions_to_line_attributions(&content, &new_attributions);

        let entry = WorkingLogEntry {
            file: relative_path.clone(),
            blob_sha,
            attributions: new_attributions,
            line_attributions,
        };

        let checkpoint_author = if let Some(ref identity) = known_human_identity {
            identity.clone()
        } else {
            kind_str.unwrap_or("human").to_string()
        };

        let mut checkpoint =
            Checkpoint::new(kind, checkpoint_author, vec![entry]);
        checkpoint.agent_id = checkpoint_agent_id.clone();
        if kind == CheckpointKind::AiAgent {
            checkpoint.trace_id = Some(format!("trace-{}", SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)));
        }

        git_ai::core::working_log::append_checkpoint(&git_dir, &base_commit, &checkpoint);
        processed += 1;
    }

    println!("{}", processed);
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
            if entry.file == relative_path && !entry.blob_sha.is_empty() {
                if let Some(content) =
                    git_ai::core::working_log::read_blob(git_dir, base_commit, &entry.blob_sha)
                {
                    return content;
                }
            }
        }
    }

    if base_commit != "initial" {
        if let Ok(content) = git_cmd(&["show", &format!("{}:{}", base_commit, relative_path)]) {
            return content;
        }
    }

    String::new()
}

// ---------------------------------------------------------------------------
// Post-commit command (called by .git/hooks/post-commit or explicitly)
// ---------------------------------------------------------------------------

fn handle_post_commit() {
    let git_dir_str = match git_cmd(&["rev-parse", "--git-dir"]) {
        Ok(d) => d,
        Err(_) => return,
    };
    let git_dir = std::fs::canonicalize(&git_dir_str).unwrap_or_else(|_| PathBuf::from(&git_dir_str));

    let commit_sha = match git_cmd(&["rev-parse", "HEAD"]) {
        Ok(s) => s,
        Err(_) => return,
    };

    let parent_sha = git_cmd(&["rev-parse", "HEAD~1"]).ok();
    let base_commit = parent_sha.as_deref().unwrap_or("initial");

    let repo_dir = git_cmd(&["rev-parse", "--show-toplevel"]).map(PathBuf::from).unwrap_or_else(|_| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));

    let human_author = git_cmd(&["log", "-1", "--format=%aN <%aE>"])
        .unwrap_or_else(|_| "Unknown <unknown>".to_string());


    let (authorship_log, initial_attrs) = match generate_authorship_for_commit(
        &git_dir,
        &repo_dir,
        base_commit,
        &commit_sha,
        &human_author,
    ) {
        Ok(result) => result,
        Err(_) => return,
    };


    let note_text = authorship_log.serialize_to_string();
    let result = Command::new("/usr/bin/git")
        .args([
            "notes", "--ref=ai", "add", "-f", "-m", &note_text, &commit_sha,
        ])
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .status();

    match result {
        Ok(status) if status.success() => {
            debug_log(&format!(
                "wrote authorship note for {}",
                &commit_sha[..7.min(commit_sha.len())]
            ));
        }
        Ok(_) => debug_log("git notes add failed"),
        Err(e) => debug_log(&format!("failed to run git notes: {}", e)),
    }

    if let Some(initial) = initial_attrs {
        git_ai::core::working_log::write_initial_attributions(&git_dir, &commit_sha, &initial);
    }

    git_ai::core::working_log::delete_working_log(&git_dir, base_commit);
}

// ---------------------------------------------------------------------------
// Blame command
// ---------------------------------------------------------------------------

fn handle_blame(args: &[String]) {
    if args.is_empty() {
        eprintln!("usage: git-ai blame <file>");
        process::exit(1);
    }

    let file_path = &args[0];

    let blame_output = match git_cmd(&["blame", "--line-porcelain", "--", file_path]) {
        Ok(o) => o,
        Err(e) => {
            eprintln!("git-ai blame: {}", e);
            process::exit(1);
        }
    };

    struct BlameLine {
        commit_sha: String,
        orig_line: u32,
        final_line: u32,
        author: String,
        author_time: i64,
        author_tz: String,
        content: String,
    }

    let mut lines: Vec<BlameLine> = Vec::new();
    let mut cur_sha = String::new();
    let mut cur_orig_line: u32 = 0;
    let mut cur_final_line: u32 = 0;
    let mut cur_author = String::new();
    let mut cur_author_time: i64 = 0;
    let mut cur_author_tz = String::new();

    for line in blame_output.lines() {
        if line.is_empty() {
            continue;
        }
        if line.starts_with('\t') {
            lines.push(BlameLine {
                commit_sha: cur_sha.clone(),
                orig_line: cur_orig_line,
                final_line: cur_final_line,
                author: cur_author.clone(),
                author_time: cur_author_time,
                author_tz: cur_author_tz.clone(),
                content: line[1..].to_string(),
            });
            continue;
        }
        if let Some(rest) = line.strip_prefix("author ") {
            cur_author = rest.to_string();
            continue;
        }
        if let Some(rest) = line.strip_prefix("author-time ") {
            cur_author_time = rest.trim().parse().unwrap_or(0);
            continue;
        }
        if let Some(rest) = line.strip_prefix("author-tz ") {
            cur_author_tz = rest.trim().to_string();
            continue;
        }
        let parts: Vec<&str> = line.split_whitespace().collect();
        if parts.len() >= 3
            && parts[0].len() == 40
            && parts[0].chars().all(|c| c.is_ascii_hexdigit())
        {
            cur_sha = parts[0].to_string();
            cur_orig_line = parts[1].parse().unwrap_or(0);
            cur_final_line = parts[2].parse().unwrap_or(0);
        }
    }

    let mut commit_notes: HashMap<String, Option<AuthorshipLog>> = HashMap::new();
    for blame_line in &lines {
        if !commit_notes.contains_key(&blame_line.commit_sha) {
            let note = load_authorship_note(&blame_line.commit_sha);
            commit_notes.insert(blame_line.commit_sha.clone(), note);
        }
    }

    let line_num_width = lines.len().to_string().len();
    let mut max_author_width = 0;
    for blame_line in &lines {
        let author = resolve_line_author(
            &blame_line.commit_sha,
            blame_line.orig_line,
            &blame_line.author,
            file_path,
            &commit_notes,
        );
        max_author_width = max_author_width.max(author.len());
    }

    for blame_line in &lines {
        let short_sha = &blame_line.commit_sha[..7.min(blame_line.commit_sha.len())];
        let display_author = resolve_line_author(
            &blame_line.commit_sha,
            blame_line.orig_line,
            &blame_line.author,
            file_path,
            &commit_notes,
        );
        let date_str = format_blame_date(blame_line.author_time, &blame_line.author_tz);

        println!(
            "{} ({:<width$} {} {:>lwidth$}) {}",
            short_sha,
            display_author,
            date_str,
            blame_line.final_line,
            blame_line.content,
            width = max_author_width,
            lwidth = line_num_width,
        );
    }
}

fn resolve_line_author(
    commit_sha: &str,
    orig_line: u32,
    git_author: &str,
    file_path: &str,
    commit_notes: &HashMap<String, Option<AuthorshipLog>>,
) -> String {
    if let Some(Some(authorship_log)) = commit_notes.get(commit_sha) {
        for file_attest in &authorship_log.attestations {
            let attest_path = file_attest
                .file_path
                .strip_prefix("./")
                .unwrap_or(&file_attest.file_path);
            let query_path = file_path.strip_prefix("./").unwrap_or(file_path);
            if attest_path != query_path {
                continue;
            }

            for entry in &file_attest.entries {
                let covers_line = entry.line_ranges.iter().any(|r| r.contains(orig_line));
                if !covers_line {
                    continue;
                }

                if let Some(prompt) = authorship_log.metadata.prompts.get(&entry.hash) {
                    return prompt.agent_id.tool.clone();
                }
                if entry.hash.starts_with("h_") {
                    return git_author.to_string();
                }
                if entry.hash.starts_with("s_") {
                    if let Some(session) = authorship_log.metadata.sessions.get(&entry.hash) {
                        return session.agent_id.tool.clone();
                    }
                }
            }
        }
    }
    git_author.to_string()
}

fn load_authorship_note(commit_sha: &str) -> Option<AuthorshipLog> {
    let note_content = git_cmd(&["notes", "--ref=ai", "show", commit_sha]).ok()?;
    AuthorshipLog::deserialize_from_string(&note_content).ok()
}

fn format_blame_date(author_time: i64, author_tz: &str) -> String {
    let offset_secs: i64 = if author_tz.len() == 5 {
        let sign: i64 = if author_tz.starts_with('+') { 1 } else { -1 };
        let hours: i64 = author_tz[1..3].parse().unwrap_or(0);
        let mins: i64 = author_tz[3..5].parse().unwrap_or(0);
        sign * (hours * 3600 + mins * 60)
    } else {
        0
    };

    let local_time = author_time + offset_secs;
    let days_since_epoch = local_time.div_euclid(86400);
    let time_of_day = local_time.rem_euclid(86400);

    let hours = time_of_day / 3600;
    let mins = (time_of_day % 3600) / 60;
    let secs = time_of_day % 60;

    let (year, month, day) = days_to_ymd(days_since_epoch);

    format!(
        "{:04}-{:02}-{:02} {:02}:{:02}:{:02} {}",
        year, month, day, hours, mins, secs, author_tz
    )
}

fn days_to_ymd(days: i64) -> (i64, u32, u32) {
    let z = days + 719468;
    let era = z.div_euclid(146097);
    let doe = z.rem_euclid(146097) as u32;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    (y, m, d)
}

// ---------------------------------------------------------------------------
// Install command
// ---------------------------------------------------------------------------

fn handle_install() {
    let git_dir_str = match git_cmd(&["rev-parse", "--git-dir"]) {
        Ok(d) => d,
        Err(e) => {
            eprintln!("git-ai install: not in a git repository: {}", e);
            process::exit(1);
        }
    };

    let hooks_dir = PathBuf::from(&git_dir_str).join("hooks");
    fs::create_dir_all(&hooks_dir).unwrap_or_else(|e| {
        eprintln!("git-ai install: failed to create hooks dir: {}", e);
        process::exit(1);
    });

    let hook_path = hooks_dir.join("post-commit");
    let hook_content = "#!/bin/sh\ngit-ai post-commit\n";
    fs::write(&hook_path, hook_content).unwrap_or_else(|e| {
        eprintln!("git-ai install: failed to write hook: {}", e);
        process::exit(1);
    });

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&hook_path, fs::Permissions::from_mode(0o755)).unwrap_or_else(|e| {
            eprintln!("git-ai install: failed to chmod hook: {}", e);
            process::exit(1);
        });
    }

    println!("git-ai: installed post-commit hook");
}

// ---------------------------------------------------------------------------
// Status command (stub)
// ---------------------------------------------------------------------------

fn handle_status(args: &[String]) {
    if args.iter().any(|a| a == "--json") {
        println!("{{}}");
    } else {
        println!("No uncommitted attributions.");
    }
}

// ---------------------------------------------------------------------------
// Stats command
// ---------------------------------------------------------------------------

fn handle_stats(args: &[String]) {
    let is_json = args.iter().any(|a| a == "--json");
    let commit_ref = args.iter().find(|a| !a.starts_with('-')).map(|s| s.as_str()).unwrap_or("HEAD");

    let commit_sha = match git_cmd(&["rev-parse", commit_ref]) {
        Ok(s) => s,
        Err(_) => {
            if is_json {
                println!("{{}}");
            } else {
                println!("No stats available.");
            }
            return;
        }
    };

    let note = match git_cmd(&["notes", "--ref=ai", "show", &commit_sha]) {
        Ok(n) => n,
        Err(_) => {
            if is_json {
                println!("{{}}");
            } else {
                println!("No stats available.");
            }
            return;
        }
    };

    let log = match git_ai::core::authorship_log::AuthorshipLog::deserialize_from_string(&note) {
        Ok(l) => l,
        Err(_) => {
            if is_json {
                println!("{{}}");
            } else {
                println!("No stats available.");
            }
            return;
        }
    };

    let mut ai_additions: u64 = 0;
    let mut human_additions: u64 = 0;

    for file_att in &log.attestations {
        for entry in &file_att.entries {
            let count: u64 = entry.line_ranges.iter().map(|r| r.line_count() as u64).sum();
            if entry.hash.starts_with("h_") {
                human_additions += count;
            } else {
                ai_additions += count;
            }
        }
    }

    if is_json {
        println!(
            "{{\"ai_additions\":{},\"human_additions\":{},\"files\":{{\"total\":{{}}}}}}",
            ai_additions, human_additions
        );
    } else {
        println!("AI additions: {}", ai_additions);
        println!("Human additions: {}", human_additions);
    }
}

// ---------------------------------------------------------------------------
// Post-rewrite command (called after rebase/amend to copy authorship notes)
// ---------------------------------------------------------------------------

fn handle_post_rewrite(args: &[String]) {
    // The post-rewrite hook receives old-sha new-sha pairs on stdin.
    // If --stdin is passed, read from stdin. Otherwise, try to infer from reflog.
    let use_stdin = args.iter().any(|a| a == "--stdin");

    let mappings: Vec<(String, String)> = if use_stdin {
        use std::io::BufRead;
        std::io::stdin()
            .lock()
            .lines()
            .filter_map(|line| {
                let line = line.ok()?;
                let parts: Vec<&str> = line.split_whitespace().collect();
                if parts.len() >= 2 {
                    Some((parts[0].to_string(), parts[1].to_string()))
                } else {
                    None
                }
            })
            .collect()
    } else if args.len() >= 2 {
        // Direct old-sha new-sha pairs as arguments
        let mut pairs = Vec::new();
        let mut i = 0;
        let filtered: Vec<&String> = args.iter().filter(|a| *a != "rebase" && *a != "amend").collect();
        while i + 1 < filtered.len() {
            pairs.push((filtered[i].clone(), filtered[i + 1].clone()));
            i += 2;
        }
        pairs
    } else {
        Vec::new()
    };

    for (old_sha, new_sha) in &mappings {
        // Try to read the authorship note from the old commit
        let note = match git_cmd(&["notes", "--ref=ai", "show", old_sha]) {
            Ok(n) => n,
            Err(_) => continue,
        };

        if note.trim().is_empty() {
            continue;
        }

        // Update the base_commit_sha in the note metadata to point to the new commit
        let updated_note = if let Ok(mut log) = AuthorshipLog::deserialize_from_string(&note) {
            log.metadata.base_commit_sha = new_sha.clone();
            log.serialize_to_string()
        } else {
            note
        };

        // Write the note to the new commit
        let result = Command::new("/usr/bin/git")
            .args([
                "notes", "--ref=ai", "add", "-f", "-m", &updated_note, new_sha,
            ])
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .status();

        match result {
            Ok(status) if status.success() => {
                debug_log(&format!(
                    "copied authorship note {} -> {}",
                    &old_sha[..7.min(old_sha.len())],
                    &new_sha[..7.min(new_sha.len())]
                ));
            }
            _ => {
                debug_log(&format!(
                    "failed to copy note from {} to {}",
                    &old_sha[..7.min(old_sha.len())],
                    &new_sha[..7.min(new_sha.len())]
                ));
            }
        }
    }

    if mappings.is_empty() {
        debug_log("post-rewrite: no mappings provided");
    }
}

// ---------------------------------------------------------------------------
// Entry point — git-ai is ONLY git-ai, never a git proxy/wrapper
// ---------------------------------------------------------------------------

fn main() {
    let args: Vec<String> = env::args().skip(1).collect();

    match args.first().map(String::as_str) {
        Some("checkpoint") => handle_checkpoint(&args[1..]),
        Some("post-commit") => handle_post_commit(),
        Some("post-rewrite") => handle_post_rewrite(&args[1..]),
        Some("blame") => handle_blame(&args[1..]),
        Some("install") => handle_install(),
        Some("status") => handle_status(&args[1..]),
        Some("stats") => handle_stats(&args[1..]),
        Some("--version") | Some("-v") | Some("version") => {
            println!("git-ai {}", env!("CARGO_PKG_VERSION"));
        }
        Some("--help") | Some("-h") | Some("help") | None => {
            println!("usage: git-ai <command> [<args>]");
            println!();
            println!("Commands:");
            println!("  checkpoint    Record attribution checkpoint");
            println!("  post-commit   Generate authorship note for HEAD commit");
            println!("  post-rewrite  Copy authorship notes after rebase/amend");
            println!("  blame         Show blame with AI/human attribution");
            println!("  install       Install git hooks for automatic attribution");
            println!("  status        Show uncommitted attribution status");
            println!("  stats         Show commit attribution stats");
        }
        Some(cmd) => {
            eprintln!("git-ai: unknown command '{}'", cmd);
            process::exit(1);
        }
    }
}
