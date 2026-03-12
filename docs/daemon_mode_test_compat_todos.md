# Daemon Mode Test Compatibility TODOs

## Scope

Track daemon-mode compatibility for integration suites that were not reached in the last full run:

`GIT_AI_TEST_GIT_MODE=daemon cargo test --package git-ai -- --nocapture`

## Known Failure: `internal_db_integration` (worktree variants)

### Symptom

All `_in_worktree` variants in `tests/internal_db_integration.rs` failed with `0` prompt rows in DB when `1+` was expected.

### Root Cause (current understanding)

- `reuse_tests_in_worktree!` drives tests through `with_worktree_mode()` and `TestRepo::new()` -> `new_worktree_variant()`.
- `new_worktree_variant()` starts from a base repo created via `new_with_mode()`, which starts daemon mode with the base `test_db_path`.
- `new_worktree_variant()` then switches to a linked worktree and assigns a new `wt_test_db_path`, while reusing the already-running daemon process.
- Checkpoints delegated to daemon write through daemon process env (`GIT_AI_TEST_DB_PATH`) from daemon startup time (base DB path).
- Tests query `repo.test_db_path()` (worktree DB path), so reads and writes target different DB files.

### Code References

- `tests/repos/mod.rs` (`reuse_tests_in_worktree!`)
- `tests/repos/test_repo.rs` (`new_worktree_variant`, `setup_daemon_mode`, `DaemonProcess::start`)
- `src/authorship/internal_db.rs` (`INTERNAL_DB` `OnceLock`, DB path env resolution)

### Action Items

- [ ] Fix worktree daemon harness DB path alignment (`new_worktree_variant` and daemon startup strategy).
- [ ] Re-run `internal_db_integration` in daemon mode and confirm all `_in_worktree` variants pass.
- [ ] Re-run full daemon-mode suite after fix.

## Unreached Suites (from last full run)

Status legend: `PASS`, `FAIL`, `SKIP` (not run), `PENDING`.

Run: `GIT_AI_TEST_GIT_MODE=daemon cargo test --package git-ai --test <suite> -- --nocapture` (each suite run sequentially).

Summary: `43` suites run, `37` passed, `6` failed.

| Suite | Status | Notes |
|---|---|---|
| `internal_machine_commands.rs` | `PASS` |  |
| `internal_spawn_safety.rs` | `PASS` |  |
| `jetbrains_download.rs` | `PASS` |  |
| `jetbrains_ide_types.rs` | `PASS` |  |
| `merge_hooks_comprehensive.rs` | `PASS` |  |
| `merge_rebase.rs` | `PASS` |  |
| `multi_repo_workspace.rs` | `PASS` |  |
| `non_utf8_files.rs` | `PASS` |  |
| `observability_flush.rs` | `PASS` |  |
| `opencode.rs` | `PASS` |  |
| `performance.rs` | `PASS` |  |
| `prompt_across_commit.rs` | `PASS` |  |
| `prompt_hash_migration.rs` | `PASS` |  |
| `prompt_picker_test.rs` | `PASS` |  |
| `prompts_db_test.rs` | `PASS` |  |
| `pull_rebase_ff.rs` | `FAIL` | 8 failing tests; pull/rebase attribution + skipped-commit mapping assertions. |
| `push_upstream_authorship.rs` | `FAIL` | 4 failing tests; authorship notes not pushed in upstream-set flows. |
| `realistic_complex_edits.rs` | `PASS` |  |
| `rebase.rs` | `FAIL` | 6 failing tests; explicit-branch/root mapping + daemon idle settle in preserve-merges paths. |
| `rebase_hooks_comprehensive.rs` | `PASS` |  |
| `reset.rs` | `PASS` |  |
| `reset_hooks_comprehensive.rs` | `PASS` |  |
| `search.rs` | `PASS` |  |
| `secrets_benchmark.rs` | `PASS` |  |
| `share_tui_comprehensive.rs` | `PASS` |  |
| `show_prompt.rs` | `PASS` |  |
| `simple_additions.rs` | `PASS` |  |
| `simple_benchmark.rs` | `PASS` |  |
| `squash_merge.rs` | `FAIL` | 8 failing tests; squash prep flows report expected AI lines as human. |
| `stash_attribution.rs` | `FAIL` | 6 failing tests; stash apply/pop paths regress AI attribution to human. |
| `stats.rs` | `PASS` |  |
| `status_ignore.rs` | `PASS` |  |
| `subdirs.rs` | `FAIL` | 4 failing tests; subdir/`-C` squash-merge cases show AI lines as human. |
| `sublime_merge_installer.rs` | `PASS` |  |
| `switch_hooks_comprehensive.rs` | `PASS` |  |
| `sync_authorship_types.rs` | `PASS` |  |
| `test_utils.rs` | `PASS` |  |
| `tls_native_certs.rs` | `PASS` |  |
| `utf8_filenames.rs` | `PASS` |  |
| `virtual_attribution_merge.rs` | `PASS` |  |
| `windsurf.rs` | `PASS` |  |
| `worktrees.rs` | `PASS` |  |
| `wrapper_performance_targets.rs` | `PASS` |  |

## Failure Buckets (from unreached-suite sweep)

- `pull_rebase_ff.rs`
  - `test_fast_forward_pull_preserves_ai_attribution` (+ worktree variant)
  - `test_pull_rebase_autostash_via_git_config` (+ worktree variant)
  - `test_pull_rebase_via_git_config_preserves_committed_ai_authorship` (+ worktree variant)
  - `test_pull_rebase_skip_commit_does_not_map_entire_upstream_history` (+ worktree variant)
- `push_upstream_authorship.rs`
  - `push_with_set_upstream_flag_pushes_authorship_notes` (+ worktree variant)
  - `push_after_branch_set_upstream_pushes_authorship_notes` (+ worktree variant)
- `rebase.rs`
  - `test_rebase_with_explicit_branch_argument_preserves_authorship` (+ worktree variant)
  - `test_rebase_root_with_explicit_branch_argument_preserves_authorship` (+ worktree variant)
  - `test_rebase_preserve_merges` (+ worktree variant; daemon sync settle timeout)
- `squash_merge.rs`
  - `test_prepare_working_log_simple_squash` (+ worktree variant)
  - `test_prepare_working_log_squash_multiple_sessions` (+ worktree variant)
  - `test_prepare_working_log_squash_with_main_changes` (+ worktree variant)
  - `test_prepare_working_log_squash_with_mixed_additions` (+ worktree variant)
- `stash_attribution.rs`
  - `test_stash_apply_reset_apply_again` (+ worktree variant)
  - `test_stash_pop_onto_head_with_ai_changes` (+ worktree variant)
  - `test_stash_pop_with_existing_stack_entries` (+ worktree variant)
- `subdirs.rs`
  - `test_squash_merge_from_subdir` (+ worktree variant)
  - `test_squash_merge_with_c_flag` (+ worktree variant)
