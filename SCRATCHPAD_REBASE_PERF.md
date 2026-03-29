# Rebase Performance Deep Dive - Scratchpad

## Timeline
- Started: 2026-03-28 18:43 UTC
- Phase 1 (Benchmark + Profile): until 22:00 UTC ✅
- Phase 2 (Solution Planning): 22:00-23:30 UTC (IN PROGRESS)
- Phase 3 (Implementation): 23:30-04:00 UTC
- Phase 4 (Report): after 04:00 UTC

## Architecture Summary

### Rebase Attribution Pipeline (end-to-end)
1. **Pre-rebase hook**: Captures original HEAD, logs RebaseStartEvent
2. **Git rebase executes**: Replays commits onto new base, creates new SHAs
3. **Post-rebase hook**: Calls `rewrite_authorship_after_rebase_v2()`

### rewrite_authorship_after_rebase_v2() Steps
1. `load_rebase_note_cache()` - Batch reads all note blob OIDs + contents (2 git calls)
2. `try_fast_path_rebase_note_remap_cached()` - If all tracked file blobs identical → direct note copy (returns early)
3. `run_diff_tree_for_commits()` - Single `git diff-tree --stdin --raw -z` to find changed files per commit
4. `try_reconstruct_attributions_from_notes_cached()` - Reconstructs initial attribution state from original notes
5. `batch_read_blob_contents_parallel()` - Reads all blob contents in parallel batches
6. **COMMIT PROCESSING LOOP** (the bottleneck):
   - For each commit: for each changed file: `diff_based_line_attribution_transfer()`
   - Uses `imara_diff::Myers` to diff old content vs new content
   - Transfers attributions positionally based on diff ops
   - Updates serialization cache and metrics
7. `notes_add_batch()` - Writes all notes via `git fast-import`

### Why We Must Look at Each Commit
- AI attribution notes are SCOPED to changes in each specific commit
- Line ranges shift as code changes across the commit sequence
- Each rebased commit may have different file content than the original
- Must track attribution evolution through the entire sequence
- Notes store per-file, per-prompt line ranges that need exact recomputation

## Benchmark Results

### Run 1: Small (20 commits × 10 files × 50 lines)
- Total rebase: 179ms (git: 139ms, overhead: 40ms = 28.8%)
- Per-commit overhead: 2.0ms

### Run 2: Medium (100 commits × 30 files × 200 lines)
- Total rebase: 2788ms
- Authorship rewrite total: 1546ms
- **Phase breakdown:**
  - load_rebase_note_cache: 31ms (2%)
  - diff_tree: 47ms (3%)
  - attribution_reconstruction: 9ms (1%)
  - blob_read_parallel: 59ms (4%)
  - **commit_processing_loop: 1347ms (87%)**
    - **loop:transform: 1294ms (84%)**
      - **transform:diff: 1328ms (86%) ← THE BOTTLENECK**
      - transform:attestation_serialize: <1ms
      - transform:content_clone: 3ms
      - transform:metrics: <1ms
    - loop:serialize: <1ms
    - loop:metrics: <1ms
  - notes_add_batch: 23ms (1.5%)
- Stats: 3000 file diffs, 805,500 total lines diffed

### Run 3: Heavy (200 commits × 50 files × 300 lines) [IN PROGRESS]
- Expected: ~10,000 file diffs, ~4M total lines, ~6-7s diff time

## THE BOTTLENECK: diff_based_line_attribution_transfer()

### What It Does
For each changed file in each commit:
1. Split old + new content into Vec<&str> lines
2. Build old_line_author lookup (Vec<Option<&str>>)
3. Create InternedInput (hashes ALL lines for interning)
4. Run Myers diff algorithm
5. Walk DiffOps to transfer attributions
6. Create Vec<LineAttribution> result

### Why It's Expensive
- 3000 diffs × 0.44ms/diff = 1328ms
- Files grow linearly (200 → 300 lines over 100 commits)
- Total work is O(N_commits × N_files × avg_file_size)
- Quadratic in commits for growing files: O(N² × files)
- InternedInput hashes every line every time (no reuse)

### What's NOT Expensive (everything else)
- Blob reading: 59ms (parallel, efficient)
- Note cache: 31ms (batch git calls)
- diff-tree: 47ms (single git call)
- Serialization: <1ms (cached assembly)
- Note writing: 23ms (git fast-import)
- Metrics: <1ms

## Solution Ideas (Ranked by Expected Impact)

### IDEA A: Hunk-Based Attribution Transfer via git diff-tree -p -U0
**Impact: HIGH (eliminates diff computation entirely)**
**Complexity: MEDIUM**

Instead of re-diffing file contents ourselves, use `git diff-tree --stdin -p -U0` which gives
minimal unified diff output with hunk headers showing exactly what line ranges changed.

Parse hunk headers like `@@ -10,5 +12,6 @@` to build line offset maps:
- Lines before hunk: map 1:1
- Deleted lines in hunk: dropped
- Inserted lines in hunk: new (no attribution)
- Lines after hunk: map 1:1 with accumulated offset

This eliminates:
- Reading blob contents entirely (~60ms saved)
- Running ANY diff algorithm (~1328ms saved)
- Only requires parsing diff-tree output (~50ms estimated)

**Key subtlety**: For commit N1, diff-tree compares against new_base, but our initial attributions
are from original_head. For N2+, diff-tree parent IS our accumulated state.

**Solution**: For N1, use the content-diff fallback. For N2+, use hunk-based transfer.
Since N1 is 1 commit out of 200, the overhead is negligible.

### IDEA B: Parallel Per-File Processing
**Impact: MEDIUM-HIGH (4-8x speedup on multi-core)**
**Complexity: LOW**

The file diffs within each commit are independent. Parallelize using smol/rayon.
Each file reads from shared immutable state and produces independent results that
are merged back.

Can be combined with IDEA A for multiplicative benefit.

### IDEA C: Fast-Path for Simple Changes
**Impact: MEDIUM (skips diff for common patterns)**
**Complexity: LOW**

Detect simple change patterns and avoid the full diff:
1. **Pure append**: new_content starts with old_content → keep all old attrs unchanged
2. **Single insertion**: common prefix + common suffix = full old content → shift attrs after insert point
3. **Single deletion**: similar detection

For the typical AI coding workflow (appending functions), this could skip >50% of diffs.

## Key Git Internals Insight

`git diff-tree --stdin -p -U0` compares each commit against its parent and outputs
minimal unified diff patches. For a linear rebase sequence (N1→N2→...→NN),
each Nk's parent is N(k-1), which is exactly the state we're tracking.

This means for commits N2..NN, the diff-tree patch output gives us exactly the
line-level change information we need, without any content reading or diff computation.

Hunk header format: `@@ -old_start[,old_count] +new_start[,new_count] @@`
- old_count defaults to 1 if omitted
- Pure insertion: old_count=0 (e.g., `@@ -10,0 +11,3 @@`)
- Pure deletion: new_count=0 (e.g., `@@ -10,3 +10,0 @@`)
- Replacement: both non-zero

## Implementation Plan

### Phase 3A: Implement IDEA A (Hunk-Based Transfer)
1. Add `run_diff_tree_with_patches()` function that runs `git diff-tree --stdin -p -U0`
2. Parse output to extract per-commit, per-file hunk headers
3. Implement `apply_hunk_offsets_to_attributions()` that adjusts line ranges
4. Replace the content-diff loop with hunk-based transfer (N2+), keep content-diff for N1
5. Skip blob content reading for files where only hunk-based transfer is needed

### Phase 3B: Implement IDEA B (Parallel Processing)
1. Restructure the inner file loop to collect independent work items
2. Process files in parallel using smol::spawn or rayon
3. Collect results and merge back into shared state

### Phase 3C: Implement IDEA C (Fast-Path Detection)
1. Add prefix/suffix check before full diff
2. Handle append-only and single-insertion cases with arithmetic

### Verification
- Run existing integration tests to ensure correctness
- Run heavy benchmark to measure improvement
- Compare attribution output before/after optimization
