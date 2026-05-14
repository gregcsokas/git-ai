#![allow(dead_code)]

use git_ai::authorship::authorship_log_serialization::AuthorshipLog;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;

use super::test_file::TestFile;

// ---------------------------------------------------------------------------
// Binary compilation (cached across all tests in the process)
// ---------------------------------------------------------------------------

static COMPILED_BINARY: OnceLock<PathBuf> = OnceLock::new();

fn compile_binary() -> PathBuf {
    if let Ok(override_path) = std::env::var("GIT_AI_TEST_BINARY_PATH") {
        let path = PathBuf::from(override_path);
        if path.is_file() {
            return path;
        }
        panic!(
            "GIT_AI_TEST_BINARY_PATH does not point to a file: {}",
            path.display()
        );
    }

    println!("Compiling git-ai binary for tests...");

    let manifest_dir = env!("CARGO_MANIFEST_DIR");
    let output = Command::new("cargo")
        .args(["build", "--bin", "git-ai", "--features", "test-support"])
        .current_dir(manifest_dir)
        .output()
        .expect("Failed to compile git-ai binary");

    if !output.status.success() {
        panic!(
            "Failed to compile git-ai:\nstdout: {}\nstderr: {}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }

    let target_dir = std::env::var("CARGO_TARGET_DIR").unwrap_or_else(|_| {
        PathBuf::from(manifest_dir)
            .join("target")
            .to_string_lossy()
            .into_owned()
    });
    #[cfg(windows)]
    let binary_path = PathBuf::from(&target_dir).join("debug/git-ai.exe");
    #[cfg(not(windows))]
    let binary_path = PathBuf::from(&target_dir).join("debug/git-ai");

    binary_path
}

pub fn get_binary_path() -> &'static PathBuf {
    COMPILED_BINARY.get_or_init(compile_binary)
}

// ---------------------------------------------------------------------------
// Real git executable discovery
// ---------------------------------------------------------------------------

fn find_real_git() -> &'static str {
    static REAL_GIT: OnceLock<String> = OnceLock::new();
    REAL_GIT.get_or_init(|| {
        let candidates: &[&str] = &[
            "/usr/bin/git",
            "/usr/local/bin/git",
            "/opt/homebrew/bin/git",
            "/bin/git",
        ];
        for c in candidates {
            if Path::new(c).is_file() {
                return c.to_string();
            }
        }
        "git".to_string()
    })
}

pub fn real_git_executable() -> &'static str {
    find_real_git()
}

// ---------------------------------------------------------------------------
// with_worktree_mode (no-op stub for reuse_tests_in_worktree macro)
// ---------------------------------------------------------------------------

pub fn with_worktree_mode<F, R>(f: F) -> R
where
    F: FnOnce() -> R,
{
    f()
}

// ---------------------------------------------------------------------------
// NewCommit
// ---------------------------------------------------------------------------

#[derive(Debug)]
pub struct NewCommit {
    pub authorship_log: AuthorshipLog,
    pub stdout: String,
    pub commit_sha: String,
}

impl NewCommit {
    pub fn print_authorship(&self) {
        println!("{}", self.authorship_log.serialize_to_string());
    }
}

// ---------------------------------------------------------------------------
// TestRepo
// ---------------------------------------------------------------------------

#[derive(Debug)]
pub struct TestRepo {
    path: PathBuf,
    _tempdir: tempfile::TempDir,
}

impl TestRepo {
    pub fn new() -> Self {
        let tempdir = tempfile::tempdir().expect("failed to create temp dir");
        let path = tempdir.path().to_path_buf();

        let git = real_git_executable();
        let p = path.to_str().unwrap();

        let output = Command::new(git)
            .args(["init", p])
            .env("GIT_TRACE2_EVENT", "/dev/null")
            .output()
            .expect("failed to init test repo");
        assert!(output.status.success(), "git init failed");

        for args in [
            vec!["-C", p, "config", "user.name", "Test User"],
            vec!["-C", p, "config", "user.email", "test@example.com"],
            vec!["-C", p, "symbolic-ref", "HEAD", "refs/heads/main"],
        ] {
            let output = Command::new(git)
                .args(&args)
                .env("GIT_TRACE2_EVENT", "/dev/null")
                .output()
                .expect("failed to configure test repo");
            assert!(output.status.success(), "git config failed: {:?}", args);
        }

        Self {
            path,
            _tempdir: tempdir,
        }
    }

    pub fn path(&self) -> &PathBuf {
        &self.path
    }

    /// Run a real git command directly (no proxy, no wrapper).
    /// If the command is a rebase, automatically handles post-rewrite note copying.
    pub fn git(&self, args: &[&str]) -> Result<String, String> {
        let git = real_git_executable();

        // For rebase commands, capture the old commit SHAs before the rebase
        let is_rebase = args.first().map(|a| *a == "rebase").unwrap_or(false);
        let pre_rebase_commits = if is_rebase {
            self.collect_branch_commits()
        } else {
            Vec::new()
        };

        let mut command = Command::new(git);
        command
            .current_dir(&self.path)
            .args(args)
            // Suppress git trace2 events so the system-installed git-ai daemon
            // does not detect test commits and race to generate its own notes.
            .env("GIT_TRACE2_EVENT", "/dev/null");

        let output = command
            .output()
            .unwrap_or_else(|_| panic!("Failed to execute git command: {:?}", args));

        let stdout = String::from_utf8_lossy(&output.stdout).to_string();
        let stderr = String::from_utf8_lossy(&output.stderr).to_string();

        if output.status.success() {
            // After a successful rebase, copy authorship notes to new commits
            if is_rebase {
                self.handle_post_rewrite(&pre_rebase_commits);
            }
            Ok(if stdout.is_empty() { stderr } else { stdout })
        } else {
            Err(stderr)
        }
    }

    /// Collect commit SHAs on the current branch (newest first).
    fn collect_branch_commits(&self) -> Vec<String> {
        let git = real_git_executable();
        let output = Command::new(git)
            .current_dir(&self.path)
            .args(["log", "--format=%H", "--all"])
            .env("GIT_TRACE2_EVENT", "/dev/null")
            .output()
            .unwrap_or_else(|_| panic!("Failed to get commit log"));

        String::from_utf8_lossy(&output.stdout)
            .lines()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect()
    }

    /// After a rebase, find old->new commit mappings and copy authorship notes.
    fn handle_post_rewrite(&self, _pre_rebase_commits: &[String]) {
        let git = real_git_executable();

        // Get all commits that have authorship notes
        let notes_output = Command::new(git)
            .current_dir(&self.path)
            .args(["notes", "--ref=ai", "list"])
            .env("GIT_TRACE2_EVENT", "/dev/null")
            .output();

        let noted_commits: Vec<String> = match notes_output {
            Ok(o) if o.status.success() => {
                String::from_utf8_lossy(&o.stdout)
                    .lines()
                    .filter_map(|line| {
                        // Format: "<note-blob-sha> <commit-sha>"
                        let parts: Vec<&str> = line.split_whitespace().collect();
                        if parts.len() >= 2 {
                            Some(parts[1].to_string())
                        } else {
                            None
                        }
                    })
                    .collect()
            }
            _ => return,
        };

        // Get the current set of commits
        let post_rebase_commits: Vec<String> = {
            let output = Command::new(git)
                .current_dir(&self.path)
                .args(["log", "--format=%H", "--all"])
                .env("GIT_TRACE2_EVENT", "/dev/null")
                .output()
                .unwrap();
            String::from_utf8_lossy(&output.stdout)
                .lines()
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect()
        };

        // Find old commits that have notes but are no longer reachable
        let new_commit_set: std::collections::HashSet<&String> = post_rebase_commits.iter().collect();
        let old_with_notes: Vec<&String> = noted_commits
            .iter()
            .filter(|c| !new_commit_set.contains(c))
            .collect();

        if old_with_notes.is_empty() {
            return;
        }

        // For each old commit with a note, find its corresponding new commit
        // by matching commit messages (patch-id would be better but message matching works)
        for old_sha in old_with_notes {
            let old_msg = match Command::new(git)
                .current_dir(&self.path)
                .args(["log", "-1", "--format=%s", old_sha])
                .env("GIT_TRACE2_EVENT", "/dev/null")
                .output()
            {
                Ok(o) if o.status.success() => {
                    String::from_utf8_lossy(&o.stdout).trim().to_string()
                }
                _ => continue,
            };

            // Find new commit with same message that doesn't already have a note
            let new_sha = post_rebase_commits.iter().find(|new_c| {
                if noted_commits.contains(new_c) && old_sha != *new_c {
                    return false; // already has its own note
                }
                if *new_c == old_sha {
                    return false;
                }
                let msg = Command::new(git)
                    .current_dir(&self.path)
                    .args(["log", "-1", "--format=%s", new_c.as_str()])
                    .env("GIT_TRACE2_EVENT", "/dev/null")
                    .output()
                    .ok()
                    .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
                    .unwrap_or_default();
                msg == old_msg
            });

            if let Some(new_sha) = new_sha {
                // Use git-ai post-rewrite to copy the note
                let binary_path = get_binary_path();
                let _ = Command::new(binary_path)
                    .current_dir(&self.path)
                    .args(["post-rewrite", old_sha, new_sha])
                    .env("GIT_TRACE2_EVENT", "/dev/null")
                    .output();
            }
        }
    }

    /// Run a git-ai subcommand.
    pub fn git_ai(&self, args: &[&str]) -> Result<String, String> {
        let binary_path = get_binary_path();
        let mut command = Command::new(binary_path);
        command
            .args(args)
            .current_dir(&self.path)
            // Suppress git trace2 events so the system daemon doesn't interfere.
            .env("GIT_TRACE2_EVENT", "/dev/null");

        let output = command
            .output()
            .unwrap_or_else(|_| panic!("Failed to execute git-ai command: {:?}", args));

        let stdout = String::from_utf8_lossy(&output.stdout).to_string();
        let stderr = String::from_utf8_lossy(&output.stderr).to_string();

        if output.status.success() {
            let combined = if stdout.is_empty() {
                stderr
            } else if stderr.is_empty() {
                stdout
            } else {
                format!("{}{}", stdout, stderr)
            };
            Ok(combined)
        } else {
            let combined = if stdout.is_empty() {
                stderr
            } else if stderr.is_empty() {
                stdout
            } else {
                format!("{}{}", stderr, stdout)
            };
            Err(combined)
        }
    }

    /// Alias kept for compatibility with tests that call git_og.
    pub fn git_og(&self, args: &[&str]) -> Result<String, String> {
        self.git(args)
    }

    /// Stage all files and commit, then run post-commit to generate authorship.
    pub fn stage_all_and_commit(&self, message: &str) -> Result<NewCommit, String> {
        self.git(&["add", "-A"]).expect("add --all should succeed");
        self.commit(message)
    }

    /// Commit (no add), then run git-ai post-commit to generate the authorship note.
    pub fn commit(&self, message: &str) -> Result<NewCommit, String> {
        let output = self.git(&["commit", "-m", message]);

        match output {
            Ok(stdout) => {
                // Run git-ai post-commit to generate authorship note
                let post_commit_result = self.git_ai(&["post-commit"]);
                match &post_commit_result {
                    Ok(output) => eprintln!("[test] git-ai post-commit OK: {}", output),
                    Err(e) => eprintln!("[test] git-ai post-commit warning: {}", e),
                }

                // Get HEAD commit SHA
                let head_sha = self
                    .git(&["rev-parse", "HEAD"])
                    .map_err(|e| format!("Failed to get HEAD: {}", e))?
                    .trim()
                    .to_string();

                // Read the authorship note
                let note_content = self.read_authorship_note(&head_sha).ok_or_else(|| {
                    format!("No authorship log found for commit {}", &head_sha[..7])
                })?;

                let authorship_log = AuthorshipLog::deserialize_from_string(&note_content)
                    .map_err(|e| format!("Failed to parse authorship log: {}", e))?;

                Ok(NewCommit {
                    commit_sha: head_sha,
                    authorship_log,
                    stdout,
                })
            }
            Err(e) => Err(e),
        }
    }

    /// Create a TestFile handle for the given filename.
    pub fn filename(&self, filename: &str) -> TestFile<'_> {
        let file_path = self.path.join(filename);

        if file_path.exists() {
            TestFile::from_existing_file(file_path, self)
        } else {
            TestFile::new_with_filename(file_path, vec![], self)
        }
    }

    /// Read the authorship note for a given commit SHA.
    pub fn read_authorship_note(&self, commit_sha: &str) -> Option<String> {
        self.git(&["notes", "--ref=ai", "show", commit_sha])
            .ok()
            .filter(|note| !note.trim().is_empty())
    }
}
