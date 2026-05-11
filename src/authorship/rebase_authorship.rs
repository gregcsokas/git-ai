use crate::authorship::authorship_log_serialization::AuthorshipLog;
use crate::authorship::post_commit;
use crate::authorship::rebase_ops;
use crate::authorship::rebase_types::{
    CommitObjectMetadata, HunksByCommitAndFile, RebaseNoteCache,
};

// Re-export for backward compatibility with tests
pub use crate::authorship::rebase_ops::diff_based_line_attribution_transfer;
use crate::error::GitAiError;
use crate::git::authorship_traversal::{
    commits_have_authorship_notes, load_ai_touched_files_for_commits,
};
use crate::git::refs::{
    batch_read_blob_contents, get_reference_as_authorship_log_v3, note_blob_oids_for_commits,
};
use crate::git::repository::{CommitRange, Repository, exec_git, exec_git_stdin};
use crate::git::rewrite_log::RewriteLogEvent;
use std::collections::{BTreeMap, HashMap, HashSet};

#[doc(hidden)]
pub fn load_rebase_note_cache(
    repo: &Repository,
    original_commits: &[String],
    new_commits: &[String],
) -> Result<RebaseNoteCache, GitAiError> {
    // Step 1: Get note blob OIDs for both original and new commits in one batch call.
    // We interleave them to make a single cat-file --batch-check call.
    let mut all_commits = Vec::with_capacity(original_commits.len() + new_commits.len());
    all_commits.extend(original_commits.iter().cloned());
    all_commits.extend(new_commits.iter().cloned());
    let all_note_oids = note_blob_oids_for_commits(repo, &all_commits)?;

    let mut original_note_blob_oids = HashMap::new();
    let mut new_commit_note_blob_oids: HashMap<String, String> = HashMap::new();

    for commit in original_commits {
        if let Some(oid) = all_note_oids.get(commit) {
            original_note_blob_oids.insert(commit.clone(), oid.clone());
        }
    }
    for commit in new_commits {
        if let Some(oid) = all_note_oids.get(commit) {
            new_commit_note_blob_oids.insert(commit.clone(), oid.clone());
        }
    }

    // Step 2: Read all note blob contents (original + new) in one batch call.
    let mut unique_blob_oids: Vec<String> = original_note_blob_oids
        .values()
        .chain(new_commit_note_blob_oids.values())
        .cloned()
        .collect::<HashSet<_>>()
        .into_iter()
        .collect();
    unique_blob_oids.sort();
    let blob_contents = batch_read_blob_contents(repo, &unique_blob_oids)?;

    // A new commit's note only counts as "already processed" when it has actual
    // attestations.  Empty notes (no attestations) arise when a post-commit hook
    // fires during `rebase --continue` for a human-resolved conflict commit —
    // in that case we must still run the slow-path rewrite to transfer attribution
    // for any AI lines that survived the merge.
    let mut new_commits_with_notes = HashSet::new();
    for (commit, blob_oid) in &new_commit_note_blob_oids {
        if let Some(content) = blob_contents.get(blob_oid)
            && let Ok(log) = AuthorshipLog::deserialize_from_string(content)
            && !log.attestations.is_empty()
        {
            new_commits_with_notes.insert(commit.clone());
        }
    }

    let mut original_note_contents = HashMap::new();
    let mut ai_touched_files = HashSet::new();

    for (commit_sha, blob_oid) in &original_note_blob_oids {
        if let Some(content) = blob_contents.get(blob_oid) {
            original_note_contents.insert(commit_sha.clone(), content.clone());
            // Extract AI-touched file paths from this note
            crate::git::authorship_traversal::extract_file_paths_from_note_public(
                content,
                &mut ai_touched_files,
            );
        }
    }

    Ok(RebaseNoteCache {
        new_commits_with_notes,
        original_note_blob_oids,
        original_note_contents,
        ai_touched_files,
    })
}

// Process events in the rewrite log and call the correct rewrite functions in this file
pub fn rewrite_authorship_if_needed(
    repo: &Repository,
    last_event: &RewriteLogEvent,
    commit_author: String,
    _full_log: &Vec<RewriteLogEvent>,
    supress_output: bool,
) -> Result<(), GitAiError> {
    match last_event {
        RewriteLogEvent::Commit { commit } => {
            // This is going to become the regualar post-commit
            post_commit::post_commit(
                repo,
                commit.base_commit.clone(),
                commit.commit_sha.clone(),
                commit_author,
                supress_output,
            )?;
        }
        RewriteLogEvent::CommitAmend { commit_amend } => {
            rewrite_authorship_after_commit_amend(
                repo,
                &commit_amend.original_commit,
                &commit_amend.amended_commit_sha,
                commit_author,
            )?;

            tracing::debug!(
                "Ammended commit {} now has authorship log {}",
                &commit_amend.original_commit,
                &commit_amend.amended_commit_sha
            );
        }
        RewriteLogEvent::MergeSquash { merge_squash } => {
            let current_head = repo
                .head()
                .ok()
                .and_then(|head| head.target().ok())
                .map(|oid| oid.to_string());
            if current_head.as_deref() != Some(merge_squash.base_head.as_str()) {
                tracing::debug!(
                    "Skipping merge --squash pre-commit prep because repo head already advanced past {}",
                    merge_squash.base_head
                );
                return Ok(());
            }
            // --squash always fails if repo is not clean
            // this clears old working logs in the event you reset, make manual changes, reset, try again
            repo.storage
                .delete_working_log_for_base_commit(&merge_squash.base_head)?;
            if merge_squash.staged_file_blobs.is_empty() {
                tracing::debug!(
                    "Skipping immediate merge --squash pre-commit prep for {} because no staged snapshot was captured; commit replay will reconstruct from the committed final state",
                    merge_squash.base_head
                );
                return Ok(());
            }

            // Prepare INITIAL attributions from the squashed changes
            prepare_working_log_after_squash(
                repo,
                &merge_squash.source_head,
                &merge_squash.base_head,
                &merge_squash.staged_file_blobs,
                &commit_author,
            )?;

            tracing::debug!(
                "✓ Prepared authorship attributions for merge --squash of {} into {}",
                merge_squash.source_branch,
                merge_squash.base_branch
            );
        }
        RewriteLogEvent::RebaseComplete { rebase_complete } => {
            // Fix #1079: fetch missing notes before attribution rewriting so that
            // daemon mode has the same remote-note resolution as wrapper mode.
            // This mirrors the fix applied to CherryPickComplete in #955.
            crate::git::sync_authorship::fetch_missing_notes_for_commits(
                repo,
                &rebase_complete.original_commits,
            );
            rewrite_authorship_after_rebase_v2(
                repo,
                &rebase_complete.original_head,
                &rebase_complete.original_commits,
                &rebase_complete.new_commits,
                &commit_author,
            )?;

            migrate_working_log_after_rebase(
                repo,
                &rebase_complete.original_head,
                &rebase_complete.new_head,
            )?;

            tracing::debug!(
                "✓ Rewrote authorship for {} rebased commits",
                rebase_complete.new_commits.len()
            );
        }
        RewriteLogEvent::CherryPickComplete {
            cherry_pick_complete,
        } => {
            // Fix #955: fetch missing notes before attribution rewriting so that
            // daemon mode has the same remote-note resolution as wrapper mode.
            crate::git::sync_authorship::fetch_missing_notes_for_commits(
                repo,
                &cherry_pick_complete.source_commits,
            );
            rewrite_authorship_after_cherry_pick(
                repo,
                &cherry_pick_complete.source_commits,
                &cherry_pick_complete.new_commits,
                &commit_author,
            )?;

            tracing::debug!(
                "✓ Rewrote authorship for {} cherry-picked commits",
                cherry_pick_complete.new_commits.len()
            );
        }
        _ => {}
    }

    Ok(())
}

/// Migrate working log from the pre-rebase HEAD to the post-rebase HEAD.
/// Rebase rewrites commit SHAs, but working logs are keyed by SHA. Without this
/// migration, uncommitted attributions stored in the working log are orphaned on
/// the old SHA and silently lost when the developer eventually commits.
///
/// When only the old working log exists, the entire directory is renamed (preserving
/// INITIAL, checkpoints, and any other data). When both old and new directories
/// exist, only INITIAL attributions are merged into the new directory -- checkpoints
/// from the old directory are intentionally dropped because the new directory's
/// checkpoints already reflect the post-rebase state.
fn migrate_working_log_after_rebase(
    repo: &Repository,
    original_head: &str,
    new_head: &str,
) -> Result<(), GitAiError> {
    if original_head == new_head {
        return Ok(());
    }

    if !repo.storage.has_working_log(original_head) {
        return Ok(());
    }

    if !repo.storage.has_working_log(new_head) {
        repo.storage.rename_working_log(original_head, new_head)?;
    } else {
        let old_wl = repo.storage.working_log_for_base_commit(original_head)?;
        let initial = old_wl.read_initial_attributions();
        if !initial.files.is_empty() {
            let new_wl = repo.storage.working_log_for_base_commit(new_head)?;
            new_wl.write_initial(initial)?;
            tracing::debug!(
                "Migrated INITIAL attributions from {} to {}",
                original_head,
                new_head
            );
        } else {
            tracing::debug!(
                "No INITIAL attributions to migrate from {} (dropping old working log)",
                original_head
            );
        }
        repo.storage
            .delete_working_log_for_base_commit(original_head)?;
    }

    Ok(())
}

/// Prepare working log after a merge --squash (before commit)
///
/// This handles the case where `git merge --squash` has staged changes but hasn't committed yet.
/// Uses VirtualAttributions to merge attributions from both branches and writes everything to INITIAL
/// since merge squash leaves all changes unstaged.
///
/// # Arguments
/// * `repo` - Git repository
/// * `source_head_sha` - SHA of the feature branch that was squashed
/// * `target_branch_head_sha` - SHA of the current HEAD (target branch where we're merging into)
/// * `_human_author` - The human author identifier (unused in current implementation)
pub fn prepare_working_log_after_squash(
    repo: &Repository,
    source_head_sha: &str,
    target_branch_head_sha: &str,
    staged_file_blobs: &HashMap<String, String>,
    _human_author: &str,
) -> Result<(), GitAiError> {
    use crate::authorship::virtual_attribution::{
        VirtualAttributions, merge_attributions_favoring_first,
    };

    // Step 1: Find merge base between source and target to optimize blame
    // We only need to look at commits after the merge base, not entire history
    let merge_base = repo
        .merge_base(
            source_head_sha.to_string(),
            target_branch_head_sha.to_string(),
        )
        .ok();

    // Step 2: Get list of changed files between the two branches
    let changed_files = repo.diff_changed_files(source_head_sha, target_branch_head_sha)?;

    if changed_files.is_empty() {
        // No files changed, nothing to do
        return Ok(());
    }

    // Step 3: Create VirtualAttributions for both branches
    // Use merge_base to limit blame range for performance
    let repo_clone = repo.clone();
    let merge_base_clone = merge_base.clone();
    let source_va = smol::block_on(async {
        VirtualAttributions::new_for_base_commit(
            repo_clone,
            source_head_sha.to_string(),
            &changed_files,
            merge_base_clone,
        )
        .await
    })?;

    let repo_clone = repo.clone();
    let target_va = smol::block_on(async {
        VirtualAttributions::new_for_base_commit(
            repo_clone,
            target_branch_head_sha.to_string(),
            &changed_files,
            merge_base,
        )
        .await
    })?;

    // Step 3: Materialize the staged snapshot captured with the squash event.
    let mut blob_oids: Vec<String> = changed_files
        .iter()
        .filter_map(|file_path| staged_file_blobs.get(file_path).cloned())
        .collect();
    blob_oids.sort();
    blob_oids.dedup();
    let blob_contents = batch_read_blob_contents(repo, &blob_oids)?;

    let mut staged_files = HashMap::new();
    for file_path in &changed_files {
        let Some(blob_oid) = staged_file_blobs.get(file_path) else {
            continue;
        };
        if let Some(content) = blob_contents.get(blob_oid) {
            staged_files.insert(file_path.clone(), content.clone());
        }
    }

    // Step 4: Merge VirtualAttributions, favoring target branch (HEAD)
    let merged_va = merge_attributions_favoring_first(target_va, source_va, staged_files)?;

    // Step 5: Convert to INITIAL (everything is uncommitted in a squash).
    // This must stay independent of the live worktree because daemon replay may lag behind
    // later user edits.
    let initial_attributions = merged_va.to_initial_working_log_only();

    // Step 6: Write INITIAL file
    if !initial_attributions.files.is_empty() {
        let working_log = repo
            .storage
            .working_log_for_base_commit(target_branch_head_sha)?;
        let initial_file_contents =
            merged_va.snapshot_contents_for_files(initial_attributions.files.keys());
        working_log.write_initial_attributions_with_contents(
            initial_attributions.files,
            initial_attributions.prompts,
            initial_attributions.humans,
            initial_file_contents,
            initial_attributions.sessions,
        )?;
    }

    Ok(())
}

pub fn prepare_working_log_after_squash_from_final_state(
    repo: &Repository,
    source_head_sha: &str,
    target_branch_head_sha: &str,
    final_state: &HashMap<String, String>,
    _human_author: &str,
) -> Result<(), GitAiError> {
    use crate::authorship::virtual_attribution::{
        VirtualAttributions, merge_attributions_favoring_first,
    };

    let merge_base = repo
        .merge_base(
            source_head_sha.to_string(),
            target_branch_head_sha.to_string(),
        )
        .ok();

    let changed_files = repo.diff_changed_files(source_head_sha, target_branch_head_sha)?;
    if changed_files.is_empty() {
        return Ok(());
    }

    let repo_clone = repo.clone();
    let merge_base_clone = merge_base.clone();
    let source_va = smol::block_on(async {
        VirtualAttributions::new_for_base_commit(
            repo_clone,
            source_head_sha.to_string(),
            &changed_files,
            merge_base_clone,
        )
        .await
    })?;

    let repo_clone = repo.clone();
    let target_va = smol::block_on(async {
        VirtualAttributions::new_for_base_commit(
            repo_clone,
            target_branch_head_sha.to_string(),
            &changed_files,
            merge_base,
        )
        .await
    })?;

    let squash_files = changed_files
        .iter()
        .filter_map(|file_path| {
            final_state
                .get(file_path)
                .cloned()
                .map(|content| (file_path.clone(), content))
        })
        .collect::<HashMap<_, _>>();

    let merged_va = merge_attributions_favoring_first(target_va, source_va, squash_files)?;
    let initial_attributions = merged_va.to_initial_working_log_only();

    if !initial_attributions.files.is_empty() {
        let working_log = repo
            .storage
            .working_log_for_base_commit(target_branch_head_sha)?;
        let initial_file_contents =
            merged_va.snapshot_contents_for_files(initial_attributions.files.keys());
        working_log.write_initial_attributions_with_contents(
            initial_attributions.files,
            initial_attributions.prompts,
            initial_attributions.humans,
            initial_file_contents,
            initial_attributions.sessions,
        )?;
    }

    Ok(())
}

/// Restore carried-over uncommitted authorship after an async head/base transition.
///
/// This uses only persisted working-log state from `old_head`, persisted state already present on
/// `new_head`, and the exact final file contents captured at command exit.
pub fn restore_working_log_carryover(
    repo: &Repository,
    old_head: &str,
    new_head: &str,
    final_state: HashMap<String, String>,
    human_author: Option<String>,
) -> Result<(), GitAiError> {
    if old_head.is_empty() || new_head.is_empty() || final_state.is_empty() {
        return Ok(());
    }

    let old_va =
        crate::authorship::virtual_attribution::VirtualAttributions::from_persisted_working_log(
            repo.clone(),
            old_head.to_string(),
            human_author,
        )?;
    restore_virtual_attribution_carryover(repo, new_head, old_va, final_state)
}

pub fn restore_virtual_attribution_carryover(
    repo: &Repository,
    new_head: &str,
    carried_va: crate::authorship::virtual_attribution::VirtualAttributions,
    final_state: HashMap<String, String>,
) -> Result<(), GitAiError> {
    if new_head.is_empty() || final_state.is_empty() || carried_va.attributions.is_empty() {
        return Ok(());
    }

    let new_va =
        crate::authorship::virtual_attribution::VirtualAttributions::from_persisted_working_log(
            repo.clone(),
            new_head.to_string(),
            None,
        )
        .unwrap_or_else(|_| {
            crate::authorship::virtual_attribution::VirtualAttributions::new(
                repo.clone(),
                new_head.to_string(),
                HashMap::new(),
                HashMap::new(),
                0,
            )
        });

    let merged_va = crate::authorship::virtual_attribution::merge_attributions_favoring_first(
        carried_va,
        new_va,
        final_state.clone(),
    )?;
    let initial_attributions = merged_va.to_initial_working_log_only();
    if initial_attributions.files.is_empty()
        && initial_attributions.prompts.is_empty()
        && initial_attributions.sessions.is_empty()
    {
        return Ok(());
    }

    let working_log = repo.storage.working_log_for_base_commit(new_head)?;
    working_log.write_initial_attributions_with_contents(
        initial_attributions.files,
        initial_attributions.prompts,
        initial_attributions.humans,
        final_state,
        initial_attributions.sessions,
    )?;
    Ok(())
}

/// Rewrite authorship after a squash or rebase merge performed in CI/GUI
///
/// This handles the case where a squash merge or rebase merge was performed via SCM GUI,
/// and we need to reconstruct authorship after the fact. Unlike `prepare_working_log_after_squash`,
/// this writes directly to the authorship log (git notes) since the merge is already committed.
///
/// # Arguments
/// * `repo` - Git repository
/// * `_head_ref` - Reference name of the source branch (e.g., "feature/123")
/// * `merge_ref` - Reference name of the target/base branch (e.g., "main")
/// * `source_head_sha` - SHA of the source branch head that was merged
/// * `merge_commit_sha` - SHA of the final merge commit
/// * `_suppress_output` - Whether to suppress output (unused, kept for API compatibility)
pub fn rewrite_authorship_after_squash_or_rebase(
    repo: &Repository,
    _head_ref: &str,
    merge_ref: &str,
    source_head_sha: &str,
    merge_commit_sha: &str,
    _suppress_output: bool,
) -> Result<(), GitAiError> {
    use crate::authorship::virtual_attribution::{
        VirtualAttributions, merge_attributions_favoring_first,
    };

    // Step 1: Get target branch head (first parent on merge_ref)
    // This is more correct than just parent(0) in cases with complex back-and-forth merge history
    let merge_commit = repo.find_commit(merge_commit_sha.to_string())?;
    let target_branch_head = if merge_commit.parent_count()? == 1 {
        // For single-parent commits (squash merges), there's no ambiguity - use the only parent
        // This avoids issues in partial clones where parent_on_refname might fail
        merge_commit.parent(0)?
    } else {
        // For multi-parent commits, find the parent that's on the target branch
        merge_commit.parent_on_refname(merge_ref)?
    };
    let target_branch_head_sha = target_branch_head.id().to_string();

    tracing::debug!(
        "Rewriting authorship for squash/rebase merge: {} -> {}",
        source_head_sha,
        merge_commit_sha
    );

    // Step 2: Find merge base between source and target to optimize blame
    // We only need to look at commits after the merge base, not entire history
    let merge_base = repo
        .merge_base(
            source_head_sha.to_string(),
            target_branch_head_sha.to_string(),
        )
        .ok();

    // Step 3: Get list of changed files between the two branches
    let changed_files = repo.diff_changed_files(source_head_sha, &target_branch_head_sha)?;

    // Get commits from source branch (from source_head back to merge_base)
    // Uses git rev-list which safely handles the range without infinite walking
    let source_commits = if let Some(ref base) = merge_base {
        let range =
            CommitRange::new_infer_refname(repo, base.clone(), source_head_sha.to_string(), None)?;
        range.all_commits()
    } else {
        vec![source_head_sha.to_string()]
    };
    let changed_files =
        filter_pathspecs_to_ai_touched_files(repo, &source_commits, &changed_files)?;

    if changed_files.is_empty() {
        if commits_have_authorship_notes(repo, &source_commits)? {
            tracing::debug!(
                "No AI-touched files in merge, but notes exist in source commits; writing empty authorship log",
            );
            if let Some(authorship_log) = build_metadata_only_authorship_log_from_source_notes(
                repo,
                &source_commits,
                merge_commit_sha,
            )? {
                let authorship_json = authorship_log.serialize_to_string().map_err(|_| {
                    GitAiError::Generic("Failed to serialize authorship log".to_string())
                })?;
                crate::git::refs::notes_add(repo, merge_commit_sha, &authorship_json)?;
            }
        } else {
            // No files changed, nothing to do
            tracing::debug!("No files changed in merge, skipping authorship rewrite");
        }
        return Ok(());
    }

    tracing::debug!(
        "Processing {} changed files for merge authorship",
        changed_files.len()
    );

    // Step 4: Create VirtualAttributions for both branches
    // Use merge_base to limit blame range for performance
    let repo_clone = repo.clone();
    let merge_base_clone = merge_base.clone();
    let source_va = smol::block_on(async {
        VirtualAttributions::new_for_base_commit(
            repo_clone,
            source_head_sha.to_string(),
            &changed_files,
            merge_base_clone,
        )
        .await
    })?;

    let repo_clone = repo.clone();
    let target_va = smol::block_on(async {
        VirtualAttributions::new_for_base_commit(
            repo_clone,
            target_branch_head_sha.clone(),
            &changed_files,
            merge_base,
        )
        .await
    })?;

    // Step 4: Read committed files from merge commit (captures final state with conflict resolutions)
    let committed_files = get_committed_files_content(repo, merge_commit_sha, &changed_files)?;

    tracing::debug!(
        "Read {} committed files from merge commit",
        committed_files.len()
    );

    // Step 5: Merge VirtualAttributions, favoring target branch (base)
    let merged_va = merge_attributions_favoring_first(target_va, source_va, committed_files)?;

    // Step 6: Convert to AuthorshipLog (everything is committed in CI merge)
    let mut authorship_log = merged_va.to_authorship_log()?;
    authorship_log.metadata.base_commit_sha = merge_commit_sha.to_string();

    // Preserve accumulated totals from source commits (squash/rebase should not drop session totals).
    let mut summed_totals: HashMap<String, (u32, u32)> = HashMap::new();
    for commit_sha in &source_commits {
        if let Ok(log) = get_reference_as_authorship_log_v3(repo, commit_sha) {
            for (prompt_id, record) in log.metadata.prompts {
                let entry = summed_totals.entry(prompt_id).or_insert((0, 0));
                entry.0 = entry.0.saturating_add(record.total_additions);
                entry.1 = entry.1.saturating_add(record.total_deletions);
            }
            for (hash, record) in log.metadata.humans {
                authorship_log.metadata.humans.entry(hash).or_insert(record);
            }
            for (id, record) in log.metadata.sessions {
                authorship_log.metadata.sessions.entry(id).or_insert(record);
            }
        }
    }

    for (prompt_id, record) in authorship_log.metadata.prompts.iter_mut() {
        if let Some((additions, deletions)) = summed_totals.get(prompt_id) {
            record.total_additions = *additions;
            record.total_deletions = *deletions;
        }
    }

    tracing::debug!(
        "Created authorship log with {} attestations, {} prompts",
        authorship_log.attestations.len(),
        authorship_log.metadata.prompts.len()
    );

    // Step 7: Save authorship log to git notes
    let authorship_json = authorship_log
        .serialize_to_string()
        .map_err(|_| GitAiError::Generic("Failed to serialize authorship log".to_string()))?;

    crate::git::refs::notes_add(repo, merge_commit_sha, &authorship_json)?;

    tracing::debug!(
        "✓ Saved authorship log for merge commit {}",
        merge_commit_sha
    );

    Ok(())
}

/// Pair original commits with new (rebased) commits for authorship rewriting.
///
/// When the counts are equal we use positional pairing (the common case for a
/// normal rebase where every original commit becomes exactly one new commit).
///
/// When counts differ — which happens when an interactive rebase *drops* one or
/// more commits — positional pairing is wrong: e.g. with originals [A, B, C] and
/// new commits [A′, C′] (B was dropped), a positional zip gives [(A,A′),(B,C′)]
/// so C′ is incorrectly attributed using B's note instead of C's.
///
/// We fix this by matching each new commit to the first unused original commit
/// that has the same subject line (first line of the commit message).  If no
/// subject match is found we fall back to the next positionally-available original
/// so that the pairing is never shorter than `new_commits`.
fn pair_commits_for_rewrite(
    repo: &Repository,
    original_commits: &[String],
    new_commits: &[String],
) -> Vec<(String, String)> {
    if original_commits.len() == new_commits.len() {
        // Equal length: positional pairing is correct and avoids extra git calls.
        return original_commits
            .iter()
            .zip(new_commits.iter())
            .map(|(a, b)| (a.clone(), b.clone()))
            .collect();
    }

    // Unequal length (dropped or squashed commits): match by commit subject.
    let original_subjects: Vec<(String, String)> = original_commits
        .iter()
        .map(|sha| {
            let subject = repo
                .find_commit(sha.clone())
                .and_then(|c| c.summary())
                .unwrap_or_default();
            (sha.clone(), subject)
        })
        .collect();

    let mut used: HashSet<String> = HashSet::new();
    let mut pairs: Vec<(String, String)> = Vec::with_capacity(new_commits.len());

    for new_sha in new_commits {
        let new_subject = repo
            .find_commit(new_sha.clone())
            .and_then(|c| c.summary())
            .unwrap_or_default();

        // Prefer an unused original with the same subject.
        let matched = original_subjects.iter().find(|(orig_sha, orig_subject)| {
            !used.contains(orig_sha) && *orig_subject == new_subject
        });

        let orig_sha = if let Some((orig_sha, _)) = matched {
            orig_sha.clone()
        } else {
            // No subject match — fall back to the next positionally-available
            // unused original so every new commit gets a pairing.
            match original_subjects
                .iter()
                .find(|(orig_sha, _)| !used.contains(orig_sha))
            {
                Some((orig_sha, _)) => orig_sha.clone(),
                None => {
                    // All originals consumed (shouldn't happen in practice).
                    continue;
                }
            }
        };

        used.insert(orig_sha.clone());
        pairs.push((orig_sha, new_sha.clone()));
    }

    pairs
}

pub fn rewrite_authorship_after_rebase_v2(
    repo: &Repository,
    original_head: &str,
    original_commits: &[String],
    new_commits: &[String],
    _human_author: &str,
) -> Result<(), GitAiError> {
    let rewrite_start = std::time::Instant::now();
    let mut timing_phases: Vec<(String, u128)> = Vec::new();
    // Handle edge case: no commits to process
    if new_commits.is_empty() {
        return Ok(());
    }

    // Load all note data upfront in a single pass (eliminates ~6 redundant git subprocess calls).
    let phase_start = std::time::Instant::now();
    let note_cache = load_rebase_note_cache(repo, original_commits, new_commits)?;
    timing_phases.push((
        "load_rebase_note_cache".to_string(),
        phase_start.elapsed().as_millis(),
    ));
    tracing::debug!(
        "rebase_v2: loaded note cache ({} original notes, {} new with notes) in {}ms",
        note_cache.original_note_contents.len(),
        note_cache.new_commits_with_notes.len(),
        phase_start.elapsed().as_millis()
    );

    // Save a pre-rebase snapshot for recovery (lightweight: only if notes exist)
    if !note_cache.original_note_contents.is_empty() {
        let new_head = new_commits.last().cloned().unwrap_or_default();
        let snapshot = crate::authorship::rebase_recovery::RebaseSnapshot::new(
            original_head.to_string(),
            new_head,
            original_commits.to_vec(),
            &note_cache.original_note_contents,
        );
        if let Err(e) = crate::authorship::rebase_recovery::save_snapshot(&repo.storage, &snapshot)
        {
            tracing::debug!("rebase_v2: failed to save recovery snapshot: {}", e);
        }
    }

    // Filter out commits that already have authorship logs (these are commits from the target branch).
    let force_process_existing_notes = original_commits.len() > new_commits.len();
    let commits_to_process: Vec<String> = new_commits
        .iter()
        .filter(|commit| {
            let has_log = !force_process_existing_notes
                && note_cache.new_commits_with_notes.contains(commit.as_str());
            if has_log {
                tracing::debug!("Skipping commit {} (already has authorship log)", commit);
            }
            !has_log
        })
        .cloned()
        .collect();

    if commits_to_process.is_empty() {
        tracing::debug!("No new commits to process (all commits already have authorship logs)");
        return Ok(());
    }

    tracing::debug!(
        "Processing {} newly created commits (skipped {} existing commits)",
        commits_to_process.len(),
        new_commits.len() - commits_to_process.len()
    );
    let commits_to_process_lookup: HashSet<&str> =
        commits_to_process.iter().map(String::as_str).collect();
    let all_commit_pairs = pair_commits_for_rewrite(repo, original_commits, new_commits);
    let commit_pairs_to_process: Vec<(String, String)> = all_commit_pairs
        .into_iter()
        .filter(|(_original_commit, new_commit)| {
            commits_to_process_lookup.contains(new_commit.as_str())
        })
        .collect();
    let original_commits_for_processing: Vec<String> = commit_pairs_to_process
        .iter()
        .map(|(original_commit, _new_commit)| original_commit.clone())
        .collect();

    // Step 1: Use AI-touched files directly from the note cache as pathspecs.
    // This eliminates a diff-tree --stdin subprocess call entirely.
    // The collect_changed_file_contents step will correctly filter to only files that changed.
    let pathspecs: Vec<String> = note_cache.ai_touched_files.iter().cloned().collect();
    timing_phases.push((
        format!("pathspecs_from_note_cache ({} files)", pathspecs.len()),
        0,
    ));

    if pathspecs.is_empty() {
        // No AI-touched files were rewritten. Preserve metadata-only / prompt-only notes by remapping
        // existing source notes to their corresponding rebased commits.
        // Use cached note contents instead of loading again.
        let original_note_contents: HashMap<String, String> = original_commits_for_processing
            .iter()
            .filter_map(|commit| {
                note_cache
                    .original_note_contents
                    .get(commit)
                    .map(|content| (commit.clone(), content.clone()))
            })
            .collect();
        let remapped_count =
            remap_notes_for_commit_pairs(repo, &commit_pairs_to_process, &original_note_contents)?;
        if remapped_count > 0 {
            tracing::debug!(
                "Remapped {} metadata-only authorship notes for rebase commits",
                remapped_count
            );
        } else {
            tracing::debug!("No AI-touched files and no source notes to remap during rebase");
        }
        return Ok(());
    }

    tracing::debug!(
        "Processing rebase: {} files modified across {} original commits -> {} new commits",
        pathspecs.len(),
        original_commits.len(),
        new_commits.len()
    );

    if try_fast_path_rebase_note_remap_cached(
        repo,
        original_commits,
        new_commits,
        &commits_to_process_lookup,
        &pathspecs,
        &note_cache,
    )? {
        return Ok(());
    }

    // Step 2: Get hunks between original→new pairs (what changed during rebase).
    // These hunks tell us how line numbers shifted between original and rebased versions.
    let phase_start = std::time::Instant::now();
    let hunks_by_commit =
        run_diff_tree_hunks_for_pairs(repo, &commit_pairs_to_process, &pathspecs)?;
    timing_phases.push((
        format!(
            "diff_tree_hunks_pairs ({} pairs)",
            commit_pairs_to_process.len()
        ),
        phase_start.elapsed().as_millis(),
    ));

    // Pre-compute parent SHAs for conflict working-log lookup.
    let commit_parent_shas: HashMap<String, String> = {
        let mut map = HashMap::new();
        for sha in &commits_to_process {
            if let Ok(commit) = repo.find_commit(sha.clone())
                && let Ok(parent) = commit.parent(0)
            {
                map.insert(sha.clone(), parent.id());
            }
        }
        map
    };

    // Step 3: For each commit pair, apply hunks to the original note's attestations.
    let mut pending_note_entries: Vec<(String, String)> =
        Vec::with_capacity(commits_to_process.len());

    for (original_commit, new_commit) in &commit_pairs_to_process {
        let Some(raw_note) = note_cache.original_note_contents.get(original_commit) else {
            continue;
        };

        let commit_hunks = hunks_by_commit.get(new_commit.as_str());
        let has_hunks = commit_hunks.is_some_and(|h| !h.is_empty());

        if !has_hunks {
            // No hunks = content unchanged, just remap the base_commit_sha.
            pending_note_entries.push((
                new_commit.clone(),
                remap_note_content_for_target_commit(raw_note, new_commit),
            ));
            continue;
        }

        // Parse the original note to get attestations and metadata.
        let Ok(mut authorship_log) = AuthorshipLog::deserialize_from_string(raw_note) else {
            // Can't parse — just remap as-is.
            pending_note_entries.push((
                new_commit.clone(),
                remap_note_content_for_target_commit(raw_note, new_commit),
            ));
            continue;
        };

        // Apply hunks to each file's attestation line ranges.
        let file_hunks = commit_hunks.unwrap();
        for file_attestation in &mut authorship_log.attestations {
            let Some(hunks) = file_hunks.get(file_attestation.file_path.as_str()) else {
                continue;
            };
            let mut line_attrs: Vec<crate::authorship::attribution_tracker::LineAttribution> =
                Vec::new();
            for entry in &file_attestation.entries {
                for range in &entry.line_ranges {
                    let (start, end) = match range {
                        crate::authorship::authorship_log::LineRange::Single(l) => (*l, *l),
                        crate::authorship::authorship_log::LineRange::Range(s, e) => (*s, *e),
                    };
                    line_attrs.push(crate::authorship::attribution_tracker::LineAttribution {
                        start_line: start,
                        end_line: end,
                        author_id: entry.hash.clone(),
                        overrode: None,
                    });
                }
            }

            let shifted = apply_hunks_to_line_attributions(&line_attrs, hunks);

            if let Some(new_attestation) =
                build_file_attestation_from_line_attributions(&file_attestation.file_path, &shifted)
            {
                file_attestation.entries = new_attestation.entries;
            } else {
                file_attestation.entries.clear();
            }
        }

        // Remove attestations with no remaining entries.
        authorship_log
            .attestations
            .retain(|a| !a.entries.is_empty());

        // Update base_commit_sha.
        authorship_log.metadata.base_commit_sha = new_commit.clone();

        if !authorship_log.attestations.is_empty() {
            if let Ok(json) = authorship_log.serialize_to_string() {
                pending_note_entries.push((new_commit.clone(), json));
            }
        } else {
            // All attestations removed by hunks — try conflict working-log first
            // (AI may have resolved conflicts producing new content).
            let wl_note = commit_parent_shas
                .get(new_commit.as_str())
                .and_then(|parent_sha| {
                    build_note_from_conflict_wl(
                        repo,
                        new_commit,
                        parent_sha,
                        &note_cache.ai_touched_files,
                    )
                });
            if let Some(json) = wl_note {
                pending_note_entries.push((new_commit.clone(), json));
            } else {
                // No working-log either — just remap metadata.
                pending_note_entries.push((
                    new_commit.clone(),
                    remap_note_content_for_target_commit(raw_note, new_commit),
                ));
            }
        }
    }

    // For commits without original notes, check for conflict working-log data.
    for (_original_commit, new_commit) in &commit_pairs_to_process {
        if pending_note_entries
            .iter()
            .any(|(sha, _)| sha == new_commit)
        {
            continue; // Already handled above.
        }
        if let Some(parent_sha) = commit_parent_shas.get(new_commit.as_str())
            && let Some(json) = build_note_from_conflict_wl(
                repo,
                new_commit,
                parent_sha,
                &note_cache.ai_touched_files,
            )
        {
            pending_note_entries.push((new_commit.clone(), json));
        }
    }

    // Write all notes in one batch.
    let phase_start = std::time::Instant::now();
    if !pending_note_entries.is_empty() {
        crate::git::refs::notes_add_batch(repo, &pending_note_entries)?;
    }
    timing_phases.push((
        format!("notes_add_batch ({} entries)", pending_note_entries.len()),
        phase_start.elapsed().as_millis(),
    ));

    let total_ms = rewrite_start.elapsed().as_millis();
    tracing::debug!(
        "rebase_v2: completed in {}ms ({} notes written)",
        total_ms,
        pending_note_entries.len(),
    );

    if let Ok(timing_path) = std::env::var("GIT_AI_REBASE_TIMING_FILE") {
        let mut summary = format!("TOTAL={}ms\n", total_ms);
        for (name, ms) in &timing_phases {
            summary.push_str(&format!("  {}={}ms\n", name, ms));
        }
        let _ = std::fs::write(&timing_path, summary);
    }

    Ok(())
}

/// Rewrite authorship logs after cherry-pick using VirtualAttributions
///
/// This is the new implementation that uses VirtualAttributions to transform authorship
/// through cherry-picked commits. It's simpler than rebase since cherry-pick just applies
/// patches from source commits onto the current branch.
///
/// # Arguments
/// * `repo` - Git repository
/// * `source_commits` - Vector of source commit SHAs (commits being cherry-picked), oldest first
/// * `new_commits` - Vector of new commit SHAs (after cherry-pick), oldest first
/// * `_human_author` - The human author identifier (unused in this implementation)
pub fn rewrite_authorship_after_cherry_pick(
    repo: &Repository,
    source_commits: &[String],
    new_commits: &[String],
    _human_author: &str,
) -> Result<(), GitAiError> {
    if new_commits.is_empty() {
        return Err(GitAiError::Generic(
            "cherry-pick rewrite missing new commits".to_string(),
        ));
    }

    if source_commits.is_empty() {
        return Err(GitAiError::Generic(
            "cherry-pick rewrite missing source commits".to_string(),
        ));
    }

    if source_commits.len() != new_commits.len() {
        return Err(GitAiError::Generic(format!(
            "cherry-pick rewrite commit count mismatch source_commits={} new_commits={}",
            source_commits.len(),
            new_commits.len()
        )));
    }

    tracing::debug!(
        "Processing cherry-pick: {} source commits -> {} new commits",
        source_commits.len(),
        new_commits.len()
    );

    let commit_pairs: Vec<(String, String)> = source_commits
        .iter()
        .zip(new_commits.iter())
        .map(|(source_commit, new_commit)| (source_commit.clone(), new_commit.clone()))
        .collect();
    let source_commits_for_pairs: Vec<String> = commit_pairs
        .iter()
        .map(|(source_commit, _new_commit)| source_commit.clone())
        .collect();

    // Step 1: Extract pathspecs from all source commits
    let pathspecs = get_pathspecs_from_commits(repo, source_commits)?;
    let pathspecs = filter_pathspecs_to_ai_touched_files(repo, source_commits, &pathspecs)?;

    if pathspecs.is_empty() {
        let source_note_contents = load_note_contents_for_commits(repo, &source_commits_for_pairs)?;
        let remapped_count =
            remap_notes_for_commit_pairs(repo, &commit_pairs, &source_note_contents)?;
        if remapped_count > 0 {
            tracing::debug!(
                "Remapped {} metadata-only authorship notes for cherry-picked commits",
                remapped_count
            );
        } else {
            tracing::debug!("No files modified in source commits");
        }
        return Ok(());
    }

    if try_fast_path_cherry_pick_note_remap(repo, &commit_pairs, &pathspecs)? {
        return Ok(());
    }
    let pathspecs_lookup: HashSet<&str> = pathspecs.iter().map(String::as_str).collect();
    let mut source_note_content_by_new_commit: HashMap<String, String> = HashMap::new();
    let mut source_note_content_loaded = false;

    tracing::debug!(
        "Processing cherry-pick: {} files modified across {} source commits",
        pathspecs.len(),
        source_commits.len()
    );

    // Step 2: Create VirtualAttributions from the LAST source commit
    // This is the key difference from rebase: cherry-pick applies patches sequentially,
    // so the last source commit contains all the accumulated changes being cherry-picked
    let source_head = source_commits.last().unwrap();
    let repo_clone = repo.clone();
    let source_head_clone = source_head.clone();
    let pathspecs_clone = pathspecs.clone();

    let mut current_va = smol::block_on(async {
        crate::authorship::virtual_attribution::VirtualAttributions::new_for_base_commit(
            repo_clone,
            source_head_clone,
            &pathspecs_clone,
            None,
        )
        .await
    })?;

    // Clone the source VA to use for restoring attributions when content reappears
    // This handles commit splitting where content from source gets re-applied
    let source_head_state_va = {
        let mut attrs = HashMap::new();
        let mut contents = HashMap::new();
        for file in current_va.files() {
            if let Some(char_attrs) = current_va.get_char_attributions(&file)
                && let Some(line_attrs) = current_va.get_line_attributions(&file)
            {
                attrs.insert(file.clone(), (char_attrs.clone(), line_attrs.clone()));
            }
            if let Some(content) = current_va.get_file_content(&file) {
                contents.insert(file, content.clone());
            }
        }
        crate::authorship::virtual_attribution::VirtualAttributions::new(
            current_va.repo().clone(),
            current_va.base_commit().to_string(),
            attrs,
            contents,
            current_va.timestamp(),
        )
    };

    // Step 3: Process each new commit in order (oldest to newest)
    for (idx, new_commit) in new_commits.iter().enumerate() {
        tracing::debug!(
            "Processing cherry-picked commit {}/{}: {}",
            idx + 1,
            new_commits.len(),
            new_commit
        );

        // Get the DIFF for this commit (what actually changed)
        let commit_obj = repo.find_commit(new_commit.clone())?;
        let parent_obj = commit_obj.parent(0)?;

        let commit_tree = commit_obj.tree()?;
        let parent_tree = parent_obj.tree()?;

        let diff = repo.diff_tree_to_tree(Some(&parent_tree), Some(&commit_tree), None, None)?;

        // Build new content by applying the diff to current content
        let mut new_content_state = HashMap::new();

        // Start with all files from current VA
        for file in current_va.files() {
            if let Some(content) = current_va.get_file_content(&file) {
                new_content_state.insert(file, content.clone());
            }
        }

        // Apply changes from this commit's diff using one batched blob read.
        let (_changed_files, new_content_for_changed_files) =
            collect_changed_file_contents_from_diff(repo, &diff, &pathspecs_lookup)?;
        new_content_state.extend(new_content_for_changed_files);

        // Transform attributions based on the new content state
        // Pass source_head state to restore attributions for content that existed before cherry-pick
        current_va = transform_attributions_to_final_state(
            &current_va,
            new_content_state,
            Some(&source_head_state_va),
        )?;

        // Convert to AuthorshipLog, but filter to only files that exist in this commit
        let mut authorship_log = current_va.to_authorship_log()?;

        // Filter out attestations for files that don't exist in this commit (empty files)
        authorship_log.attestations.retain(|attestation| {
            if let Some(content) = current_va.get_file_content(&attestation.file_path) {
                !content.is_empty()
            } else {
                false
            }
        });

        authorship_log.metadata.base_commit_sha = new_commit.clone();

        // Save computed note when it has payload; otherwise preserve original metadata-only notes.
        let computed_note_has_payload = !authorship_log.attestations.is_empty()
            || !authorship_log.metadata.prompts.is_empty()
            || !authorship_log.metadata.sessions.is_empty();
        let authorship_json = if computed_note_has_payload {
            authorship_log.serialize_to_string().map_err(|_| {
                GitAiError::Generic("Failed to serialize authorship log".to_string())
            })?
        } else {
            if !source_note_content_loaded {
                source_note_content_by_new_commit =
                    load_note_contents_for_commit_pairs(repo, &commit_pairs)?;
                source_note_content_loaded = true;
            }
            if let Some(raw_note) = source_note_content_by_new_commit.get(new_commit) {
                remap_note_content_for_target_commit(raw_note, new_commit)
            } else {
                authorship_log.serialize_to_string().map_err(|_| {
                    GitAiError::Generic("Failed to serialize authorship log".to_string())
                })?
            }
        };

        crate::git::refs::notes_add(repo, new_commit, &authorship_json)?;

        tracing::debug!(
            "Saved authorship log for cherry-picked commit {} ({} files)",
            new_commit,
            authorship_log.attestations.len()
        );
    }

    Ok(())
}

/// Get file contents from a commit tree for specified pathspecs
fn get_committed_files_content(
    repo: &Repository,
    commit_sha: &str,
    pathspecs: &[String],
) -> Result<HashMap<String, String>, GitAiError> {
    use std::collections::HashMap;

    let commit = repo.find_commit(commit_sha.to_string())?;
    let tree = commit.tree()?;

    let mut files = HashMap::new();

    for file_path in pathspecs {
        match tree.get_path(std::path::Path::new(file_path)) {
            Ok(entry) => {
                if let Ok(blob) = repo.find_blob(entry.id()) {
                    let blob_content = blob.content().unwrap_or_default();
                    let content = String::from_utf8_lossy(&blob_content).to_string();
                    files.insert(file_path.clone(), content);
                }
            }
            Err(_) => {
                // File doesn't exist in this commit (could be deleted), skip it
            }
        }
    }

    Ok(files)
}

fn is_zero_oid(oid: &str) -> bool {
    !oid.is_empty() && oid.bytes().all(|b| b == b'0')
}

fn is_blob_mode(mode: &str) -> bool {
    mode.starts_with("100") || mode == "120000"
}

#[doc(hidden)]
pub fn collect_changed_file_contents_from_diff(
    repo: &Repository,
    diff: &crate::git::diff_tree_to_tree::Diff,
    pathspecs_lookup: &HashSet<&str>,
) -> Result<(HashSet<String>, HashMap<String, String>), GitAiError> {
    let mut changed_files = HashSet::new();
    let mut file_to_blob_oid: Vec<(String, Option<String>)> = Vec::new();
    let mut blob_oids = HashSet::new();

    for delta in diff.deltas() {
        let file_path = delta
            .new_file()
            .path()
            .or(delta.old_file().path())
            .ok_or_else(|| GitAiError::Generic("File path not available".to_string()))?;
        let file_path_str = file_path.to_string_lossy().to_string();

        // Only process files we're tracking.
        if !pathspecs_lookup.contains(file_path_str.as_str()) {
            continue;
        }

        changed_files.insert(file_path_str.clone());

        let new_file = delta.new_file();
        let new_blob_oid = new_file.id();
        // Keep behavior aligned with the old tree+find_blob path:
        // only regular file/symlink blobs are materialized.
        if is_zero_oid(new_blob_oid) || !is_blob_mode(new_file.mode()) {
            file_to_blob_oid.push((file_path_str, None));
            continue;
        }

        let oid = new_blob_oid.to_string();
        blob_oids.insert(oid.clone());
        file_to_blob_oid.push((file_path_str, Some(oid)));
    }

    let mut blob_oid_list: Vec<String> = blob_oids.into_iter().collect();
    blob_oid_list.sort();
    let blob_contents = batch_read_blob_contents(repo, &blob_oid_list)?;

    let mut file_contents = HashMap::new();
    for (file_path, blob_oid) in file_to_blob_oid {
        let content = blob_oid
            .as_ref()
            .and_then(|oid| blob_contents.get(oid).cloned())
            .unwrap_or_default();
        file_contents.insert(file_path, content);
    }

    Ok((changed_files, file_contents))
}

pub(crate) fn committed_file_snapshot_between_commits(
    repo: &Repository,
    from_commit: Option<&str>,
    to_commit: &str,
) -> Result<HashMap<String, String>, GitAiError> {
    let to_commit = repo.find_commit(to_commit.to_string())?;
    let to_tree = to_commit.tree()?;
    if matches!(from_commit, None | Some("initial")) {
        let mut args = repo.global_args_for_exec();
        args.push("ls-tree".to_string());
        args.push("-r".to_string());
        args.push("-z".to_string());
        args.push("--name-only".to_string());
        args.push(to_tree.id());

        let output = exec_git(&args)?;
        let tracked_paths = output
            .stdout
            .split(|byte| *byte == 0)
            .filter(|bytes| !bytes.is_empty())
            .filter_map(|bytes| String::from_utf8(bytes.to_vec()).ok())
            .collect::<Vec<_>>();
        return get_committed_files_content(repo, &to_commit.id(), &tracked_paths);
    }

    let from_tree = repo.find_commit(from_commit.unwrap().to_string())?.tree()?;
    let diff = repo.diff_tree_to_tree(Some(&from_tree), Some(&to_tree), None, None)?;
    let tracked_paths = diff
        .deltas()
        .filter_map(|delta| delta.new_file().path().or(delta.old_file().path()))
        .map(|path| path.to_string_lossy().to_string())
        .collect::<HashSet<_>>();

    if tracked_paths.is_empty() {
        return Ok(HashMap::new());
    }

    let tracked_lookup = tracked_paths
        .iter()
        .map(|path| path.as_str())
        .collect::<HashSet<_>>();
    let (_changed_files, contents) =
        collect_changed_file_contents_from_diff(repo, &diff, &tracked_lookup)?;
    Ok(contents)
}

// Re-export for backward compatibility with tests
pub use crate::git::refs::parse_cat_file_batch_output_with_oids;

fn load_commit_metadata_batch(
    repo: &Repository,
    commit_shas: &[String],
) -> Result<HashMap<String, CommitObjectMetadata>, GitAiError> {
    if commit_shas.is_empty() {
        return Ok(HashMap::new());
    }

    let mut unique_commits = Vec::new();
    let mut seen = HashSet::new();
    for commit_sha in commit_shas {
        if seen.insert(commit_sha.as_str()) {
            unique_commits.push(commit_sha.clone());
        }
    }

    let mut args = repo.global_args_for_exec();
    args.push("cat-file".to_string());
    args.push("--batch".to_string());

    let stdin_data = unique_commits.join("\n") + "\n";
    let output = exec_git_stdin(&args, stdin_data.as_bytes())?;
    let data = output.stdout;

    let mut metadata_by_commit = HashMap::new();
    let mut pos = 0usize;

    while pos < data.len() {
        let header_end = match data[pos..].iter().position(|&b| b == b'\n') {
            Some(idx) => pos + idx,
            None => break,
        };
        let header = std::str::from_utf8(&data[pos..header_end])?;
        let mut parts = header.split_whitespace();
        let oid = match parts.next() {
            Some(v) => v.to_string(),
            None => {
                pos = header_end + 1;
                continue;
            }
        };
        let object_type = parts.next().unwrap_or_default();
        if object_type == "missing" {
            pos = header_end + 1;
            continue;
        }
        let size: usize = parts
            .next()
            .ok_or_else(|| {
                GitAiError::Generic("Malformed cat-file --batch header: missing size".to_string())
            })?
            .parse()
            .map_err(|e| {
                GitAiError::Generic(format!("Invalid cat-file --batch object size: {}", e))
            })?;

        let content_start = header_end + 1;
        let content_end = content_start + size;
        if content_end > data.len() {
            return Err(GitAiError::Generic(
                "Malformed cat-file --batch output: truncated commit object".to_string(),
            ));
        }

        if object_type == "commit" {
            let content = std::str::from_utf8(&data[content_start..content_end])?;
            let mut tree_oid = String::new();

            for line in content.lines() {
                if let Some(rest) = line.strip_prefix("tree ") {
                    tree_oid = rest.trim().to_string();
                    break;
                }
            }

            metadata_by_commit.insert(oid, CommitObjectMetadata { tree_oid });
        }

        pos = content_end;
        if pos < data.len() && data[pos] == b'\n' {
            pos += 1;
        }
    }

    Ok(metadata_by_commit)
}

/// Collect changed file contents for a list of commit SHAs using a single diff-tree --stdin call.
/// Result of parsing diff-tree output: per-commit deltas and the set of all blob OIDs needed.
use rebase_ops::parse_hunk_header;

use rebase_ops::apply_hunks_to_line_attributions;

/// Diff original→new commit pairs and return per-file hunks for each new commit.
/// Uses tree-to-tree diffing via `git diff-tree` to get how lines shifted during rebase.
fn run_diff_tree_hunks_for_pairs(
    repo: &Repository,
    commit_pairs: &[(String, String)],
    pathspecs: &[String],
) -> Result<HunksByCommitAndFile, GitAiError> {
    if commit_pairs.is_empty() {
        return Ok(HashMap::new());
    }

    let mut commits_to_load: Vec<String> = Vec::with_capacity(commit_pairs.len() * 2);
    for (orig, new) in commit_pairs {
        commits_to_load.push(orig.clone());
        commits_to_load.push(new.clone());
    }
    let commit_metadata = load_commit_metadata_batch(repo, &commits_to_load)?;

    let mut base_args = repo.global_args_for_exec();
    base_args.push("diff-tree".to_string());
    base_args.push("-p".to_string());
    base_args.push("-U0".to_string());
    base_args.push("--no-color".to_string());
    base_args.push("--no-abbrev".to_string());
    base_args.push("-r".to_string());

    let mut hunks_by_commit: HunksByCommitAndFile = HashMap::new();

    for (original_commit, new_commit) in commit_pairs {
        let left_tree = match commit_metadata.get(original_commit) {
            Some(meta) if !meta.tree_oid.is_empty() => meta.tree_oid.clone(),
            _ => continue,
        };
        let right_tree = match commit_metadata.get(new_commit) {
            Some(meta) if !meta.tree_oid.is_empty() => meta.tree_oid.clone(),
            _ => continue,
        };

        if left_tree == right_tree {
            continue;
        }

        let mut pair_args = base_args.clone();
        pair_args.push(left_tree);
        pair_args.push(right_tree);
        if !pathspecs.is_empty() {
            pair_args.push("--".to_string());
            pair_args.extend(pathspecs.iter().cloned());
        }

        let output = exec_git(&pair_args)?;
        let text = String::from_utf8_lossy(&output.stdout);

        let mut current_file: Option<String> = None;
        for line in text.lines() {
            if line.starts_with("diff --git ") {
                if let Some(b_path) = line.split(" b/").last() {
                    current_file = Some(b_path.to_string());
                }
                continue;
            }
            if line.starts_with("@@ ") {
                if let Some(ref file) = current_file
                    && let Some(hunk) = parse_hunk_header(line)
                {
                    hunks_by_commit
                        .entry(new_commit.clone())
                        .or_default()
                        .entry(file.clone())
                        .or_default()
                        .push(hunk);
                }
                continue;
            }
            if line.starts_with('+')
                && !line.starts_with("+++ ")
                && let Some(ref file) = current_file
                && let Some(file_hunks) = hunks_by_commit.get_mut(new_commit)
                && let Some(hunks) = file_hunks.get_mut(file.as_str())
                && let Some(last_hunk) = hunks.last_mut()
            {
                last_hunk.added_lines.push(line[1..].to_string());
            }
        }
    }

    Ok(hunks_by_commit)
}

pub fn rewrite_authorship_after_commit_amend(
    repo: &Repository,
    original_commit: &str,
    amended_commit: &str,
    _human_author: String,
) -> Result<AuthorshipLog, GitAiError> {
    rewrite_authorship_after_commit_amend_with_snapshot(
        repo,
        original_commit,
        amended_commit,
        _human_author,
        None,
    )
}

pub fn rewrite_authorship_after_commit_amend_with_snapshot(
    repo: &Repository,
    original_commit: &str,
    amended_commit: &str,
    human_author: String,
    final_state_override: Option<&HashMap<String, String>>,
) -> Result<AuthorshipLog, GitAiError> {
    use crate::authorship::virtual_attribution::VirtualAttributions;

    // Get the files that changed between original and amended commit
    let changed_files = repo.list_commit_files(amended_commit, None)?;
    let mut pathspecs: HashSet<String> = changed_files.into_iter().collect();

    let working_log = repo.storage.working_log_for_base_commit(original_commit)?;
    let touched_files = working_log.all_touched_files()?;
    pathspecs.extend(touched_files);

    // Check if original commit has an authorship log with prompts or humans
    let has_existing_log = get_reference_as_authorship_log_v3(repo, original_commit).is_ok();
    let has_existing_data = if has_existing_log {
        let original_log = get_reference_as_authorship_log_v3(repo, original_commit).unwrap();
        !original_log.metadata.prompts.is_empty()
            || !original_log.metadata.humans.is_empty()
            || !original_log.metadata.sessions.is_empty()
    } else {
        false
    };

    // Phase 1: Load all attributions (committed + uncommitted)
    let repo_clone = repo.clone();
    let pathspecs_vec: Vec<String> = pathspecs.iter().cloned().collect();
    let working_va = if let Some(snapshot) = final_state_override {
        smol::block_on(async {
            VirtualAttributions::from_working_log_for_commit_snapshot(
                repo_clone,
                original_commit.to_string(),
                &pathspecs_vec,
                if has_existing_data {
                    None
                } else {
                    Some(human_author.clone())
                },
                None,
                snapshot,
            )
            .await
        })?
    } else {
        smol::block_on(async {
            VirtualAttributions::from_working_log_for_commit(
                repo_clone,
                original_commit.to_string(),
                &pathspecs_vec,
                if has_existing_data {
                    None
                } else {
                    Some(human_author.clone())
                },
                None,
            )
            .await
        })?
    };

    // Phase 2: Get parent of amended commit for diff calculation
    let amended_commit_obj = repo.find_commit(amended_commit.to_string())?;
    let parent_sha = if amended_commit_obj.parent_count()? > 0 {
        amended_commit_obj.parent(0)?.id().to_string()
    } else {
        "initial".to_string()
    };

    let pathspecs_set = pathspecs;

    let (mut authorship_log, initial_attributions) = working_va
        .to_authorship_log_and_initial_working_log(
            repo,
            &parent_sha,
            amended_commit,
            Some(&pathspecs_set),
            final_state_override,
        )?;

    // Update base commit SHA
    authorship_log.metadata.base_commit_sha = amended_commit.to_string();

    // Fill unattributed lines with bg agent attribution (same as post_commit path)
    if !matches!(
        crate::authorship::background_agent::detect(),
        crate::authorship::background_agent::BackgroundAgent::None
            | crate::authorship::background_agent::BackgroundAgent::WithHooks { .. }
    ) {
        let diff_base = if parent_sha == "initial" {
            "4b825dc642cb6eb9a060e54bf8d69288fbee4904"
        } else {
            &parent_sha
        };
        if let Ok(added_lines) = repo.diff_added_lines(diff_base, amended_commit, None) {
            let committed_hunks: std::collections::HashMap<
                String,
                Vec<crate::authorship::authorship_log::LineRange>,
            > = added_lines
                .into_iter()
                .filter(|(_, lines)| !lines.is_empty())
                .map(|(path, lines)| {
                    (
                        path,
                        crate::authorship::authorship_log::LineRange::compress_lines(&lines),
                    )
                })
                .collect();
            crate::authorship::background_agent::fill_unattributed_lines(
                &mut authorship_log,
                &committed_hunks,
                &human_author,
            );
        }
    }

    // Preserve human contributors from the original commit's note — deleting a
    // KnownHuman-attributed line removes the attribution coordinate but must not
    // erase the contributor's association with the commit.
    if let Ok(original_log) = get_reference_as_authorship_log_v3(repo, original_commit) {
        for (id, record) in original_log.metadata.humans {
            authorship_log.metadata.humans.entry(id).or_insert(record);
        }
        // Only preserve sessions from the original commit if they are still
        // referenced by attestations in the amended commit.
        let referenced_session_ids: std::collections::HashSet<String> = authorship_log
            .attestations
            .iter()
            .flat_map(|fa| fa.entries.iter())
            .filter_map(|entry| {
                if entry.hash.starts_with("s_") {
                    Some(
                        entry
                            .hash
                            .split("::")
                            .next()
                            .unwrap_or(&entry.hash)
                            .to_string(),
                    )
                } else {
                    None
                }
            })
            .collect();
        for (id, record) in original_log.metadata.sessions {
            if referenced_session_ids.contains(&id) {
                authorship_log.metadata.sessions.entry(id).or_insert(record);
            }
        }
    }

    // Inject custom attributes into all PromptRecords and SessionRecords (same behavior as post_commit).
    // Always use Config::fresh() to support runtime config updates
    // (especially important for daemon mode, but also good for consistency)
    let custom_attrs = crate::config::Config::fresh().custom_attributes().clone();
    if !custom_attrs.is_empty() {
        for pr in authorship_log.metadata.prompts.values_mut() {
            pr.custom_attributes = Some(custom_attrs.clone());
        }
        for sr in authorship_log.metadata.sessions.values_mut() {
            sr.custom_attributes = Some(custom_attrs.clone());
        }
    }

    // Save authorship log
    let authorship_json = authorship_log
        .serialize_to_string()
        .map_err(|_| GitAiError::Generic("Failed to serialize authorship log".to_string()))?;
    crate::git::refs::notes_add(repo, amended_commit, &authorship_json)?;

    // Save INITIAL file for uncommitted attributions
    if !initial_attributions.files.is_empty() {
        let new_working_log = repo.storage.working_log_for_base_commit(amended_commit)?;
        let initial_file_contents =
            working_va.snapshot_contents_for_files(initial_attributions.files.keys());
        new_working_log.write_initial_attributions_with_contents(
            initial_attributions.files,
            initial_attributions.prompts,
            initial_attributions.humans,
            initial_file_contents,
            initial_attributions.sessions,
        )?;
    }

    // Clean up old working log
    repo.storage
        .delete_working_log_for_base_commit(original_commit)?;

    Ok(authorship_log)
}

pub fn walk_commits_to_base(
    repository: &Repository,
    head: &str,
    base: &str,
) -> Result<Vec<String>, crate::error::GitAiError> {
    if head == base {
        return Ok(Vec::new());
    }

    // Validate commit-ish values early so callers get a clear error.
    repository.find_commit(head.to_string())?;
    repository.find_commit(base.to_string())?;

    // Guard against pathological traversals when `base` is not actually an ancestor.
    // The old BFS fallback could walk huge histories in this case.
    let mut is_ancestor_args = repository.global_args_for_exec();
    is_ancestor_args.push("merge-base".to_string());
    is_ancestor_args.push("--is-ancestor".to_string());
    is_ancestor_args.push(base.to_string());
    is_ancestor_args.push(head.to_string());
    if exec_git(&is_ancestor_args).is_err() {
        return Err(GitAiError::Generic(format!(
            "Base commit {} is not an ancestor of {}",
            base, head
        )));
    }

    // Use git's native graph walker instead of per-parent subprocess traversal.
    // Return newest->oldest so existing callers can keep their current reverse() behavior.
    let mut args = repository.global_args_for_exec();
    args.push("rev-list".to_string());
    args.push("--topo-order".to_string());
    args.push("--ancestry-path".to_string());
    args.push(format!("{}..{}", base, head));

    let output = exec_git(&args)?;
    let stdout = String::from_utf8(output.stdout)?;
    let commits = stdout
        .lines()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(ToOwned::to_owned)
        .collect();

    Ok(commits)
}

/// Get all file paths changed between two commits
/// Reconstruct working log after a reset that preserves working directory
///
/// This handles --soft, --mixed, and --merge resets where we move HEAD backward
/// but keep the working directory state. We need to create a working log that
/// captures AI authorship from the "unwound" commits plus any existing uncommitted changes.
///
/// Uses VirtualAttributions to merge AI authorship from old_head (with working log) and
/// target_commit, generating INITIAL checkpoints that seed the AI state on target_commit.
pub fn reconstruct_working_log_after_reset(
    repo: &Repository,
    target_commit_sha: &str, // Where we reset TO
    old_head_sha: &str,      // Where HEAD was BEFORE reset
    _human_author: &str,
    user_pathspecs: Option<&[String]>, // Optional user-specified pathspecs for partial reset
    final_state_override: Option<HashMap<String, String>>,
) -> Result<(), GitAiError> {
    if target_commit_sha.trim().is_empty()
        || old_head_sha.trim().is_empty()
        || is_zero_oid(target_commit_sha)
        || is_zero_oid(old_head_sha)
    {
        tracing::debug!("Skipping reset working-log reconstruction for invalid zero/empty oid");
        return Ok(());
    }

    tracing::debug!(
        "Reconstructing working log after reset from {} to {}",
        old_head_sha,
        target_commit_sha
    );

    // Step 1: Get all files changed between target and old_head
    let all_changed_files = repo.diff_changed_files(target_commit_sha, old_head_sha)?;

    // Filter to user pathspecs if provided
    let pathspecs: Vec<String> = if let Some(user_paths) = user_pathspecs {
        all_changed_files
            .into_iter()
            .filter(|f| {
                user_paths.iter().any(|p| {
                    f == p
                        || (p.ends_with('/') && f.starts_with(p))
                        || f.starts_with(&format!("{}/", p))
                })
            })
            .collect()
    } else {
        all_changed_files
    };

    // Get all commits in the range from old_head back to target (exclusive of target)
    // Uses git rev-list which safely handles the range without infinite walking
    let range = CommitRange::new_infer_refname(
        repo,
        target_commit_sha.to_string(),
        old_head_sha.to_string(),
        None,
    )?;
    let commits_in_range = range.all_commits();
    let pathspecs = filter_pathspecs_to_ai_touched_files(repo, &commits_in_range, &pathspecs)?;

    if pathspecs.is_empty() {
        tracing::debug!("No files changed between commits, nothing to reconstruct");
        // Still delete old working log
        repo.storage
            .delete_working_log_for_base_commit(old_head_sha)?;
        return Ok(());
    }

    tracing::debug!(
        "Processing {} files for reset authorship reconstruction",
        pathspecs.len()
    );

    // Step 2: Build final state from the captured command-exit snapshot when available.
    let has_captured_snapshot = final_state_override.is_some();
    let final_state = if let Some(final_state_override) = final_state_override {
        final_state_override
    } else {
        let mut final_state: HashMap<String, String> = HashMap::new();
        let workdir = repo.workdir()?;
        for file_path in &pathspecs {
            let abs_path = workdir.join(file_path);
            let content = if abs_path.exists() {
                std::fs::read_to_string(&abs_path).unwrap_or_default()
            } else {
                String::new()
            };
            final_state.insert(file_path.clone(), content);
        }
        tracing::debug!("Read {} files from working directory", final_state.len());
        final_state
    };

    // Step 3: Build VirtualAttributions from old_head with working log applied.
    // When we have a captured snapshot, use it instead of the live worktree so line
    // coordinates stay stable under async replay.
    let repo_clone = repo.clone();
    let old_head_clone = old_head_sha.to_string();
    let pathspecs_clone = pathspecs.clone();

    let old_head_va = if has_captured_snapshot {
        smol::block_on(async {
            crate::authorship::virtual_attribution::VirtualAttributions::from_working_log_for_commit_snapshot(
                repo_clone,
                old_head_clone,
                &pathspecs_clone,
                None,
                Some(target_commit_sha.to_string()),
                &final_state,
            )
            .await
        })?
    } else {
        smol::block_on(async {
            crate::authorship::virtual_attribution::VirtualAttributions::from_working_log_for_commit(
                repo_clone,
                old_head_clone,
                &pathspecs_clone,
                None,
                Some(target_commit_sha.to_string()),
            )
            .await
        })?
    };

    tracing::debug!(
        "Built old_head VA with {} files, {} prompts",
        old_head_va.files().len(),
        old_head_va.prompts().len()
    );

    // Step 4+5: Merge old_head attributions with an empty VA against the final state.
    // target_va is always empty (the original blame range was broken: target..target is
    // always empty), but merge_attributions_favoring_first still performs the essential
    // transformation of primary attributions into the final_state coordinate space.
    let target_va = crate::authorship::virtual_attribution::VirtualAttributions::new(
        repo.clone(),
        target_commit_sha.to_string(),
        HashMap::new(),
        HashMap::new(),
        0,
    );
    let merged_va = crate::authorship::virtual_attribution::merge_attributions_favoring_first(
        old_head_va,
        target_va,
        final_state.clone(),
    )?;

    tracing::debug!("Merged VAs, result has {} files", merged_va.files().len());

    // Step 6: Convert to INITIAL (everything is uncommitted after reset) without consulting the
    // live worktree again.
    let initial_attributions = merged_va.to_initial_working_log_only();

    tracing::debug!(
        "Generated INITIAL attributions for {} files, {} prompts",
        initial_attributions.files.len(),
        initial_attributions.prompts.len()
    );

    // Step 7: Write INITIAL file
    let new_working_log = repo
        .storage
        .working_log_for_base_commit(target_commit_sha)?;
    new_working_log.reset_working_log()?;

    if !initial_attributions.files.is_empty() {
        new_working_log.write_initial_attributions_with_contents(
            initial_attributions.files,
            initial_attributions.prompts,
            initial_attributions.humans,
            final_state,
            initial_attributions.sessions,
        )?;
    }

    // Delete old working log
    repo.storage
        .delete_working_log_for_base_commit(old_head_sha)?;

    tracing::debug!(
        "✓ Wrote INITIAL attributions to working log for {}",
        target_commit_sha
    );

    Ok(())
}

/// Get all file paths modified across a list of commits
#[doc(hidden)]
pub fn get_pathspecs_from_commits(
    repo: &Repository,
    commits: &[String],
) -> Result<Vec<String>, GitAiError> {
    if commits.is_empty() {
        return Ok(Vec::new());
    }

    let mut args = repo.global_args_for_exec();
    args.push("diff-tree".to_string());
    args.push("--stdin".to_string());
    args.push("--name-only".to_string());
    args.push("-r".to_string());
    args.push("-z".to_string());

    let stdin_data = commits.join("\n") + "\n";
    let output = exec_git_stdin(&args, stdin_data.as_bytes())?;
    let commit_markers: HashSet<&str> = commits.iter().map(String::as_str).collect();

    let mut pathspecs = HashSet::new();
    for token in output
        .stdout
        .split(|&b| b == 0)
        .filter(|token| !token.is_empty())
    {
        let value = String::from_utf8(token.to_vec())?;
        // diff-tree --stdin prefixes each commit section with the commit SHA.
        // Filter only the exact commit markers we asked diff-tree to emit.
        if commit_markers.contains(value.as_str()) {
            continue;
        }
        pathspecs.insert(value);
    }

    Ok(pathspecs.into_iter().collect())
}

fn load_note_contents_for_commits(
    repo: &Repository,
    commit_shas: &[String],
) -> Result<HashMap<String, String>, GitAiError> {
    if commit_shas.is_empty() {
        return Ok(HashMap::new());
    }

    let note_blob_oids = note_blob_oids_for_commits(repo, commit_shas)?;
    if note_blob_oids.is_empty() {
        return Ok(HashMap::new());
    }

    let mut blob_oids: Vec<String> = note_blob_oids
        .values()
        .cloned()
        .collect::<HashSet<_>>()
        .into_iter()
        .collect();
    blob_oids.sort();
    let blob_contents = batch_read_blob_contents(repo, &blob_oids)?;

    let mut note_contents = HashMap::new();
    for (commit_sha, blob_oid) in note_blob_oids {
        if let Some(content) = blob_contents.get(&blob_oid) {
            note_contents.insert(commit_sha, content.clone());
        }
    }

    Ok(note_contents)
}

fn load_note_contents_for_commit_pairs(
    repo: &Repository,
    commit_pairs: &[(String, String)],
) -> Result<HashMap<String, String>, GitAiError> {
    if commit_pairs.is_empty() {
        return Ok(HashMap::new());
    }

    let source_commits: Vec<String> = commit_pairs
        .iter()
        .map(|(source_commit, _target_commit)| source_commit.clone())
        .collect();
    let source_note_contents = load_note_contents_for_commits(repo, &source_commits)?;

    let mut source_note_content_by_target_commit = HashMap::new();
    for (source_commit, target_commit) in commit_pairs {
        if let Some(note_content) = source_note_contents.get(source_commit) {
            source_note_content_by_target_commit
                .insert(target_commit.clone(), note_content.clone());
        }
    }

    Ok(source_note_content_by_target_commit)
}

use rebase_ops::remap_note_content_for_target_commit;

fn remap_notes_for_commit_pairs(
    repo: &Repository,
    commit_pairs: &[(String, String)],
    original_note_contents: &HashMap<String, String>,
) -> Result<usize, GitAiError> {
    if commit_pairs.is_empty() || original_note_contents.is_empty() {
        return Ok(0);
    }

    let mut entries = Vec::new();
    for (original_commit, new_commit) in commit_pairs {
        if let Some(raw_note) = original_note_contents.get(original_commit) {
            entries.push((
                new_commit.clone(),
                remap_note_content_for_target_commit(raw_note, new_commit),
            ));
        }
    }

    if entries.is_empty() {
        return Ok(0);
    }

    let count = entries.len();
    crate::git::refs::notes_add_batch(repo, &entries)?;

    Ok(count)
}

fn build_metadata_only_authorship_log_from_source_notes(
    repo: &Repository,
    source_commits: &[String],
    target_commit_sha: &str,
) -> Result<Option<AuthorshipLog>, GitAiError> {
    use crate::authorship::authorship_log::{HumanRecord, SessionRecord};

    let mut merged_prompts = BTreeMap::new();
    let mut prompt_totals: HashMap<String, (u32, u32)> = HashMap::new();
    let mut merged_humans: BTreeMap<String, HumanRecord> = BTreeMap::new();
    let mut merged_sessions: BTreeMap<String, SessionRecord> = BTreeMap::new();
    let mut saw_any_note = false;

    for commit_sha in source_commits {
        let Ok(log) = get_reference_as_authorship_log_v3(repo, commit_sha) else {
            continue;
        };
        saw_any_note = true;

        for (prompt_id, prompt_record) in log.metadata.prompts {
            let entry = prompt_totals.entry(prompt_id.clone()).or_insert((0, 0));
            entry.0 = entry.0.saturating_add(prompt_record.total_additions);
            entry.1 = entry.1.saturating_add(prompt_record.total_deletions);
            merged_prompts.insert(prompt_id, prompt_record);
        }
        for (hash, record) in log.metadata.humans {
            merged_humans.entry(hash).or_insert(record);
        }
        for (id, record) in log.metadata.sessions {
            merged_sessions.entry(id).or_insert(record);
        }
    }

    if !saw_any_note {
        return Ok(None);
    }

    for (prompt_id, (total_additions, total_deletions)) in prompt_totals {
        if let Some(prompt) = merged_prompts.get_mut(&prompt_id) {
            prompt.total_additions = total_additions;
            prompt.total_deletions = total_deletions;
        }
    }

    let mut authorship_log = AuthorshipLog::new();
    authorship_log.metadata.base_commit_sha = target_commit_sha.to_string();
    authorship_log.metadata.prompts = merged_prompts;
    authorship_log.metadata.humans = merged_humans;
    authorship_log.metadata.sessions = merged_sessions;
    Ok(Some(authorship_log))
}

/// Cached version of try_fast_path_rebase_note_remap that uses pre-loaded note data.
#[doc(hidden)]
pub fn try_fast_path_rebase_note_remap_cached(
    repo: &Repository,
    original_commits: &[String],
    new_commits: &[String],
    commits_to_process_lookup: &HashSet<&str>,
    tracked_paths: &[String],
    note_cache: &RebaseNoteCache,
) -> Result<bool, GitAiError> {
    let fast_path_start = std::time::Instant::now();
    if original_commits.len() != new_commits.len()
        || tracked_paths.is_empty()
        || commits_to_process_lookup.is_empty()
    {
        return Ok(false);
    }

    let commits_to_remap: Vec<(String, String)> = original_commits
        .iter()
        .zip(new_commits.iter())
        .filter(|(_original_commit, new_commit)| {
            commits_to_process_lookup.contains(new_commit.as_str())
        })
        .map(|(original_commit, new_commit)| (original_commit.clone(), new_commit.clone()))
        .collect();

    if commits_to_remap.is_empty() {
        return Ok(false);
    }

    let compare_start = std::time::Instant::now();
    if !tracked_paths_match_for_commit_pairs(repo, &commits_to_remap, tracked_paths)? {
        return Ok(false);
    }
    tracing::debug!(
        "Fast-path rebase note remap: compared tracked blobs for {} commit pairs in {}ms",
        commits_to_remap.len(),
        compare_start.elapsed().as_millis()
    );

    // Use cached note blob OIDs and contents instead of additional git calls.
    for (original_commit, _) in &commits_to_remap {
        if !note_cache
            .original_note_blob_oids
            .contains_key(original_commit)
        {
            return Ok(false);
        }
    }

    let mut remapped_note_entries: Vec<(String, String)> =
        Vec::with_capacity(commits_to_remap.len());
    for (original_commit, new_commit) in &commits_to_remap {
        let Some(raw_note) = note_cache.original_note_contents.get(original_commit) else {
            return Ok(false);
        };
        remapped_note_entries.push((
            new_commit.clone(),
            remap_note_content_for_target_commit(raw_note, new_commit),
        ));
    }

    let remapped_count = remapped_note_entries.len();
    let write_start = std::time::Instant::now();
    crate::git::refs::notes_add_batch(repo, &remapped_note_entries)?;

    tracing::debug!(
        "Fast-path rebase note remap: wrote {} remapped notes in {}ms",
        remapped_count,
        write_start.elapsed().as_millis()
    );

    tracing::debug!(
        "Fast-path remapped authorship logs for {} commits (blob-equivalent tracked files)",
        remapped_count
    );
    tracing::debug!(
        "Fast-path rebase note remap complete in {}ms",
        fast_path_start.elapsed().as_millis()
    );
    Ok(true)
}

fn try_fast_path_cherry_pick_note_remap(
    repo: &Repository,
    commit_pairs: &[(String, String)],
    tracked_paths: &[String],
) -> Result<bool, GitAiError> {
    let fast_path_start = std::time::Instant::now();
    if commit_pairs.is_empty() || tracked_paths.is_empty() {
        return Ok(false);
    }

    let compare_start = std::time::Instant::now();
    if !tracked_paths_match_for_commit_pairs(repo, commit_pairs, tracked_paths)? {
        return Ok(false);
    }
    tracing::debug!(
        "Fast-path cherry-pick note remap: compared tracked blobs for {} commit pairs in {}ms",
        commit_pairs.len(),
        compare_start.elapsed().as_millis()
    );

    let source_commits: Vec<String> = commit_pairs
        .iter()
        .map(|(source_commit, _new_commit)| source_commit.clone())
        .collect();
    let note_oid_lookup_start = std::time::Instant::now();
    let source_note_blob_oids = note_blob_oids_for_commits(repo, &source_commits)?;
    tracing::debug!(
        "Fast-path cherry-pick note remap: resolved {} note blob oids in {}ms",
        source_note_blob_oids.len(),
        note_oid_lookup_start.elapsed().as_millis()
    );
    if source_note_blob_oids.len() != source_commits.len() {
        return Ok(false);
    }

    let mut remapped_blob_entries: Vec<(String, String)> = Vec::with_capacity(commit_pairs.len());
    for (source_commit, new_commit) in commit_pairs {
        let blob_oid = match source_note_blob_oids.get(source_commit) {
            Some(oid) => oid.clone(),
            None => return Ok(false),
        };
        remapped_blob_entries.push((new_commit.clone(), blob_oid));
    }

    if remapped_blob_entries.is_empty() {
        return Ok(false);
    }

    let mut blob_oids: Vec<String> = remapped_blob_entries
        .iter()
        .map(|(_new_commit, blob_oid)| blob_oid.clone())
        .collect::<HashSet<_>>()
        .into_iter()
        .collect();
    blob_oids.sort();
    let blob_contents = batch_read_blob_contents(repo, &blob_oids)?;

    let mut remapped_note_entries: Vec<(String, String)> =
        Vec::with_capacity(remapped_blob_entries.len());
    for (new_commit, blob_oid) in remapped_blob_entries {
        let Some(raw_note) = blob_contents.get(&blob_oid) else {
            return Ok(false);
        };
        remapped_note_entries.push((
            new_commit.clone(),
            remap_note_content_for_target_commit(raw_note, &new_commit),
        ));
    }

    let remapped_count = remapped_note_entries.len();
    let write_start = std::time::Instant::now();
    crate::git::refs::notes_add_batch(repo, &remapped_note_entries)?;

    tracing::debug!(
        "Fast-path cherry-pick note remap: wrote {} remapped notes in {}ms",
        remapped_count,
        write_start.elapsed().as_millis()
    );

    tracing::debug!(
        "Fast-path remapped authorship logs for {} cherry-picked commits (blob-equivalent tracked files)",
        remapped_count
    );
    tracing::debug!(
        "Fast-path cherry-pick note remap complete in {}ms",
        fast_path_start.elapsed().as_millis()
    );
    Ok(true)
}

fn tracked_paths_match_for_commit_pairs(
    repo: &Repository,
    commit_pairs: &[(String, String)],
    tracked_paths: &[String],
) -> Result<bool, GitAiError> {
    if commit_pairs.is_empty() {
        return Ok(true);
    }

    let mut commits_to_load = Vec::with_capacity(commit_pairs.len() * 2);
    for (left_commit, right_commit) in commit_pairs {
        commits_to_load.push(left_commit.clone());
        commits_to_load.push(right_commit.clone());
    }
    let commit_metadata = load_commit_metadata_batch(repo, &commits_to_load)?;

    let mut args = repo.global_args_for_exec();
    args.push("diff-tree".to_string());
    args.push("--stdin".to_string());
    args.push("--raw".to_string());
    args.push("-z".to_string());
    args.push("--no-abbrev".to_string());
    args.push("-r".to_string());
    if !tracked_paths.is_empty() {
        args.push("--".to_string());
        args.extend(tracked_paths.iter().cloned());
    }

    let mut stdin_lines = String::new();
    for (left_commit, right_commit) in commit_pairs {
        let left_tree = match commit_metadata.get(left_commit) {
            Some(meta) if !meta.tree_oid.is_empty() => meta.tree_oid.as_str(),
            _ => return Ok(false),
        };
        let right_tree = match commit_metadata.get(right_commit) {
            Some(meta) if !meta.tree_oid.is_empty() => meta.tree_oid.as_str(),
            _ => return Ok(false),
        };
        stdin_lines.push_str(left_tree);
        stdin_lines.push(' ');
        stdin_lines.push_str(right_tree);
        stdin_lines.push('\n');
    }

    let output = exec_git_stdin(&args, stdin_lines.as_bytes())?;
    let data = output.stdout;

    let mut pos = 0usize;
    for _ in commit_pairs {
        let header_end = match data[pos..].iter().position(|&b| b == b'\n') {
            Some(idx) => pos + idx,
            None => return Ok(false),
        };
        pos = header_end + 1;

        // Any delta line means tracked path blobs differ for this pair.
        if pos < data.len() && data[pos] == b':' {
            return Ok(false);
        }

        // Skip any blank separators between sections.
        while pos < data.len() && data[pos] == b'\n' {
            pos += 1;
        }
    }

    // If the output still contains deltas, consider it non-matching to keep correctness.
    while pos < data.len() {
        if data[pos] == b':' {
            return Ok(false);
        }
        if data[pos] == b'\n' {
            pos += 1;
            continue;
        }
        if let Some(next_nl) = data[pos..].iter().position(|&b| b == b'\n') {
            pos += next_nl + 1;
        } else {
            break;
        }
    }

    Ok(true)
}

pub fn filter_pathspecs_to_ai_touched_files(
    repo: &Repository,
    commit_shas: &[String],
    pathspecs: &[String],
) -> Result<Vec<String>, GitAiError> {
    let touched_files = smol::block_on(load_ai_touched_files_for_commits(
        repo,
        commit_shas.to_vec(),
    ))?;
    Ok(pathspecs
        .iter()
        .filter(|p| touched_files.contains(p.as_str()))
        .cloned()
        .collect())
}

#[doc(hidden)]
pub fn build_file_attestation_from_line_attributions(
    file_path: &str,
    line_attrs: &[crate::authorship::attribution_tracker::LineAttribution],
) -> Option<crate::authorship::authorship_log_serialization::FileAttestation> {
    let mut by_author: HashMap<String, Vec<(u32, u32)>> = HashMap::new();
    for line_attr in line_attrs {
        if line_attr.author_id == crate::authorship::working_log::CheckpointKind::Human.to_str() {
            continue;
        }
        by_author
            .entry(line_attr.author_id.clone())
            .or_default()
            .push((line_attr.start_line, line_attr.end_line));
    }

    if by_author.is_empty() {
        return None;
    }

    let mut file_attestation =
        crate::authorship::authorship_log_serialization::FileAttestation::new(
            file_path.to_string(),
        );

    for (author_id, mut ranges) in by_author {
        if ranges.is_empty() {
            continue;
        }
        ranges.sort_by_key(|(start, end)| (*start, *end));

        let mut merged: Vec<(u32, u32)> = Vec::new();
        for (start, end) in ranges {
            match merged.last_mut() {
                Some((_, last_end)) => {
                    if start <= last_end.saturating_add(1) {
                        *last_end = (*last_end).max(end);
                    } else {
                        merged.push((start, end));
                    }
                }
                None => merged.push((start, end)),
            }
        }

        let line_ranges = merged
            .into_iter()
            .map(|(start, end)| {
                if start == end {
                    crate::authorship::authorship_log::LineRange::Single(start)
                } else {
                    crate::authorship::authorship_log::LineRange::Range(start, end)
                }
            })
            .collect::<Vec<_>>();

        if !line_ranges.is_empty() {
            file_attestation.add_entry(
                crate::authorship::authorship_log_serialization::AttestationEntry::new(
                    author_id,
                    line_ranges,
                ),
            );
        }
    }

    if file_attestation.entries.is_empty() {
        None
    } else {
        Some(file_attestation)
    }
}

/// Build an authorship note for `new_commit` from working-log checkpoint data stored
/// under `parent_sha`.  This is the fallback path for AI-resolved rebase conflicts:
/// when content-diff transfer produces no AI attribution (because the AI wrote *different*
/// content from the original commit), we fall back to the `line_attributions` that
/// `git-ai checkpoint` recorded in the working log during `rebase --continue`.
///
/// Returns `None` when no AI checkpoint data exists for any of `changed_files`
/// (human-only resolution or no checkpoint at all).
fn build_note_from_conflict_wl(
    repo: &crate::git::repository::Repository,
    new_commit: &str,
    parent_sha: &str,
    changed_files: &HashSet<String>,
) -> Option<String> {
    use crate::authorship::authorship_log_serialization::generate_short_hash;
    use crate::authorship::working_log::CheckpointKind;

    let working_log = repo.storage.working_log_for_base_commit(parent_sha).ok()?;
    let checkpoints = working_log.read_all_checkpoints().ok()?;

    let mut authorship_log = AuthorshipLog::new();
    authorship_log.metadata.base_commit_sha = new_commit.to_string();

    // Collect all line_attributions per file across all AI checkpoints, then build
    // a single FileAttestation per file. This avoids duplicate attestation entries
    // when multiple checkpoints contain entries for the same file.
    let mut file_line_attrs: HashMap<
        String,
        Vec<crate::authorship::attribution_tracker::LineAttribution>,
    > = HashMap::new();
    let mut has_ai_content = false;

    for checkpoint in &checkpoints {
        if checkpoint.kind == CheckpointKind::Human {
            continue;
        }

        // KnownHuman checkpoints: record the human identity in metadata.humans and skip
        // AI-prompt processing.  The AI checkpoint that follows a KnownHuman checkpoint
        // already carries the h_-attributed line_attributions in its own entries (because
        // the attribution state is accumulated across checkpoints), so there is no need to
        // process the KnownHuman checkpoint's entries separately.
        if checkpoint.kind == CheckpointKind::KnownHuman {
            let hash = crate::authorship::authorship_log_serialization::generate_human_short_hash(
                &checkpoint.author,
            );
            authorship_log
                .metadata
                .humans
                .entry(hash)
                .or_insert_with(|| crate::authorship::authorship_log::HumanRecord {
                    author: checkpoint.author.clone(),
                });
            continue;
        }

        // Skip checkpoints without an agent_id: their line_attributions would
        // reference an author_id not present in metadata.prompts/sessions, causing
        // blame to fall back to human attribution.
        let agent_id = match &checkpoint.agent_id {
            Some(id) => id,
            None => continue,
        };

        if checkpoint.trace_id.is_some() {
            // New session format: generate session_id and record in metadata.sessions.
            let session_id = crate::authorship::authorship_log_serialization::generate_session_id(
                &agent_id.id,
                &agent_id.tool,
            );
            authorship_log
                .metadata
                .sessions
                .entry(session_id)
                .or_insert_with(|| crate::authorship::authorship_log::SessionRecord {
                    agent_id: agent_id.clone(),
                    human_author: None,
                    custom_attributes: None,
                });
        } else {
            // Old prompt format: generate prompt hash and record in metadata.prompts.
            let author_id = generate_short_hash(&agent_id.id, &agent_id.tool);
            authorship_log
                .metadata
                .prompts
                .entry(author_id)
                .or_insert_with(|| crate::authorship::authorship_log::PromptRecord {
                    agent_id: agent_id.clone(),
                    human_author: None,
                    total_additions: checkpoint.line_stats.additions,
                    total_deletions: checkpoint.line_stats.deletions,
                    accepted_lines: 0,
                    overriden_lines: 0,
                    custom_attributes: None,
                    messages_url: None,
                });
        }

        for entry in &checkpoint.entries {
            if !changed_files.contains(&entry.file) {
                continue;
            }
            if entry.line_attributions.is_empty() {
                continue;
            }
            file_line_attrs
                .entry(entry.file.clone())
                .or_default()
                .extend(entry.line_attributions.iter().cloned());
        }
    }

    // Build one FileAttestation per file from the merged line attributions.
    // Also tally accepted_lines per author_id so the metadata prompts section
    // reflects the actual AI line count (not the hard-coded zero set above).
    let mut accepted_per_author: HashMap<String, u32> = HashMap::new();
    for (file_path, line_attrs) in &file_line_attrs {
        // Tally accepted lines per author from the raw LineAttribution slice.
        for la in line_attrs {
            // end_line is inclusive (1-indexed); count = end_line - start_line + 1.
            *accepted_per_author.entry(la.author_id.clone()).or_insert(0) +=
                la.end_line - la.start_line + 1;
        }
        if let Some(file_att) = build_file_attestation_from_line_attributions(file_path, line_attrs)
        {
            authorship_log.attestations.push(file_att);
            has_ai_content = true;
        }
    }

    // Patch each prompt's accepted_lines with the actual tally.
    for (author_id, count) in accepted_per_author {
        if let Some(record) = authorship_log.metadata.prompts.get_mut(&author_id) {
            record.accepted_lines = count;
        }
    }

    if !has_ai_content {
        return None;
    }

    authorship_log.serialize_to_string().ok()
}

/// Transform VirtualAttributions to match a new final state (single-source variant)
#[doc(hidden)]
pub fn transform_attributions_to_final_state(
    source_va: &crate::authorship::virtual_attribution::VirtualAttributions,
    final_state: HashMap<String, String>,
    original_head_state: Option<&crate::authorship::virtual_attribution::VirtualAttributions>,
) -> Result<crate::authorship::virtual_attribution::VirtualAttributions, GitAiError> {
    use crate::authorship::attribution_tracker::AttributionTracker;
    use crate::authorship::virtual_attribution::VirtualAttributions;

    let tracker = AttributionTracker::new();
    let ts = source_va.timestamp();
    let repo = source_va.repo().clone();
    let base_commit = source_va.base_commit().to_string();

    // Start from the current state so unchanged files stay tracked across commits.
    // This is required for cases where a file changes in commit N, is untouched in N+1,
    // and changes again later in the rewritten sequence.
    let mut attributions = HashMap::new();
    let mut file_contents = HashMap::new();
    for file in source_va.files() {
        if let Some(content) = source_va.get_file_content(&file) {
            file_contents.insert(file.clone(), content.clone());
        }
        if let Some(char_attrs) = source_va.get_char_attributions(&file)
            && let Some(line_attrs) = source_va.get_line_attributions(&file)
        {
            attributions.insert(file, (char_attrs.clone(), line_attrs.clone()));
        }
    }

    // Process each file in the final state
    for (file_path, final_content) in final_state {
        // Skip empty files (they don't exist in this commit yet)
        // Keep the source attributions for when the file appears later
        if final_content.is_empty() {
            continue;
        }

        // Get source attributions and content
        let source_attrs = source_va.get_char_attributions(&file_path);
        let source_content = source_va.get_file_content(&file_path);

        // Transform to final state
        let mut transformed_attrs =
            if let (Some(attrs), Some(content)) = (source_attrs, source_content) {
                // Use a dummy author for new insertions
                let dummy_author = "__DUMMY__";

                // Keep all attributions initially (including dummy ones)
                tracker.update_attributions(content, &final_content, attrs, dummy_author, ts)?
            } else {
                Vec::new()
            };

        // Try to restore attributions from original_head_state using line-content matching
        // This handles commit splitting where content from original_head gets re-applied
        if let Some(original_state) = original_head_state
            && let Some(original_content) = original_state.get_file_content(&file_path)
        {
            if original_content == &final_content {
                // The final content matches the original content exactly!
                // Use the original attributions
                if let Some(original_attrs) = original_state.get_char_attributions(&file_path) {
                    transformed_attrs = original_attrs.clone();
                }
            } else {
                // Use line-content matching to restore attributions for lines that existed before
                // Build a map of line content -> author from original state
                let mut original_line_to_author: HashMap<String, String> = HashMap::new();

                if let Some(original_line_attrs) = original_state.get_line_attributions(&file_path)
                {
                    let original_lines: Vec<&str> = original_content.lines().collect();

                    for line_attr in original_line_attrs {
                        // LineAttribution is 1-indexed
                        for line_num in line_attr.start_line..=line_attr.end_line {
                            let line_idx = (line_num as usize).saturating_sub(1);
                            if line_idx < original_lines.len() {
                                let line_content = original_lines[line_idx].to_string();
                                // Store all non-human attributions (AI attributions)
                                // VirtualAttributions normalizes humans to "human" via return_human_authors_as_human flag
                                // AI authors keep their tool names (mock_ai, Claude, GPT, etc.) or prompt hashes
                                if line_attr.author_id != "human" {
                                    original_line_to_author
                                        .insert(line_content, line_attr.author_id.clone());
                                }
                            }
                        }
                    }
                }

                // Now update char attributions based on line content matching
                let dummy_author = "__DUMMY__";
                let final_lines: Vec<&str> = final_content.lines().collect();
                let line_count = final_lines.len();

                // Convert char attributions to line attributions to process line by line
                let temp_line_attrs =
                    crate::authorship::attribution_tracker::attributions_to_line_attributions(
                        &transformed_attrs,
                        &final_content,
                    );

                // Build a line-level bitmap for dummy-attributed lines in O(attrs + lines).
                let mut dummy_diff = vec![0i32; line_count + 2];
                for la in &temp_line_attrs {
                    if la.author_id != dummy_author {
                        continue;
                    }
                    let start = (la.start_line as usize).max(1).min(line_count);
                    let end = (la.end_line as usize).max(1).min(line_count);
                    if start > end {
                        continue;
                    }
                    dummy_diff[start] += 1;
                    dummy_diff[end + 1] -= 1;
                }
                let mut has_dummy_line = vec![false; line_count + 1]; // 1-indexed
                let mut running = 0i32;
                for line in 1..=line_count {
                    running += dummy_diff[line];
                    has_dummy_line[line] = running > 0;
                }

                // Precompute per-line char starts once to avoid O(n^2) prefix sums.
                let mut line_start_chars = Vec::with_capacity(line_count);
                let mut char_pos = 0usize;
                for line in &final_lines {
                    line_start_chars.push(char_pos);
                    char_pos += line.len() + 1; // +1 for newline
                }

                // For each line with dummy attribution, try to restore from original
                for (line_idx, line_content) in final_lines.iter().enumerate() {
                    // Check if this line has a dummy attribution
                    let line_num = (line_idx + 1) as u32; // LineAttribution is 1-indexed
                    let has_dummy = has_dummy_line[line_num as usize];

                    if has_dummy {
                        // Try to find this line content in original state
                        if let Some(original_author) = original_line_to_author.get(*line_content) {
                            // Update all char attributions on this line
                            // Find the char range for this line
                            let line_start_char = line_start_chars[line_idx];
                            let line_end_char = line_start_char + line_content.len();

                            // Update attributions that overlap with this line
                            for attr in &mut transformed_attrs {
                                if attr.author_id == dummy_author
                                    && attr.start < line_end_char
                                    && attr.end > line_start_char
                                {
                                    attr.author_id = original_author.clone();
                                }
                            }
                        }
                    }
                }
            }
        }

        // Now filter out any remaining dummy attributions
        let dummy_author = "__DUMMY__";
        transformed_attrs.retain(|attr| attr.author_id != dummy_author);

        // Convert to line attributions
        let line_attrs = crate::authorship::attribution_tracker::attributions_to_line_attributions(
            &transformed_attrs,
            &final_content,
        );

        attributions.insert(file_path.clone(), (transformed_attrs, line_attrs));
        file_contents.insert(file_path, final_content);
    }

    // Merge prompts from source VA and original_head_state (source wins on conflict)
    let mut prompts = if let Some(original_state) = original_head_state {
        let mut merged = original_state.prompts().clone();
        for (id, commits) in source_va.prompts() {
            merged.insert(id.clone(), commits.clone());
        }
        merged
    } else {
        source_va.prompts().clone()
    };

    // Save total_additions and total_deletions from the merged prompts
    let mut saved_totals: HashMap<String, (u32, u32)> = HashMap::new();
    for (prompt_id, commits) in &prompts {
        for prompt_record in commits.values() {
            saved_totals.insert(
                prompt_id.clone(),
                (prompt_record.total_additions, prompt_record.total_deletions),
            );
        }
    }

    // Calculate and update prompt metrics based on transformed attributions
    crate::authorship::virtual_attribution::VirtualAttributions::calculate_and_update_prompt_metrics(
        &mut prompts,
        &attributions,
        &HashMap::new(), // Empty - will result in total_additions = 0
        &HashMap::new(), // Empty - will result in total_deletions = 0
    );

    // Restore the saved total_additions and total_deletions
    for (prompt_id, commits) in prompts.iter_mut() {
        if let Some(&(additions, deletions)) = saved_totals.get(prompt_id) {
            for prompt_record in commits.values_mut() {
                prompt_record.total_additions = additions;
                prompt_record.total_deletions = deletions;
            }
        }
    }

    Ok(VirtualAttributions::new_with_prompts(
        repo,
        base_commit,
        attributions,
        file_contents,
        prompts,
        ts,
    ))
}
