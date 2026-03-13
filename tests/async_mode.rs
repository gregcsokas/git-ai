mod repos;

use repos::test_repo::{GitTestMode, TestRepo, real_git_executable};
use std::fs;
use std::path::PathBuf;
use std::process::Command;

fn git_common_dir(repo: &TestRepo) -> PathBuf {
    let common_dir = repo
        .git(&["rev-parse", "--git-common-dir"])
        .expect("rev-parse --git-common-dir should succeed");
    let common_dir = PathBuf::from(common_dir.trim());
    if common_dir.is_absolute() {
        common_dir
    } else {
        repo.path().join(common_dir)
    }
}

fn read_global_git_config(repo: &TestRepo, key: &str) -> Option<String> {
    let output = Command::new(real_git_executable())
        .args(["config", "--global", "--get", key])
        .current_dir(repo.path())
        .env("HOME", repo.test_home_path())
        .env(
            "GIT_CONFIG_GLOBAL",
            repo.test_home_path().join(".gitconfig"),
        )
        .output()
        .expect("failed to read global git config");

    if output.status.success() {
        let value = String::from_utf8_lossy(&output.stdout).trim().to_string();
        if value.is_empty() { None } else { Some(value) }
    } else {
        None
    }
}

#[test]
fn async_mode_wrapper_commit_passthrough_skips_git_ai_side_effects() {
    let repo = TestRepo::new_with_mode(GitTestMode::Wrapper);
    let ai_dir = git_common_dir(&repo).join("ai");
    let _ = fs::remove_dir_all(&ai_dir);
    assert!(
        ai_dir.symlink_metadata().is_err(),
        "expected test setup to start without .git/ai state"
    );

    fs::write(repo.path().join("async-mode.txt"), "async mode test\n")
        .expect("failed to write test file");

    repo.git_with_env(
        &["add", "async-mode.txt"],
        &[("GIT_AI_ASYNC_MODE", "true")],
        None,
    )
    .expect("git add should succeed in async mode");
    repo.git_with_env(
        &["commit", "-m", "async passthrough commit"],
        &[("GIT_AI_ASYNC_MODE", "true")],
        None,
    )
    .expect("git commit should succeed in async mode");

    assert!(
        ai_dir.symlink_metadata().is_err(),
        "async mode wrapper should passthrough without creating .git/ai side effects"
    );
}

#[test]
fn install_hooks_async_mode_sets_daemon_trace2_global_config() {
    let repo = TestRepo::new_with_mode(GitTestMode::Wrapper);

    let output = repo
        .git_ai_with_env(
            &["install-hooks", "--dry-run=false"],
            &[("GIT_AI_ASYNC_MODE", "true")],
        )
        .expect("install-hooks should succeed in async mode");

    assert!(
        !output.contains("trace2.eventTarget") && !output.contains("trace2.eventNesting"),
        "async preflight should run silently without trace2 config output"
    );

    let expected_trace_socket = repo
        .test_home_path()
        .join(".git-ai")
        .join("internal")
        .join("daemon")
        .join("trace2.sock");
    let expected_target = format!("af_unix:stream:{}", expected_trace_socket.to_string_lossy());

    let target = read_global_git_config(&repo, "trace2.eventTarget");
    let nesting = read_global_git_config(&repo, "trace2.eventNesting");

    assert_eq!(target.as_deref(), Some(expected_target.as_str()));
    assert_eq!(nesting.as_deref(), Some("10"));
}

#[test]
fn install_hooks_async_mode_dry_run_does_not_write_trace2_global_config() {
    let repo = TestRepo::new_with_mode(GitTestMode::Wrapper);

    repo.git_ai_with_env(
        &["install-hooks", "--dry-run=true"],
        &[("GIT_AI_ASYNC_MODE", "true")],
    )
    .expect("install-hooks dry-run should succeed in async mode");

    let target = read_global_git_config(&repo, "trace2.eventTarget");
    let nesting = read_global_git_config(&repo, "trace2.eventNesting");

    assert!(
        target.is_none(),
        "install-hooks dry-run should not set trace2.eventTarget"
    );
    assert!(
        nesting.is_none(),
        "install-hooks dry-run should not set trace2.eventNesting"
    );
}
