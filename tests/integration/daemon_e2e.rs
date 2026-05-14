use std::fs;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use crate::repos::test_repo::{get_binary_path, real_git_executable};

/// Wait for a file to exist, with timeout.
fn wait_for_file(path: &PathBuf, timeout: Duration) -> bool {
    let start = Instant::now();
    while start.elapsed() < timeout {
        if path.exists() {
            return true;
        }
        thread::sleep(Duration::from_millis(50));
    }
    false
}

/// Create a HOME directory with a .gitconfig that routes trace2 events to the socket.
/// Returns the HOME path. Git commands run with this HOME will send trace2 events.
fn create_trace2_home(base_dir: &std::path::Path, socket_path: &PathBuf) -> PathBuf {
    let home = base_dir.join("trace2home");
    fs::create_dir_all(&home).unwrap();
    let gitconfig = home.join(".gitconfig");
    let content = format!(
        "[trace2]\n\teventTarget = af_unix:stream:{}\n\teventNesting = 10\n",
        socket_path.display()
    );
    fs::write(&gitconfig, content).unwrap();
    home
}

/// Wait for the daemon to write an authorship note on a commit.
fn wait_for_note(repo_path: &PathBuf, commit_sha: &str, timeout: Duration) -> Option<String> {
    let git = real_git_executable();
    let start = Instant::now();
    while start.elapsed() < timeout {
        let output = Command::new(git)
            .current_dir(repo_path)
            .args(["notes", "--ref=ai", "show", commit_sha])
            .env("GIT_TRACE2_EVENT", "/dev/null")
            .output()
            .ok()?;

        if output.status.success() {
            let note = String::from_utf8_lossy(&output.stdout).to_string();
            if !note.trim().is_empty() {
                return Some(note);
            }
        }
        thread::sleep(Duration::from_millis(100));
    }
    None
}

#[test]
fn test_daemon_detects_commit_and_writes_note() {
    let binary = get_binary_path();
    let git = real_git_executable();

    // Create an isolated daemon home directory
    let daemon_home = tempfile::tempdir().unwrap();
    let daemon_base = daemon_home
        .path()
        .join(".git-ai")
        .join("internal")
        .join("daemon");
    fs::create_dir_all(&daemon_base).unwrap();

    let socket_path = daemon_base.join("trace2.sock");
    let pid_file = daemon_base.join("daemon.pid.json");

    // Start the daemon in foreground mode with custom HOME
    let mut daemon_proc = Command::new(binary)
        .args(["bg", "run"])
        .env("HOME", daemon_home.path())
        .env("GIT_TRACE2_EVENT", "/dev/null")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("failed to spawn daemon");

    // Wait for the daemon to start (PID file appears)
    assert!(
        wait_for_file(&pid_file, Duration::from_secs(5)),
        "daemon did not start within 5s"
    );

    // Wait for the socket to appear
    assert!(
        wait_for_file(&socket_path, Duration::from_secs(5)),
        "daemon socket did not appear within 5s"
    );

    // Create a test repo
    let repo_dir = tempfile::tempdir().unwrap();
    let repo_path = repo_dir.path().to_path_buf();

    let init_output = Command::new(git)
        .args(["init", repo_path.to_str().unwrap()])
        .env("GIT_TRACE2_EVENT", "/dev/null")
        .output()
        .unwrap();
    assert!(init_output.status.success(), "git init failed");

    // Configure the repo
    for args in [
        vec!["config", "user.name", "Test User"],
        vec!["config", "user.email", "test@example.com"],
    ] {
        Command::new(git)
            .current_dir(&repo_path)
            .args(&args)
            .env("GIT_TRACE2_EVENT", "/dev/null")
            .output()
            .unwrap();
    }

    // Create an initial commit so HEAD exists for checkpoint
    let init_file = repo_path.join("init.txt");
    fs::write(&init_file, "init\n").unwrap();
    Command::new(git)
        .current_dir(&repo_path)
        .args(["add", "-A"])
        .env("GIT_TRACE2_EVENT", "/dev/null")
        .output()
        .unwrap();
    Command::new(git)
        .current_dir(&repo_path)
        .args(["commit", "-m", "initial"])
        .env("GIT_TRACE2_EVENT", "/dev/null")
        .output()
        .unwrap();

    // Write a file and create an AI checkpoint
    let test_file = repo_path.join("hello.txt");
    fs::write(&test_file, "Hello from AI\n").unwrap();

    // Run git-ai checkpoint to mark this as AI-authored
    let cp_output = Command::new(binary)
        .current_dir(&repo_path)
        .args(["checkpoint", "mock_ai", "hello.txt"])
        .env("GIT_TRACE2_EVENT", "/dev/null")
        .output()
        .expect("checkpoint failed");
    assert!(
        cp_output.status.success(),
        "checkpoint failed: {}",
        String::from_utf8_lossy(&cp_output.stderr)
    );

    // Route trace2 events to daemon via HOME/.gitconfig.
    // GIT_TRACE2_EVENT env var does NOT support af_unix: targets —
    // only trace2.eventTarget in ~/.gitconfig (read via HOME) works.
    let trace2_home = create_trace2_home(daemon_home.path(), &socket_path);

    Command::new(git)
        .current_dir(&repo_path)
        .args(["add", "hello.txt"])
        .env("HOME", &trace2_home)
        .env_remove("GIT_TRACE2_EVENT")
        .env_remove("GIT_TRACE2_EVENT_TARGET")
        .output()
        .unwrap();

    // Print env vars that might affect trace2
    for (k, v) in std::env::vars() {
        if k.contains("GIT") || k.contains("TRACE") {
            eprintln!("[test-env] {}={}", k, v);
        }
    }

    // Verify git can see the trace2 config
    let config_check = Command::new(git)
        .current_dir(&repo_path)
        .args(["config", "--list", "--show-origin"])
        .env("HOME", &trace2_home)
        .env_remove("GIT_TRACE2_EVENT")
        .output()
        .unwrap();
    let config_output = String::from_utf8_lossy(&config_check.stdout);
    for line in config_output.lines() {
        if line.contains("trace2") {
            eprintln!("[test] {}", line);
        }
    }
    eprintln!("[test] trace2_home: {}", trace2_home.display());
    eprintln!(
        "[test] .gitconfig exists: {}",
        trace2_home.join(".gitconfig").exists()
    );
    eprintln!(
        "[test] .gitconfig content: {}",
        fs::read_to_string(trace2_home.join(".gitconfig")).unwrap_or_default()
    );

    let commit_output = Command::new(git)
        .current_dir(&repo_path)
        .args(["commit", "-m", "AI commit"])
        .env("HOME", &trace2_home)
        .env_remove("GIT_TRACE2_EVENT")
        .env_remove("GIT_TRACE2_EVENT_TARGET")
        .output()
        .unwrap();
    assert!(
        commit_output.status.success(),
        "git commit failed: {}",
        String::from_utf8_lossy(&commit_output.stderr)
    );
    eprintln!(
        "[test] commit output: {}",
        String::from_utf8_lossy(&commit_output.stdout).trim()
    );

    // Get the commit SHA
    let head_output = Command::new(git)
        .current_dir(&repo_path)
        .args(["rev-parse", "HEAD"])
        .env("GIT_TRACE2_EVENT", "/dev/null")
        .output()
        .unwrap();
    let commit_sha = String::from_utf8_lossy(&head_output.stdout)
        .trim()
        .to_string();

    // Wait for the daemon to process the commit and write the note
    let note = wait_for_note(&repo_path, &commit_sha, Duration::from_secs(10));

    // Read the daemon log file (stderr is redirected there)
    let log_file = daemon_base.join("daemon.log");
    let daemon_log = fs::read_to_string(&log_file).unwrap_or_default();

    // Kill the daemon
    let _ = daemon_proc.kill();
    let _ = daemon_proc.wait();

    // Verify the note was written
    assert!(
        note.is_some(),
        "daemon did not write authorship note within 10s for commit {}\ndaemon log:\n{}",
        &commit_sha[..7],
        daemon_log
    );

    let note_content = note.unwrap();
    assert!(
        note_content.contains("authorship/3.0.0"),
        "note doesn't contain expected schema version: {}",
        &note_content[..200.min(note_content.len())]
    );
}

#[test]
fn test_daemon_is_idempotent() {
    let binary = get_binary_path();
    let git = real_git_executable();

    // Create an isolated daemon home
    let daemon_home = tempfile::tempdir().unwrap();
    let daemon_base = daemon_home
        .path()
        .join(".git-ai")
        .join("internal")
        .join("daemon");
    fs::create_dir_all(&daemon_base).unwrap();

    let socket_path = daemon_base.join("trace2.sock");
    let pid_file = daemon_base.join("daemon.pid.json");

    // Start daemon
    let mut daemon_proc = Command::new(binary)
        .args(["bg", "run"])
        .env("HOME", daemon_home.path())
        .env("GIT_TRACE2_EVENT", "/dev/null")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("failed to spawn daemon");

    assert!(wait_for_file(&pid_file, Duration::from_secs(5)));
    assert!(wait_for_file(&socket_path, Duration::from_secs(5)));

    let trace2_home = create_trace2_home(daemon_home.path(), &socket_path);

    // Create test repo
    let repo_dir = tempfile::tempdir().unwrap();
    let repo_path = repo_dir.path().to_path_buf();

    Command::new(git)
        .args(["init", repo_path.to_str().unwrap()])
        .env("GIT_TRACE2_EVENT", "/dev/null")
        .output()
        .unwrap();

    for args in [
        vec!["config", "user.name", "Test User"],
        vec!["config", "user.email", "test@example.com"],
    ] {
        Command::new(git)
            .current_dir(&repo_path)
            .args(&args)
            .env("GIT_TRACE2_EVENT", "/dev/null")
            .output()
            .unwrap();
    }

    // Initial commit
    fs::write(repo_path.join("init.txt"), "init\n").unwrap();
    Command::new(git)
        .current_dir(&repo_path)
        .args(["add", "-A"])
        .env("GIT_TRACE2_EVENT", "/dev/null")
        .output()
        .unwrap();
    Command::new(git)
        .current_dir(&repo_path)
        .args(["commit", "-m", "initial"])
        .env("GIT_TRACE2_EVENT", "/dev/null")
        .output()
        .unwrap();

    // Write a file with checkpoint
    fs::write(repo_path.join("hello.txt"), "Hello\n").unwrap();
    Command::new(binary)
        .current_dir(&repo_path)
        .args(["checkpoint", "mock_ai", "hello.txt"])
        .env("GIT_TRACE2_EVENT", "/dev/null")
        .output()
        .unwrap();

    // Commit with trace2 going to daemon
    Command::new(git)
        .current_dir(&repo_path)
        .args(["add", "-A"])
        .env("HOME", &trace2_home)
        .env_remove("GIT_TRACE2_EVENT")
        .output()
        .unwrap();
    Command::new(git)
        .current_dir(&repo_path)
        .args(["commit", "-m", "Test idempotency"])
        .env("HOME", &trace2_home)
        .env_remove("GIT_TRACE2_EVENT")
        .output()
        .unwrap();

    // Get commit SHA
    let head = Command::new(git)
        .current_dir(&repo_path)
        .args(["rev-parse", "HEAD"])
        .env("GIT_TRACE2_EVENT", "/dev/null")
        .output()
        .unwrap();
    let commit_sha = String::from_utf8_lossy(&head.stdout).trim().to_string();

    // Manually run post-commit (simulating the hook firing too)
    Command::new(binary)
        .current_dir(&repo_path)
        .args(["post-commit"])
        .env("GIT_TRACE2_EVENT", "/dev/null")
        .output()
        .unwrap();

    // Read the note that the hook wrote
    let note_before = Command::new(git)
        .current_dir(&repo_path)
        .args(["notes", "--ref=ai", "show", &commit_sha])
        .env("GIT_TRACE2_EVENT", "/dev/null")
        .output()
        .unwrap();
    let note_before_content = String::from_utf8_lossy(&note_before.stdout).to_string();

    // Wait for the daemon to have had time to process (it should skip)
    thread::sleep(Duration::from_secs(3));

    // Read the note again — it should be identical (daemon skipped it)
    let note_after = Command::new(git)
        .current_dir(&repo_path)
        .args(["notes", "--ref=ai", "show", &commit_sha])
        .env("GIT_TRACE2_EVENT", "/dev/null")
        .output()
        .unwrap();
    let note_after_content = String::from_utf8_lossy(&note_after.stdout).to_string();

    // Kill daemon
    let _ = daemon_proc.kill();
    let _ = daemon_proc.wait();

    assert_eq!(
        note_before_content, note_after_content,
        "daemon should not overwrite an existing note"
    );
}

#[test]
fn test_daemon_handles_rapid_sequential_commits() {
    let binary = get_binary_path();
    let git = real_git_executable();

    // Create an isolated daemon home
    let daemon_home = tempfile::tempdir().unwrap();
    let daemon_base = daemon_home
        .path()
        .join(".git-ai")
        .join("internal")
        .join("daemon");
    fs::create_dir_all(&daemon_base).unwrap();

    let socket_path = daemon_base.join("trace2.sock");
    let pid_file = daemon_base.join("daemon.pid.json");

    // Start daemon
    let mut daemon_proc = Command::new(binary)
        .args(["bg", "run"])
        .env("HOME", daemon_home.path())
        .env("GIT_TRACE2_EVENT", "/dev/null")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("failed to spawn daemon");

    assert!(wait_for_file(&pid_file, Duration::from_secs(5)));
    assert!(wait_for_file(&socket_path, Duration::from_secs(5)));

    let trace2_home = create_trace2_home(daemon_home.path(), &socket_path);

    // Create test repo
    let repo_dir = tempfile::tempdir().unwrap();
    let repo_path = repo_dir.path().to_path_buf();

    Command::new(git)
        .args(["init", repo_path.to_str().unwrap()])
        .env("GIT_TRACE2_EVENT", "/dev/null")
        .output()
        .unwrap();
    for args in [
        vec!["config", "user.name", "Test User"],
        vec!["config", "user.email", "test@example.com"],
    ] {
        Command::new(git)
            .current_dir(&repo_path)
            .args(&args)
            .env("GIT_TRACE2_EVENT", "/dev/null")
            .output()
            .unwrap();
    }

    // Initial commit so HEAD exists for checkpoints
    fs::write(repo_path.join("init.txt"), "init\n").unwrap();
    Command::new(git)
        .current_dir(&repo_path)
        .args(["add", "-A"])
        .env("GIT_TRACE2_EVENT", "/dev/null")
        .output()
        .unwrap();
    Command::new(git)
        .current_dir(&repo_path)
        .args(["commit", "-m", "initial"])
        .env("GIT_TRACE2_EVENT", "/dev/null")
        .output()
        .unwrap();

    // Make 3 rapid commits
    let mut commit_shas = Vec::new();
    for i in 1..=3 {
        let file = repo_path.join(format!("file{}.txt", i));
        fs::write(&file, format!("Content {}\n", i)).unwrap();

        Command::new(binary)
            .current_dir(&repo_path)
            .args(["checkpoint", "mock_ai", &format!("file{}.txt", i)])
            .env("GIT_TRACE2_EVENT", "/dev/null")
            .output()
            .unwrap();

        Command::new(git)
            .current_dir(&repo_path)
            .args(["add", "-A"])
            .env("HOME", &trace2_home)
            .env_remove("GIT_TRACE2_EVENT")
            .output()
            .unwrap();

        Command::new(git)
            .current_dir(&repo_path)
            .args(["commit", "-m", &format!("Commit {}", i)])
            .env("HOME", &trace2_home)
            .env_remove("GIT_TRACE2_EVENT")
            .output()
            .unwrap();

        let head = Command::new(git)
            .current_dir(&repo_path)
            .args(["rev-parse", "HEAD"])
            .env("GIT_TRACE2_EVENT", "/dev/null")
            .output()
            .unwrap();
        commit_shas.push(String::from_utf8_lossy(&head.stdout).trim().to_string());
    }

    // Wait for all notes to appear
    let mut all_noted = true;
    for sha in &commit_shas {
        if wait_for_note(&repo_path, sha, Duration::from_secs(15)).is_none() {
            all_noted = false;
            eprintln!("note missing for commit {}", &sha[..7]);
        }
    }

    // Kill daemon
    let _ = daemon_proc.kill();
    let _ = daemon_proc.wait();

    assert!(
        all_noted,
        "daemon should write notes for all 3 rapid commits"
    );
}
