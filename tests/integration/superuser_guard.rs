use crate::repos::test_repo::TestRepo;
#[cfg(unix)]
use crate::repos::test_repo::get_binary_path;
#[cfg(unix)]
use std::process::Command;

#[test]
fn superuser_guard_does_not_block_non_root_invocations() {
    let repo = TestRepo::new();
    let result = repo.git_ai(&["version"]);
    assert!(result.is_ok(), "version command should always succeed");
}

#[test]
fn superuser_guard_allow_env_var_is_respected() {
    let repo = TestRepo::new();
    let result = repo.git_ai_with_env(&["version"], &[("GIT_AI_ALLOW_SUPERUSER", "1")]);
    assert!(
        result.is_ok(),
        "version should succeed with GIT_AI_ALLOW_SUPERUSER=1"
    );
}

#[test]
fn superuser_guard_exempt_commands_always_work() {
    let repo = TestRepo::new();
    for cmd in ["version", "--version", "-v", "help", "--help", "-h"] {
        let result = repo.git_ai(&[cmd]);
        assert!(
            result.is_ok(),
            "{cmd} should be exempt from superuser guard"
        );
    }
}

#[test]
#[cfg(unix)]
fn superuser_guard_blocks_when_running_as_root_without_opt_in() {
    if unsafe { libc::geteuid() } != 0 {
        // Can't test blocking behavior as non-root; skip.
        return;
    }

    let binary_path = get_binary_path();
    let mut cmd = Command::new(binary_path);
    cmd.args(["status"]).env_remove("GIT_AI_ALLOW_SUPERUSER");
    remove_all_ci_env_vars(&mut cmd);
    let output = cmd.output().expect("failed to execute binary");

    assert!(
        !output.status.success(),
        "should fail when running as root without opt-in"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("should not be run with elevated privileges"),
        "should show superuser error message, got: {stderr}"
    );
}

#[cfg(unix)]
fn remove_all_ci_env_vars(cmd: &mut Command) -> &mut Command {
    cmd.env_remove("CI")
        .env_remove("GITHUB_ACTIONS")
        .env_remove("GITLAB_CI")
        .env_remove("JENKINS_URL")
        .env_remove("BUILDKITE")
        .env_remove("CIRCLECI")
        .env_remove("CODEBUILD_BUILD_ID")
        .env_remove("AGENT_OS")
        .env_remove("KUBERNETES_SERVICE_HOST")
        .env_remove("GIT_AI_DAEMON_UPGRADE")
}

#[test]
#[cfg(unix)]
fn superuser_guard_allows_root_with_env_var_opt_in() {
    if unsafe { libc::geteuid() } != 0 {
        return;
    }

    let binary_path = get_binary_path();
    let mut cmd = Command::new(binary_path);
    cmd.args(["status"]).env("GIT_AI_ALLOW_SUPERUSER", "1");
    remove_all_ci_env_vars(&mut cmd);
    let output = cmd.output().expect("failed to execute binary");

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !stderr.contains("should not be run with elevated privileges"),
        "should NOT block when GIT_AI_ALLOW_SUPERUSER=1 is set, got: {stderr}"
    );
    assert!(
        stderr.contains("warning: running as superuser"),
        "should show warning when running as root with opt-in, got: {stderr}"
    );
}

#[test]
#[cfg(unix)]
fn superuser_guard_allows_root_in_ci_environment() {
    if unsafe { libc::geteuid() } != 0 {
        return;
    }

    let binary_path = get_binary_path();
    let mut cmd = Command::new(binary_path);
    cmd.args(["status"])
        .env("CI", "true")
        .env_remove("GIT_AI_ALLOW_SUPERUSER");
    remove_all_ci_env_vars(&mut cmd);
    // Re-add just CI=true (remove_all_ci_env_vars removes it)
    cmd.env("CI", "true");
    let output = cmd.output().expect("failed to execute binary");

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !stderr.contains("should not be run with elevated privileges"),
        "should NOT block in CI environment, got: {stderr}"
    );
    assert!(
        !stderr.contains("warning: running as superuser"),
        "should NOT warn in CI environment (silent pass), got: {stderr}"
    );
}
