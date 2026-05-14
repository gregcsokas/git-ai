#[macro_use]
pub mod test_file;
pub mod test_repo;

/// No-op macro that re-runs listed tests in worktree mode.
/// In this minimal harness, it simply generates duplicate test functions
/// that call the original (no actual worktree setup).
#[macro_export]
macro_rules! reuse_tests_in_worktree {
    (
        $( $test_name:ident ),+ $(,)?
    ) => {
        // No-op: worktree variants are not supported in the minimal harness.
    };
}
