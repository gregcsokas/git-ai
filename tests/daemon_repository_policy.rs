#[path = "integration/repos/mod.rs"]
mod repos;

use repos::test_repo::{
    DaemonTestScope, GitTestMode, TestRepo, get_binary_path, real_git_executable,
};
use std::fs;
use std::process::Command;
use std::thread;
use std::time::{Duration, Instant};

const EXCLUDED_REMOTE: &str = "https://github.com/excluded/repo";
const ALLOWED_REMOTE: &str = "https://github.com/allowed/repo";

fn configure_repo_policy(repo: &mut TestRepo, allowed: bool) {
    repo.patch_git_ai_config(|patch| {
        if allowed {
            patch.allow_repositories = Some(vec!["https://github.com/allowed/*".to_string()]);
            patch.exclude_repositories = Some(vec![]);
        } else {
            patch.allow_repositories = Some(vec![]);
            patch.exclude_repositories = Some(vec!["https://github.com/excluded/*".to_string()]);
        }
    });
}

fn new_repo_with_policy(mode: GitTestMode, allowed: bool) -> TestRepo {
    let remote = if allowed {
        ALLOWED_REMOTE
    } else {
        EXCLUDED_REMOTE
    };
    TestRepo::new_with_mode_and_daemon_scope_configured(mode, DaemonTestScope::Dedicated, |repo| {
        configure_repo_policy(repo, allowed);
        repo.git_og(&["remote", "add", "origin", remote])
            .expect("remote add should succeed before daemon startup");
    })
}

fn run_tracked_commit_without_test_sync(repo: &TestRepo, message: &str) {
    fs::write(repo.path().join("tracked.txt"), "tracked\n").expect("write should succeed");

    let mut command = if repo.mode().uses_wrapper() {
        Command::new(get_binary_path())
    } else {
        Command::new(real_git_executable())
    };
    command.current_dir(repo.path());
    command.args(["-C", repo.path().to_str().unwrap(), "add", "tracked.txt"]);
    repo.configure_test_git_command_env(&mut command);
    let add_output = command.output().expect("git add should execute");
    assert!(
        add_output.status.success(),
        "git add should succeed: stdout={} stderr={}",
        String::from_utf8_lossy(&add_output.stdout),
        String::from_utf8_lossy(&add_output.stderr)
    );

    let mut command = if repo.mode().uses_wrapper() {
        Command::new(get_binary_path())
    } else {
        Command::new(real_git_executable())
    };
    command.current_dir(repo.path());
    command.args(["-C", repo.path().to_str().unwrap(), "commit", "-m", message]);
    repo.configure_test_git_command_env(&mut command);
    let commit_output = command.output().expect("git commit should execute");
    assert!(
        commit_output.status.success(),
        "git commit should succeed: stdout={} stderr={}",
        String::from_utf8_lossy(&commit_output.stdout),
        String::from_utf8_lossy(&commit_output.stderr)
    );
}

fn wait_for_completion_count(repo: &TestRepo, baseline: u64, expected: u64) {
    repo.wait_for_daemon_total_completion_count(baseline, expected);
}

fn assert_completion_count_stable(repo: &TestRepo, expected: u64) {
    let deadline = Instant::now() + Duration::from_secs(2);
    while Instant::now() < deadline {
        assert_eq!(
            repo.daemon_total_completion_count(),
            expected,
            "daemon should not process excluded repository commands"
        );
        thread::sleep(Duration::from_millis(25));
    }
}

fn assert_repository_policy(mode: GitTestMode, allowed: bool) {
    let repo = new_repo_with_policy(mode, allowed);
    let baseline = repo.daemon_total_completion_count();
    run_tracked_commit_without_test_sync(
        &repo,
        if allowed {
            "allowed daemon commit"
        } else {
            "excluded daemon commit"
        },
    );

    if allowed {
        wait_for_completion_count(&repo, baseline, baseline.saturating_add(1));
    } else {
        assert_completion_count_stable(&repo, baseline);
    }
}

#[test]
fn daemon_mode_excluded_repository_skips_processing() {
    assert_repository_policy(GitTestMode::Daemon, false);
}

#[test]
fn daemon_mode_allowlisted_repository_processes_commands() {
    assert_repository_policy(GitTestMode::Daemon, true);
}

#[test]
fn wrapper_daemon_mode_excluded_repository_skips_processing() {
    assert_repository_policy(GitTestMode::WrapperDaemon, false);
}

#[test]
fn wrapper_daemon_mode_allowlisted_repository_processes_commands() {
    assert_repository_policy(GitTestMode::WrapperDaemon, true);
}
