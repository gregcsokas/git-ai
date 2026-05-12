use crate::authorship::authorship_log_serialization::AuthorshipLog;
use crate::authorship::post_commit;
use crate::error::GitAiError;
use crate::git::authorship_traversal::{
    commits_have_authorship_notes, load_ai_touched_files_for_commits,
};
use crate::git::notes_api::{
    read_authorship_v3 as get_reference_as_authorship_log_v3, write_note as notes_add,
};
use crate::git::repository::{CommitRange, Repository, exec_git, exec_git_stdin};
use crate::git::rewrite_log::RewriteLogEvent;
use std::collections::{BTreeMap, HashMap, HashSet};

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
                notes_add(repo, merge_commit_sha, &authorship_json)?;
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

    notes_add(repo, merge_commit_sha, &authorship_json)?;

    tracing::debug!(
        "✓ Saved authorship log for merge commit {}",
        merge_commit_sha
    );

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

fn batch_read_blob_contents(
    repo: &Repository,
    blob_oids: &[String],
) -> Result<HashMap<String, String>, GitAiError> {
    if blob_oids.is_empty() {
        return Ok(HashMap::new());
    }

    let mut args = repo.global_args_for_exec();
    args.push("cat-file".to_string());
    args.push("--batch".to_string());

    let stdin_data = blob_oids.join("\n") + "\n";
    let output = exec_git_stdin(&args, stdin_data.as_bytes())?;

    parse_cat_file_batch_output_with_oids(&output.stdout)
}

#[doc(hidden)]
pub fn parse_cat_file_batch_output_with_oids(
    data: &[u8],
) -> Result<HashMap<String, String>, GitAiError> {
    let mut results = HashMap::new();
    let mut pos = 0usize;

    while pos < data.len() {
        let header_end = match data[pos..].iter().position(|&b| b == b'\n') {
            Some(idx) => pos + idx,
            None => break,
        };

        let header = std::str::from_utf8(&data[pos..header_end])?;
        let parts: Vec<&str> = header.split_whitespace().collect();
        if parts.len() < 2 {
            pos = header_end + 1;
            continue;
        }

        let oid = parts[0].to_string();
        if parts[1] == "missing" {
            pos = header_end + 1;
            continue;
        }

        if parts.len() < 3 {
            pos = header_end + 1;
            continue;
        }

        let size: usize = parts[2]
            .parse()
            .map_err(|e| GitAiError::Generic(format!("Invalid size in cat-file output: {}", e)))?;

        let content_start = header_end + 1;
        let content_end = content_start + size;
        if content_end > data.len() {
            return Err(GitAiError::Generic(
                "Malformed cat-file --batch output: truncated content".to_string(),
            ));
        }

        let content = String::from_utf8_lossy(&data[content_start..content_end]).to_string();
        results.insert(oid, content);

        pos = content_end;
        if pos < data.len() && data[pos] == b'\n' {
            pos += 1;
        }
    }

    Ok(results)
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
    notes_add(repo, amended_commit, &authorship_json)?;

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
fn get_files_changed_between_commits(
    repo: &Repository,
    from_commit: &str,
    to_commit: &str,
) -> Result<Vec<String>, GitAiError> {
    repo.diff_changed_files(from_commit, to_commit)
}

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
    let all_changed_files =
        get_files_changed_between_commits(repo, target_commit_sha, old_head_sha)?;

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

    // Step 4: Build VirtualAttributions from target_commit.
    //
    // The original intent was to capture AI lines that predate the reset range — lines that were
    // AI-authored before `target_commit` and are still present in the working directory — so that
    // `merge_attributions_favoring_first` (Step 5) could fill gaps in `old_head_va` with them.
    //
    // The implementation was broken from the start: it called `new_for_base_commit` with both
    // `base_commit` and `blame_start_commit` set to `target_commit_sha`, producing a blame range
    // of `target..target` (oldest == newest). That range is always empty — every line is
    // attributed to a boundary commit and mapped to human — so `target_va` always had zero AI
    // attributions and never filled any gaps.
    //
    // Additionally, `old_head_va` is built via `from_working_log_for_commit`, which replays the
    // existing working log entries at `old_head` on top of blame. Any AI lines that predate the
    // reset range and are tracked by git-ai are already carried into `old_head_va` through the
    // working log replay, so a correct `target_va` would have been redundant anyway.
    //
    // We create an empty VA directly (no subprocess calls). The merge result is identical to
    // before the fix because `target_va` was always empty.
    let target_va = {
        use std::time::{SystemTime, UNIX_EPOCH};
        let ts = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis();
        crate::authorship::virtual_attribution::VirtualAttributions::new(
            repo.clone(),
            target_commit_sha.to_string(),
            HashMap::new(),
            HashMap::new(),
            ts,
        )
    };

    // Step 5: Merge VAs favoring old_head to preserve uncommitted AI changes
    // old_head (with working log) wins overlaps, target fills gaps
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

/// Serialize attestation text directly from line_attrs without building intermediate FileAttestation.
/// This avoids HashMap allocation, sorting, and range merging overhead.
#[doc(hidden)]
pub fn diff_based_line_attribution_transfer(
    old_content: &str,
    new_content: &str,
    old_line_attrs: &[crate::authorship::attribution_tracker::LineAttribution],
) -> Vec<crate::authorship::attribution_tracker::LineAttribution> {
    use crate::authorship::imara_diff_utils::{DiffOp, capture_diff_slices};

    let old_lines: Vec<&str> = old_content.lines().collect();
    let new_lines: Vec<&str> = new_content.lines().collect();

    // Build a sparse lookup from 0-indexed line position → author_id for old content.
    // Using a HashMap instead of a full-size Vec avoids allocating O(file_size) memory
    // when only a small fraction of lines carry AI attribution.
    let mut old_line_author: HashMap<usize, &str> = HashMap::new();
    for attr in old_line_attrs {
        for line_num in attr.start_line..=attr.end_line {
            let idx = (line_num as usize).saturating_sub(1);
            if idx < old_lines.len() {
                old_line_author.insert(idx, &attr.author_id);
            }
        }
    }

    let diff_ops = capture_diff_slices(&old_lines, &new_lines);

    let mut new_line_attrs: Vec<crate::authorship::attribution_tracker::LineAttribution> =
        Vec::with_capacity(old_line_author.len());

    for op in &diff_ops {
        match op {
            DiffOp::Equal {
                old_index,
                new_index,
                len,
            } => {
                // Carry attributions forward for equal lines
                for i in 0..*len {
                    let old_idx = old_index + i;
                    let new_line_num = (new_index + i + 1) as u32;
                    if let Some(author_id) = old_line_author.get(&old_idx) {
                        new_line_attrs.push(
                            crate::authorship::attribution_tracker::LineAttribution {
                                start_line: new_line_num,
                                end_line: new_line_num,
                                author_id: author_id.to_string(),
                                overrode: None,
                            },
                        );
                    }
                }
            }
            DiffOp::Insert { .. } | DiffOp::Delete { .. } | DiffOp::Replace { .. } => {
                // Insert: new lines, no attribution
                // Delete: old lines removed, nothing to output
                // Replace: content changed, no attribution carried
            }
        }
    }

    new_line_attrs
}
