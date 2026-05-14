use std::fs;
use std::path::PathBuf;

use crate::commands::helpers::{debug_log, git_cmd};

pub fn handle_stash_save() {
    let git_dir_str = match git_cmd(&["rev-parse", "--git-dir"]) {
        Ok(d) => d,
        Err(_) => return,
    };
    let git_dir = PathBuf::from(&git_dir_str);
    let base_commit = git_cmd(&["rev-parse", "HEAD"]).unwrap_or_else(|_| "initial".to_string());

    // Save current working log state before stash
    let stash_dir = git_dir.join("ai").join("stash_backup");
    let working_log_dir = git_dir.join("ai").join("working_logs").join(&base_commit);

    if working_log_dir.exists() {
        let _ = fs::create_dir_all(&stash_dir);
        // Copy working log to stash backup
        if let Ok(entries) = fs::read_dir(&working_log_dir) {
            for entry in entries.flatten() {
                let dest = stash_dir.join(entry.file_name());
                let _ = fs::copy(entry.path(), dest);
            }
        }
        // Write the base commit SHA for later restoration
        let _ = fs::write(stash_dir.join("base_commit"), &base_commit);
    }
    debug_log("stash-save: preserved working log state");
}

pub fn handle_stash_restore() {
    let git_dir_str = match git_cmd(&["rev-parse", "--git-dir"]) {
        Ok(d) => d,
        Err(_) => return,
    };
    let git_dir = PathBuf::from(&git_dir_str);
    let current_head = git_cmd(&["rev-parse", "HEAD"]).unwrap_or_else(|_| "initial".to_string());

    let stash_dir = git_dir.join("ai").join("stash_backup");
    if !stash_dir.exists() {
        debug_log("stash-restore: no stash backup found");
        return;
    }

    // Restore working log to current HEAD's working_logs dir
    let working_log_dir = git_dir.join("ai").join("working_logs").join(&current_head);
    let _ = fs::create_dir_all(&working_log_dir);

    if let Ok(entries) = fs::read_dir(&stash_dir) {
        for entry in entries.flatten() {
            let name = entry.file_name();
            if name == "base_commit" {
                continue;
            }
            let dest = working_log_dir.join(&name);
            let _ = fs::copy(entry.path(), dest);
        }
    }

    // Strip h_ attributions for lines in files that are IDENTICAL to HEAD after stash pop.
    // Only when the working tree file equals the HEAD file (meaning the stash didn't
    // actually modify that file), the h_ entries are stale and should be removed.
    // If the file differs from HEAD (user has uncommitted changes), h_ attributions
    // are genuine and must be preserved to prevent gap-fill from claiming human lines.
    let repo_root = git_cmd(&["rev-parse", "--show-toplevel"]).unwrap_or_else(|_| ".".to_string());
    let repo_root_path = PathBuf::from(&repo_root);
    let checkpoints = git_ai::core::working_log::read_checkpoints(&git_dir, &current_head);
    if !checkpoints.is_empty() {
        let mut modified = false;
        let mut new_checkpoints = checkpoints.clone();
        for checkpoint in &mut new_checkpoints {
            for entry in &mut checkpoint.entries {
                if entry.line_attributions.is_empty() {
                    continue;
                }
                // Get HEAD content for this file
                let head_content = git_cmd(&["show", &format!("{}:{}", current_head, entry.file)])
                    .unwrap_or_default();
                if head_content.is_empty() {
                    continue;
                }

                // Get working tree content for this file
                let wt_path = repo_root_path.join(&entry.file);
                let wt_content = fs::read_to_string(&wt_path).unwrap_or_default();

                // Only strip h_ if the file is identical to HEAD (no uncommitted changes)
                if wt_content == head_content {
                    // File wasn't actually modified by the stash — h_ entries are stale
                    for attr in &mut entry.line_attributions {
                        if attr.author_id.starts_with("h_") {
                            attr.author_id = String::new();
                            modified = true;
                        }
                    }
                    entry.line_attributions.retain(|a| !a.author_id.is_empty());
                }
            }
        }
        if modified {
            // Rewrite the checkpoints file
            let checkpoints_path = working_log_dir.join("checkpoints.jsonl");
            let mut content = String::new();
            for cp in &new_checkpoints {
                if let Ok(json) = serde_json::to_string(cp) {
                    content.push_str(&json);
                    content.push('\n');
                }
            }
            let _ = fs::write(&checkpoints_path, &content);
            debug_log("stash-restore: stripped stale h_ attributions");
        }
    }

    // Clean up stash backup
    let _ = fs::remove_dir_all(&stash_dir);
    debug_log("stash-restore: restored working log state");
}

pub fn handle_stash_restore_ref(args: &[String]) {
    let stash_ref = args.first().map(|s| s.as_str()).unwrap_or("stash@{0}");
    debug_log(&format!("stash-restore-ref: {}", stash_ref));
    handle_stash_restore();
}
