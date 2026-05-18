//! Integration tests for `git-ai post-commit-hook`.
//!
//! Covers the two contracts the hook must hold:
//!   1. When the daemon has already produced an authorship note, the hook
//!      reads it back and renders the human/AI graph.
//!   2. The hook must NEVER exit non-zero (post-commit hooks must not fail
//!      the commit), even when the daemon socket is unreachable.

use crate::repos::test_repo::TestRepo;

#[test]
fn post_commit_hook_renders_graph_after_normal_commit() {
    let repo = TestRepo::new();

    // Write a file with both an AI-attributed line and a human-attributed line
    // so the stats have non-trivial content to render.
    std::fs::write(repo.path().join("hello.txt"), "human line\n").unwrap();
    repo.git_ai(&["checkpoint", "mock_known_human", "hello.txt"])
        .unwrap();

    std::fs::write(repo.path().join("hello.txt"), "human line\nai line\n").unwrap();
    repo.git_ai(&["checkpoint", "mock_ai", "hello.txt"])
        .unwrap();

    repo.git(&["add", "hello.txt"]).unwrap();
    repo.stage_all_and_commit("first commit").unwrap();

    // Daemon already wrote the note during the commit. The hook's job is to
    // render the graph for HEAD. Force TTY so the subcommand doesn't bail on
    // non-interactive stdout.
    let output = repo
        .git_ai_with_env(&["post-commit-hook"], &[("GIT_AI_TEST_FORCE_TTY", "1")])
        .expect("post-commit-hook must exit 0");

    // The graph uses Unicode block characters (█ known-human, · untracked,
    // ░ AI). At least one of them should appear in normal output.
    let has_graph = output.contains('█') || output.contains('░') || output.contains('·');
    assert!(
        has_graph,
        "expected post-commit-hook to render a graph; got:\n{}",
        output
    );
}

#[test]
fn post_commit_hook_exits_zero_when_daemon_socket_is_missing() {
    let repo = TestRepo::new();

    // Need an actual commit so HEAD resolves.
    std::fs::write(repo.path().join("a.txt"), "hi\n").unwrap();
    repo.git(&["add", "a.txt"]).unwrap();
    repo.stage_all_and_commit("initial").unwrap();

    // Point the daemon control socket at a path that definitely doesn't
    // exist. The hook's daemon RPC must fail-soft, fall through to the
    // poll-only path, and exit 0 — never propagate an error.
    let bogus_socket = repo.path().join("does-not-exist.sock");
    let result = repo.git_ai_with_env(
        &["post-commit-hook"],
        &[
            ("GIT_AI_TEST_FORCE_TTY", "1"),
            (
                "GIT_AI_DAEMON_CONTROL_SOCKET",
                bogus_socket.to_str().unwrap(),
            ),
            // Keep the poll loop short so the test runs quickly.
            ("GIT_AI_POST_COMMIT_TIMEOUT_MS", "50"),
        ],
    );

    // The whole point: post-commit hooks must not fail the commit. Exit 0,
    // always.
    assert!(
        result.is_ok(),
        "post-commit-hook must exit 0 when the daemon is unreachable; got error: {:?}",
        result
    );
}

#[test]
fn post_commit_hook_is_quiet_on_non_tty() {
    let repo = TestRepo::new();
    std::fs::write(repo.path().join("a.txt"), "hi\n").unwrap();
    repo.git(&["add", "a.txt"]).unwrap();
    repo.stage_all_and_commit("initial").unwrap();

    // No GIT_AI_TEST_FORCE_TTY → stdout is not a TTY → hook should silently
    // exit 0 without rendering anything.
    let output = repo
        .git_ai(&["post-commit-hook"])
        .expect("post-commit-hook must exit 0 even on non-TTY");
    assert!(
        !output.contains('█') && !output.contains('░'),
        "non-TTY post-commit-hook must not render a graph; got:\n{}",
        output
    );
}
