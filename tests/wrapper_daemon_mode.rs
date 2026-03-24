#[path = "integration/repos/mod.rs"]
mod repos;

use repos::test_file::ExpectedLineExt;
use repos::test_repo::{DaemonTestScope, GitTestMode, TestRepo};

#[test]
fn wrapper_daemon_checkout_branch_migrates_working_log_in_worktree() {
    let repo = TestRepo::new_worktree_with_mode_and_daemon_scope(
        GitTestMode::WrapperDaemon,
        DaemonTestScope::Dedicated,
    );

    let mut readme = repo.filename("README.md");
    readme.set_contents(vec!["# Test Repo".to_string()]);
    repo.stage_all_and_commit("initial commit")
        .expect("initial commit should succeed");

    repo.git(&["branch", "feature"])
        .expect("branch creation should succeed");

    let mut ai_file = repo.filename("ai_work.txt");
    ai_file.set_contents(vec!["AI generated line 1".ai(), "AI generated line 2".ai()]);

    repo.git_ai(&["checkpoint", "mock_ai"])
        .expect("checkpoint should succeed");

    repo.git(&["checkout", "feature"])
        .expect("checkout should succeed");

    repo.stage_all_and_commit("commit on feature branch")
        .expect("commit should succeed");

    ai_file.assert_lines_and_blame(vec!["AI generated line 1".ai(), "AI generated line 2".ai()]);
}

#[test]
fn wrapper_daemon_switch_branch_migrates_working_log_in_worktree() {
    let repo = TestRepo::new_worktree_with_mode_and_daemon_scope(
        GitTestMode::WrapperDaemon,
        DaemonTestScope::Dedicated,
    );

    let mut readme = repo.filename("README.md");
    readme.set_contents(vec!["# Test Repo".to_string()]);
    repo.stage_all_and_commit("initial commit")
        .expect("initial commit should succeed");

    repo.git(&["branch", "feature"])
        .expect("branch creation should succeed");

    let mut ai_file = repo.filename("ai_work.txt");
    ai_file.set_contents(vec!["AI generated line 1".ai(), "AI generated line 2".ai()]);

    repo.git_ai(&["checkpoint", "mock_ai"])
        .expect("checkpoint should succeed");

    repo.git(&["switch", "feature"])
        .expect("switch should succeed");

    repo.stage_all_and_commit("commit on feature branch")
        .expect("commit should succeed");

    ai_file.assert_lines_and_blame(vec!["AI generated line 1".ai(), "AI generated line 2".ai()]);
}
