use std::env;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

pub fn debug_log(msg: &str) {
    if cfg!(debug_assertions) || env::var("GIT_AI_DEBUG").as_deref() == Ok("1") {
        eprintln!("[git-ai] {}", msg);
    }
}

pub fn git_cmd(args: &[&str]) -> Result<String, String> {
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

/// Run a git command from a specific working directory.
pub fn git_cmd_in(dir: &Path, args: &[&str]) -> Result<String, String> {
    let output = Command::new("/usr/bin/git")
        .args(args)
        .current_dir(dir)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .map_err(|e| format!("failed to run git: {}", e))?;

    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout)
            .trim_end()
            .to_string())
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        Err(format!("git {} failed: {}", args.join(" "), stderr))
    }
}

/// Given an absolute file path, find the git repository root that contains it.
/// Walks up from the file's parent directory looking for `.git/` (directory or file for worktrees).
pub fn find_repo_root_for_path(file_path: &Path) -> Option<PathBuf> {
    let start_dir = if file_path.is_dir() {
        file_path.to_path_buf()
    } else {
        file_path.parent()?.to_path_buf()
    };

    let mut current = start_dir.as_path();
    loop {
        let git_path = current.join(".git");
        if git_path.exists() {
            return Some(current.to_path_buf());
        }
        match current.parent() {
            Some(parent) => current = parent,
            None => return None,
        }
    }
}
