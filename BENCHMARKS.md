# Benchmarks: v1 vs v2

## Environment

- Platform: Linux 6.12.86+deb13-cloud-arm64 (aarch64)
- Git: 2.47.3
- v1: 1.4.6 (92 MB binary, daemon architecture)
- v2: 2.0.0-alpha.1 (2.4 MB binary, daemon + sync fallback)
- Methodology: median of 20-50 runs per operation, fresh repo per test

## Checkpoint

The hottest path — called on every AI file edit by every agent preset.

| Scenario | v1 | v2 |
|----------|----|----|
| Small file (10 lines) | 2ms | 3ms |
| Medium file (200 lines) | 3ms | 3ms |

v1 dispatches via IPC to a background daemon. v2 processes synchronously with zero git process spawns — repo root and HEAD are resolved entirely from the filesystem.

## Post-commit

Generates and writes the authorship note after `git commit`.

| Scenario | v1 | v2 |
|----------|----|----|
| Single file commit | 2ms | 7ms |

v1's daemon has already processed checkpoints before the commit happens, so post-commit is a near-instant note write. v2 requires two git process spawns (`cat-file` to read commit metadata, `notes add` to write the note) at ~3ms each, plus working log processing.

## Blame

Read path — resolves per-line authorship from git notes.

| File size | v1 | v2 | Speedup |
|-----------|----|----|---------|
| 100 lines | 22ms | 6ms | 3.7x |
| 500 lines | 41ms | 11ms | 3.7x |
| 1000 lines | 64ms | 16ms | 4.0x |

v2 wins on blame due to its 38x smaller binary (faster cold start), leaner initialization, and streamlined note-parsing path.

## Binary size and startup

| Metric | v1 | v2 |
|--------|----|----|
| Binary size | 92 MB | 2.4 MB |
| Startup (`--version`) | 2ms | 1ms |

## Summary

| Operation | Winner | Margin |
|-----------|--------|--------|
| Checkpoint | Tie | v1 2ms, v2 3ms |
| Post-commit | v1 | 2ms vs 7ms |
| Blame | v2 | 4x faster |
| Binary size | v2 | 38x smaller |

v2 matches v1 on the critical checkpoint hot path. The 5ms post-commit gap comes from two required git subprocess spawns that v1 avoids by keeping state in a persistent daemon process. v2 dominates the read path (blame) where its smaller binary and leaner runtime pay off.
