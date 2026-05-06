use crate::repos::test_repo::TestRepo;

#[test]
fn proxy_passthrough_forwards_git_commands() {
    let repo = TestRepo::new();

    // Basic git status via the proxy
    let result = repo.git(&["status"]);
    assert!(result.is_ok(), "git status should succeed: {:?}", result);

    // Create a file and commit via the proxy
    std::fs::write(repo.path().join("proxy_test.txt"), "hello from proxy test").unwrap();
    repo.git(&["add", "proxy_test.txt"]).unwrap();
    let result = repo.git(&["commit", "-m", "test proxy commit"]);
    assert!(result.is_ok(), "git commit should succeed: {:?}", result);

    // Verify the commit exists via the proxy
    let output = repo.git(&["log", "--oneline"]).unwrap();
    assert!(
        output.contains("test proxy commit"),
        "commit message should appear in log: {}",
        output
    );
}

#[test]
fn proxy_passthrough_forwards_nonzero_exit_code() {
    let repo = TestRepo::new();

    // A git command that will fail (checking out nonexistent branch)
    let result = repo.git(&["checkout", "nonexistent-branch-xyz"]);
    assert!(
        result.is_err(),
        "checkout of nonexistent branch should fail"
    );
}
