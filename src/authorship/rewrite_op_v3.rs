use std::collections::{HashMap, HashSet};

use crate::authorship::attribution_tracker::LineAttribution;
use crate::authorship::authorship_log::LineRange;
use crate::authorship::authorship_log_serialization::AuthorshipLog;
use crate::authorship::rebase_authorship::{
    build_file_attestation_from_line_attributions, diff_based_line_attribution_transfer,
};
use crate::authorship::working_log::CheckpointKind;
use crate::error::GitAiError;
use crate::git::range_diff::{CommitMapping, MappingKind, run_range_diff};
use crate::git::refs::{notes_add, show_authorship_note};
use crate::git::repository::{Repository, exec_git, exec_git_stdin};
use crate::git::rewrite_log::RewriteLogEvent;

/// The only input a rewrite operation needs.
#[derive(Debug, Clone)]
pub struct RewriteTriple {
    pub onto: String,
    pub original_head: String,
    pub new_head: String,
}

/// Unified handler for ALL rewrite operations.
///
/// Replaces `rewrite_authorship_after_rebase_v2`, `rewrite_authorship_after_cherry_pick`,
/// `rewrite_authorship_after_commit_amend`, and the squash working-log preparation.
///
/// Algorithm:
/// 1. `git range-diff onto..original onto..new` -> commit mappings
/// 2. For each mapping:
///    - Identical: copy note (with base_commit_sha remapped)
///    - Modified: load old note -> diff file contents -> transfer attributions -> write new note
///    - Deleted: no-op
///    - Added: no-op (new commits get attribution via normal post-commit flow)
/// 3. Migrate working log from original_head to new_head
pub fn handle_rewrite_op_v3(repo: &Repository, triple: &RewriteTriple) -> Result<(), GitAiError> {
    if triple.original_head == triple.new_head {
        return Ok(());
    }

    let mappings = run_range_diff(repo, &triple.onto, &triple.original_head, &triple.new_head)?;

    if mappings.is_empty() {
        return Ok(());
    }

    // Fetch any missing remote notes for original commits
    let original_commits: Vec<String> =
        mappings.iter().filter_map(|m| m.original.clone()).collect();
    crate::git::sync_authorship::fetch_missing_notes_for_commits(repo, &original_commits);

    // Detect squash groups (N Deleted + 1 Modified/Added)
    let squash_groups = detect_squash_groups(&mappings);

    for mapping in &mappings {
        if mapping.kind == MappingKind::Deleted {
            continue;
        }

        match mapping.kind {
            MappingKind::Identical => {
                if let (Some(original), Some(new)) = (&mapping.original, &mapping.new) {
                    copy_note(repo, original, new)?;
                }
            }
            MappingKind::Modified => {
                if let (Some(original), Some(new)) = (&mapping.original, &mapping.new) {
                    if let Some(group) = squash_groups.get(new.as_str()) {
                        transfer_attribution_squash(repo, &group.all_originals(), new)?;
                    } else {
                        let empty_map = HashMap::new();
                        transfer_attribution_via_diff(repo, original, new, &empty_map)?;
                    }
                }
            }
            MappingKind::Added => {
                if let Some(new) = &mapping.new
                    && let Some(group) = squash_groups.get(new.as_str())
                {
                    transfer_attribution_squash(repo, &group.all_originals(), new)?;
                }
            }
            MappingKind::Deleted => unreachable!(),
        }
    }

    migrate_working_log(repo, &triple.original_head, &triple.new_head)?;

    Ok(())
}

/// Handle a rewrite from the existing `RewriteLogEvent` data.
/// This is the adapter that plugs into the existing daemon dispatch.
pub fn handle_rewrite_from_event(
    repo: &Repository,
    event: &RewriteLogEvent,
) -> Result<(), GitAiError> {
    match event {
        RewriteLogEvent::RebaseComplete { rebase_complete } => {
            crate::git::sync_authorship::fetch_missing_notes_for_commits(
                repo,
                &rebase_complete.original_commits,
            );
            let pairs = pair_commits_for_rewrite(
                repo,
                &rebase_complete.original_commits,
                &rebase_complete.new_commits,
            );
            // Detect squash: multiple originals mapped to one new commit
            let is_squash =
                rebase_complete.original_commits.len() > rebase_complete.new_commits.len();
            if is_squash && rebase_complete.new_commits.len() == 1 {
                transfer_attribution_squash(
                    repo,
                    &rebase_complete.original_commits,
                    &rebase_complete.new_commits[0],
                )?;
            } else {
                // Build combined content→author map from ALL original commits.
                // This allows lines attributed in earlier commits (e.g. A) to be
                // recognized when transferring later commits (e.g. B) whose notes
                // only contain their own delta.
                let content_author_map = build_content_author_map(
                    repo,
                    &rebase_complete.original_commits,
                    &rebase_complete.original_head,
                );
                for (original, new) in &pairs {
                    transfer_attribution_via_diff(repo, original, new, &content_author_map)?;
                }
            }
            migrate_working_log(
                repo,
                &rebase_complete.original_head,
                &rebase_complete.new_head,
            )?;
            Ok(())
        }
        RewriteLogEvent::CherryPickComplete {
            cherry_pick_complete,
        } => {
            crate::git::sync_authorship::fetch_missing_notes_for_commits(
                repo,
                &cherry_pick_complete.source_commits,
            );
            let pairs = pair_commits_for_rewrite(
                repo,
                &cherry_pick_complete.source_commits,
                &cherry_pick_complete.new_commits,
            );
            let empty_map = HashMap::new();
            for (source, new) in &pairs {
                transfer_attribution_via_diff(repo, source, new, &empty_map)?;
            }
            migrate_working_log(
                repo,
                &cherry_pick_complete.original_head,
                &cherry_pick_complete.new_head,
            )?;
            Ok(())
        }
        RewriteLogEvent::CommitAmend { commit_amend } => {
            crate::git::sync_authorship::fetch_missing_notes_for_commits(
                repo,
                std::slice::from_ref(&commit_amend.original_commit),
            );
            let empty_map = HashMap::new();
            transfer_attribution_via_diff(
                repo,
                &commit_amend.original_commit,
                &commit_amend.amended_commit_sha,
                &empty_map,
            )?;
            migrate_working_log(
                repo,
                &commit_amend.original_commit,
                &commit_amend.amended_commit_sha,
            )?;
            Ok(())
        }
        _ => Ok(()),
    }
}

/// Pair original commits to new commits using subject-line matching for unequal lengths.
fn pair_commits_for_rewrite(
    repo: &Repository,
    original_commits: &[String],
    new_commits: &[String],
) -> Vec<(String, String)> {
    if original_commits.len() == new_commits.len() {
        return original_commits
            .iter()
            .zip(new_commits.iter())
            .map(|(a, b)| (a.clone(), b.clone()))
            .collect();
    }

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
    let mut pairs = Vec::with_capacity(new_commits.len());

    for new_sha in new_commits {
        let new_subject = repo
            .find_commit(new_sha.clone())
            .and_then(|c| c.summary())
            .unwrap_or_default();

        let matched = original_subjects.iter().find(|(orig_sha, orig_subject)| {
            !used.contains(orig_sha) && *orig_subject == new_subject
        });

        let orig_sha = if let Some((orig_sha, _)) = matched {
            orig_sha.clone()
        } else {
            match original_subjects
                .iter()
                .find(|(orig_sha, _)| !used.contains(orig_sha))
            {
                Some((orig_sha, _)) => orig_sha.clone(),
                None => continue,
            }
        };

        used.insert(orig_sha.clone());
        pairs.push((orig_sha, new_sha.clone()));
    }

    pairs
}

/// Copy an authorship note from one commit to another, remapping base_commit_sha.
fn copy_note(repo: &Repository, from_sha: &str, to_sha: &str) -> Result<(), GitAiError> {
    let note_content = match show_authorship_note(repo, from_sha) {
        Some(content) => content,
        None => return Ok(()),
    };
    let remapped = remap_base_commit_sha(&note_content, to_sha);
    notes_add(repo, to_sha, &remapped)?;
    Ok(())
}

/// Transfer attribution from an original commit to a modified commit using diff-based hunk transfer.
/// `content_author_map` maps file_path → (line_content → author_id) for content-matching
/// fallback when lines exist in the new commit that weren't in the original note.
fn transfer_attribution_via_diff(
    repo: &Repository,
    original_sha: &str,
    new_sha: &str,
    content_author_map: &HashMap<String, HashMap<String, String>>,
) -> Result<(), GitAiError> {
    // Skip if the new commit already has an authorship note (e.g. it was already
    // processed via post-commit). This prevents a mis-mapped rebase event from
    // overwriting a valid note with data from an unrelated original commit.
    if show_authorship_note(repo, new_sha).is_some() {
        return Ok(());
    }

    let note_content = match show_authorship_note(repo, original_sha) {
        Some(content) => content,
        None => return Ok(()),
    };

    let authorship_log = match AuthorshipLog::deserialize_from_string(&note_content) {
        Ok(log) => log,
        Err(_) => return Ok(()),
    };

    let attested_files: Vec<String> = authorship_log
        .attestations
        .iter()
        .map(|a| a.file_path.clone())
        .collect();

    if attested_files.is_empty() {
        // Metadata-only note — copy with remapped base_commit_sha
        let remapped = remap_base_commit_sha(&note_content, new_sha);
        notes_add(repo, new_sha, &remapped)?;
        return Ok(());
    }

    // Read file contents at both commits in a single batch call each
    let old_contents = batch_cat_file(repo, original_sha, &attested_files)?;
    let new_contents = batch_cat_file(repo, new_sha, &attested_files)?;

    let mut new_log = AuthorshipLog::new();
    new_log.metadata.base_commit_sha = new_sha.to_string();
    new_log.metadata.prompts = authorship_log.metadata.prompts.clone();
    new_log.metadata.sessions = authorship_log.metadata.sessions.clone();
    // humans are scoped per-commit: only include if attestation entries reference h_-prefixed IDs
    // (populated after we process attestations below)

    for file_attestation in &authorship_log.attestations {
        let file_path = &file_attestation.file_path;
        let old_content = match old_contents.get(file_path) {
            Some(c) if !c.is_empty() => c.as_str(),
            _ => continue,
        };
        let new_content = match new_contents.get(file_path) {
            Some(c) if !c.is_empty() => c.as_str(),
            _ => continue,
        };

        let old_attrs = attestation_entries_to_line_attributions(&file_attestation.entries);
        if old_attrs.is_empty() {
            continue;
        }

        let mut transferred =
            diff_based_line_attribution_transfer(old_content, new_content, &old_attrs);

        // Content-matching fallback: fill in lines from the combined content→author map
        // that weren't covered by the diff transfer (lines attributed by earlier commits
        // in the chain whose notes use per-commit-delta scoping).
        if let Some(file_map) = content_author_map.get(file_path) {
            let covered: HashSet<u32> = transferred
                .iter()
                .flat_map(|a| a.start_line..=a.end_line)
                .collect();
            for (line_idx, line_content) in new_content.lines().enumerate() {
                let line_num = (line_idx + 1) as u32;
                if covered.contains(&line_num) {
                    continue;
                }
                if let Some(author_id) = file_map.get(line_content) {
                    transferred.push(LineAttribution {
                        start_line: line_num,
                        end_line: line_num,
                        author_id: author_id.clone(),
                        overrode: None,
                    });
                }
            }
            transferred.sort_by_key(|a| a.start_line);
        }

        if let Some(new_attestation) =
            build_file_attestation_from_line_attributions(file_path, &transferred)
        {
            new_log.attestations.push(new_attestation);
        }
    }

    // Populate humans metadata from two sources:
    // 1. h_-prefixed author IDs in transferred attestation entries
    for attestation in &new_log.attestations {
        for entry in &attestation.entries {
            if entry.hash.starts_with("h_")
                && let Some(record) = authorship_log.metadata.humans.get(&entry.hash)
            {
                new_log
                    .metadata
                    .humans
                    .entry(entry.hash.clone())
                    .or_insert_with(|| record.clone());
            }
        }
    }
    // 2. KnownHuman checkpoints in the working log at the new commit's parent
    //    (written during conflict resolution via `rebase --continue`)
    populate_humans_from_working_log(repo, new_sha, &mut new_log);

    // Conflict resolution fallback: if diff-based transfer produced no attestations,
    // check the working log at the new commit's parent for AI checkpoint data written
    // during `rebase --continue` conflict resolution.
    if new_log.attestations.is_empty() {
        if let Some(conflict_note) = build_note_from_conflict_wl(repo, new_sha) {
            notes_add(repo, new_sha, &conflict_note)?;
            return Ok(());
        }
        // No conflict working log either — remap the original note as a last resort.
        let remapped = remap_base_commit_sha(&note_content, new_sha);
        notes_add(repo, new_sha, &remapped)?;
        return Ok(());
    }

    let serialized = new_log
        .serialize_to_string()
        .map_err(|e| GitAiError::Generic(format!("Failed to serialize authorship log: {}", e)))?;
    notes_add(repo, new_sha, &serialized)?;

    Ok(())
}

/// Transfer attribution from multiple original commits into a single squash result.
/// Uses sequential replay: processes original commits in order, transferring accumulated
/// attributions through each diff step, then overlaying each commit's per-delta note.
/// This matches the old code's hunk-based replay semantics.
fn transfer_attribution_squash(
    repo: &Repository,
    originals: &[String],
    new_sha: &str,
) -> Result<(), GitAiError> {
    if originals.is_empty() {
        return Ok(());
    }

    // Collect all files mentioned in any original note.
    let mut all_files: HashSet<String> = HashSet::new();
    let mut base_log: Option<AuthorshipLog> = None;
    let mut parsed_notes: Vec<(String, AuthorshipLog)> = Vec::new();

    for original_sha in originals {
        let note_content = match show_authorship_note(repo, original_sha) {
            Some(content) => content,
            None => continue,
        };
        let authorship_log = match AuthorshipLog::deserialize_from_string(&note_content) {
            Ok(log) => log,
            Err(_) => continue,
        };

        if base_log.is_none() {
            base_log = Some(authorship_log.clone());
        } else if let Some(ref mut base) = base_log {
            for (k, v) in &authorship_log.metadata.prompts {
                base.metadata.prompts.entry(k.clone()).or_insert(v.clone());
            }
            for (k, v) in &authorship_log.metadata.humans {
                base.metadata.humans.entry(k.clone()).or_insert(v.clone());
            }
            for (k, v) in &authorship_log.metadata.sessions {
                base.metadata.sessions.entry(k.clone()).or_insert(v.clone());
            }
        }

        for att in &authorship_log.attestations {
            all_files.insert(att.file_path.clone());
        }
        parsed_notes.push((original_sha.clone(), authorship_log));
    }

    let Some(mut base) = base_log else {
        return Ok(());
    };

    let all_files_vec: Vec<String> = all_files.iter().cloned().collect();

    // Sequential replay: accumulate attributions by diffing consecutive commits.
    // For each file, maintain current attributions and content, then:
    // 1. Diff previous content → current commit content → transfer attrs
    // 2. Overlay the current commit's note attrs on top
    let mut accumulated_attrs: HashMap<String, Vec<LineAttribution>> = HashMap::new();
    let mut prev_contents: HashMap<String, String> = HashMap::new();

    for (i, (original_sha, authorship_log)) in parsed_notes.iter().enumerate() {
        let current_contents = batch_cat_file(repo, original_sha, &all_files_vec)?;

        if i > 0 {
            // Diff previous content → current content and transfer accumulated attrs
            for file_path in &all_files_vec {
                let prev_content = prev_contents.get(file_path).map(String::as_str);
                let curr_content = current_contents.get(file_path).map(String::as_str);

                if let (Some(prev_c), Some(curr_c)) = (prev_content, curr_content)
                    && !prev_c.is_empty()
                    && !curr_c.is_empty()
                    && prev_c != curr_c
                    && let Some(attrs) = accumulated_attrs.get(file_path)
                    && !attrs.is_empty()
                {
                    let transferred = diff_based_line_attribution_transfer(prev_c, curr_c, attrs);
                    accumulated_attrs.insert(file_path.clone(), transferred);
                }
            }
        }

        // Overlay this commit's note attributions
        for file_attestation in &authorship_log.attestations {
            let file_path = &file_attestation.file_path;
            let note_attrs = attestation_entries_to_line_attributions(&file_attestation.entries);
            if note_attrs.is_empty() {
                continue;
            }
            let entry = accumulated_attrs.entry(file_path.clone()).or_default();
            for attr in note_attrs {
                overlay_attribution(entry, attr.start_line, attr.end_line, attr.author_id);
            }
        }

        prev_contents = current_contents;
    }

    // Finally, diff the last original's content → squash result content
    let final_contents = batch_cat_file(repo, new_sha, &all_files_vec)?;
    for file_path in &all_files_vec {
        let prev_content = prev_contents.get(file_path).map(String::as_str);
        let final_content = final_contents.get(file_path).map(String::as_str);

        if let (Some(prev_c), Some(final_c)) = (prev_content, final_content)
            && !prev_c.is_empty()
            && !final_c.is_empty()
            && prev_c != final_c
            && let Some(attrs) = accumulated_attrs.get(file_path)
            && !attrs.is_empty()
        {
            let transferred = diff_based_line_attribution_transfer(prev_c, final_c, attrs);
            accumulated_attrs.insert(file_path.clone(), transferred);
        }
    }

    base.metadata.base_commit_sha = new_sha.to_string();
    base.attestations.clear();

    for (file_path, mut attrs) in accumulated_attrs {
        attrs.sort_by_key(|a| a.start_line);
        if let Some(attestation) = build_file_attestation_from_line_attributions(&file_path, &attrs)
        {
            base.attestations.push(attestation);
        }
    }

    let serialized = base
        .serialize_to_string()
        .map_err(|e| GitAiError::Generic(format!("Failed to serialize authorship log: {}", e)))?;
    notes_add(repo, new_sha, &serialized)?;

    Ok(())
}

/// Convert attestation entries (hash + line ranges) into flat LineAttribution list.
fn attestation_entries_to_line_attributions(
    entries: &[crate::authorship::authorship_log_serialization::AttestationEntry],
) -> Vec<LineAttribution> {
    let mut attrs = Vec::new();
    for entry in entries {
        for range in &entry.line_ranges {
            let (start, end) = match range {
                LineRange::Single(l) => (*l, *l),
                LineRange::Range(s, e) => (*s, *e),
            };
            attrs.push(LineAttribution {
                start_line: start,
                end_line: end,
                author_id: entry.hash.clone(),
                overrode: None,
            });
        }
    }
    attrs
}

/// Overlay a new attribution range onto existing attributions, splitting partial overlaps.
fn overlay_attribution(attrs: &mut Vec<LineAttribution>, start: u32, end: u32, author_id: String) {
    let mut i = 0;
    let mut to_insert_after: Vec<LineAttribution> = Vec::new();
    while i < attrs.len() {
        let a = &attrs[i];
        if a.end_line < start || a.start_line > end {
            i += 1;
            continue;
        }
        let removed = attrs.remove(i);
        if removed.start_line < start {
            attrs.insert(
                i,
                LineAttribution {
                    start_line: removed.start_line,
                    end_line: start - 1,
                    author_id: removed.author_id.clone(),
                    overrode: removed.overrode.clone(),
                },
            );
            i += 1;
        }
        if removed.end_line > end {
            to_insert_after.push(LineAttribution {
                start_line: end + 1,
                end_line: removed.end_line,
                author_id: removed.author_id,
                overrode: removed.overrode,
            });
        }
    }
    for frag in to_insert_after {
        attrs.push(frag);
    }
    attrs.push(LineAttribution {
        start_line: start,
        end_line: end,
        author_id,
        overrode: None,
    });
}

/// Batch read file contents at a specific commit.
fn batch_cat_file(
    repo: &Repository,
    commit_sha: &str,
    file_paths: &[String],
) -> Result<HashMap<String, String>, GitAiError> {
    if file_paths.is_empty() {
        return Ok(HashMap::new());
    }

    let mut args = repo.global_args_for_exec();
    args.push("cat-file".to_string());
    args.push("--batch".to_string());

    let stdin_data: String = file_paths
        .iter()
        .map(|path| format!("{}:{}", commit_sha, path))
        .collect::<Vec<_>>()
        .join("\n")
        + "\n";

    let output = exec_git_stdin(&args, stdin_data.as_bytes())?;
    let data = &output.stdout;

    let mut results = HashMap::new();
    let mut pos = 0usize;
    let mut path_idx = 0usize;

    while pos < data.len() && path_idx < file_paths.len() {
        let header_end = match data[pos..].iter().position(|&b| b == b'\n') {
            Some(idx) => pos + idx,
            None => break,
        };

        let header = std::str::from_utf8(&data[pos..header_end]).unwrap_or("");
        let parts: Vec<&str> = header.split_whitespace().collect();

        if parts.len() >= 2 && parts[1] == "missing" {
            pos = header_end + 1;
            path_idx += 1;
            continue;
        }

        if parts.len() < 3 {
            pos = header_end + 1;
            path_idx += 1;
            continue;
        }

        let size: usize = parts[2].parse().unwrap_or(0);
        let content_start = header_end + 1;
        let content_end = content_start + size;

        if content_end <= data.len() {
            let content = String::from_utf8_lossy(&data[content_start..content_end]).to_string();
            results.insert(file_paths[path_idx].clone(), content);
        }

        pos = content_end;
        if pos < data.len() && data[pos] == b'\n' {
            pos += 1;
        }
        path_idx += 1;
    }

    Ok(results)
}

/// Remap the base_commit_sha field in a serialized note to a new target commit.
fn remap_base_commit_sha(note_content: &str, target_commit: &str) -> String {
    // Fast path: direct string replacement of the JSON field value
    let field = "\"base_commit_sha\"";
    let Some(field_pos) = note_content.find(field) else {
        return note_content.to_string();
    };
    let bytes = note_content.as_bytes();

    let mut pos = field_pos + field.len();
    while pos < bytes.len() && matches!(bytes[pos], b' ' | b'\n' | b'\t' | b'\r') {
        pos += 1;
    }
    if pos >= bytes.len() || bytes[pos] != b':' {
        return note_content.to_string();
    }
    pos += 1;

    while pos < bytes.len() && matches!(bytes[pos], b' ' | b'\n' | b'\t' | b'\r') {
        pos += 1;
    }
    if pos >= bytes.len() || bytes[pos] != b'"' {
        return note_content.to_string();
    }
    pos += 1;
    let value_start = pos;

    while pos < bytes.len() {
        match bytes[pos] {
            b'\\' => {
                pos += 2;
            }
            b'"' => {
                let mut remapped = String::with_capacity(
                    note_content.len() - (pos - value_start) + target_commit.len(),
                );
                remapped.push_str(&note_content[..value_start]);
                remapped.push_str(target_commit);
                remapped.push_str(&note_content[pos..]);
                return remapped;
            }
            _ => {
                pos += 1;
            }
        }
    }

    note_content.to_string()
}

struct SquashGroup {
    matched_original: Option<String>,
    deleted_originals: Vec<String>,
}

impl SquashGroup {
    fn all_originals(&self) -> Vec<String> {
        let mut all = self.deleted_originals.clone();
        if let Some(matched) = &self.matched_original {
            all.push(matched.clone());
        }
        all
    }
}

/// Detect squash patterns: consecutive Deleted commits followed by a Modified/Added.
fn detect_squash_groups(mappings: &[CommitMapping]) -> HashMap<&str, SquashGroup> {
    let mut groups: HashMap<&str, SquashGroup> = HashMap::new();
    let mut pending_deleted: Vec<String> = Vec::new();

    for mapping in mappings {
        match mapping.kind {
            MappingKind::Deleted => {
                if let Some(original) = &mapping.original {
                    pending_deleted.push(original.clone());
                }
            }
            MappingKind::Modified | MappingKind::Added => {
                if !pending_deleted.is_empty()
                    && let Some(new) = &mapping.new
                {
                    groups.insert(
                        new.as_str(),
                        SquashGroup {
                            matched_original: mapping.original.clone(),
                            deleted_originals: std::mem::take(&mut pending_deleted),
                        },
                    );
                }
                pending_deleted.clear();
            }
            MappingKind::Identical => {
                pending_deleted.clear();
            }
        }
    }
    groups
}

/// Build a combined content→author map from all original commits' notes.
/// Maps file_path → (line_content → author_id) based on the file content at `original_head`.
/// This enables content-matching for lines attributed by earlier commits in the chain.
fn build_content_author_map(
    repo: &Repository,
    original_commits: &[String],
    original_head: &str,
) -> HashMap<String, HashMap<String, String>> {
    let mut all_file_attrs: HashMap<String, Vec<LineAttribution>> = HashMap::new();

    for sha in original_commits {
        let note = match show_authorship_note(repo, sha) {
            Some(n) => n,
            None => continue,
        };
        let log = match AuthorshipLog::deserialize_from_string(&note) {
            Ok(l) => l,
            Err(_) => continue,
        };
        for attestation in &log.attestations {
            let attrs = attestation_entries_to_line_attributions(&attestation.entries);
            all_file_attrs
                .entry(attestation.file_path.clone())
                .or_default()
                .extend(attrs);
        }
    }

    if all_file_attrs.is_empty() {
        return HashMap::new();
    }

    let file_paths: Vec<String> = all_file_attrs.keys().cloned().collect();
    let head_contents = batch_cat_file(repo, original_head, &file_paths).unwrap_or_default();

    let mut result: HashMap<String, HashMap<String, String>> = HashMap::new();
    for (file_path, attrs) in &all_file_attrs {
        let content = match head_contents.get(file_path) {
            Some(c) if !c.is_empty() => c,
            _ => continue,
        };
        let lines: Vec<&str> = content.lines().collect();
        let mut line_map: HashMap<String, String> = HashMap::new();
        for attr in attrs {
            for line_num in attr.start_line..=attr.end_line {
                let idx = line_num.saturating_sub(1) as usize;
                if let Some(line_content) = lines.get(idx)
                    && !line_content.is_empty()
                {
                    line_map
                        .entry(line_content.to_string())
                        .or_insert_with(|| attr.author_id.clone());
                }
            }
        }
        if !line_map.is_empty() {
            result.insert(file_path.clone(), line_map);
        }
    }

    result
}

/// Populate humans metadata from the working log at the new commit's parent SHA.
/// This handles the case where KnownHuman checkpoints were written during conflict
/// resolution (rebase --continue) and need to appear in the note's metadata.
fn populate_humans_from_working_log(repo: &Repository, new_sha: &str, log: &mut AuthorshipLog) {
    let parent_sha = match repo
        .find_commit(new_sha.to_string())
        .ok()
        .and_then(|c| c.parent(0).ok())
    {
        Some(p) => p.id(),
        None => return,
    };

    let working_log = match repo.storage.working_log_for_base_commit(&parent_sha) {
        Ok(wl) => wl,
        Err(_) => return,
    };
    let checkpoints = match working_log.read_all_checkpoints() {
        Ok(cps) => cps,
        Err(_) => return,
    };

    let changed_files = get_changed_files_in_commit(repo, new_sha);

    for checkpoint in &checkpoints {
        if checkpoint.kind != CheckpointKind::KnownHuman {
            continue;
        }
        // Only include if checkpoint covers files that changed in this commit
        if !checkpoint
            .entries
            .iter()
            .any(|e| changed_files.contains(&e.file))
        {
            continue;
        }
        let hash = crate::authorship::authorship_log_serialization::generate_human_short_hash(
            &checkpoint.author,
        );
        log.metadata.humans.entry(hash).or_insert_with(|| {
            crate::authorship::authorship_log::HumanRecord {
                author: checkpoint.author.clone(),
            }
        });
    }
}

/// Get the list of files changed in a commit (vs its first parent).
fn get_changed_files_in_commit(repo: &Repository, commit_sha: &str) -> HashSet<String> {
    let mut args = repo.global_args_for_exec();
    args.push("diff-tree".to_string());
    args.push("--no-commit-id".to_string());
    args.push("-r".to_string());
    args.push("--name-only".to_string());
    args.push(commit_sha.to_string());

    match exec_git(&args) {
        Ok(output) => {
            let stdout = String::from_utf8_lossy(&output.stdout);
            stdout
                .lines()
                .filter(|l| !l.is_empty())
                .map(|l| l.to_string())
                .collect()
        }
        Err(_) => HashSet::new(),
    }
}

/// Build an authorship note from working-log checkpoint data written during conflict resolution.
///
/// When an AI resolves a rebase conflict (via `git-ai checkpoint` during `rebase --continue`),
/// the diff-based transfer can't carry attribution because the content differs from the original.
/// This fallback reads the working log at the new commit's parent SHA and builds a note from
/// the AI checkpoint entries for the changed files.
fn build_note_from_conflict_wl(repo: &Repository, new_commit: &str) -> Option<String> {
    use crate::authorship::authorship_log_serialization::generate_short_hash;

    let parent_sha = repo
        .find_commit(new_commit.to_string())
        .ok()?
        .parent(0)
        .ok()?
        .id();

    let working_log = repo.storage.working_log_for_base_commit(&parent_sha).ok()?;
    let checkpoints = working_log.read_all_checkpoints().ok()?;

    let changed_files = get_changed_files_in_commit(repo, new_commit);
    if changed_files.is_empty() {
        return None;
    }

    let mut authorship_log = AuthorshipLog::new();
    authorship_log.metadata.base_commit_sha = new_commit.to_string();

    let mut file_line_attrs: HashMap<String, Vec<LineAttribution>> = HashMap::new();
    let mut has_ai_content = false;

    for checkpoint in &checkpoints {
        if checkpoint.kind == CheckpointKind::Human {
            continue;
        }

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

        let agent_id = match &checkpoint.agent_id {
            Some(id) => id,
            None => continue,
        };

        if checkpoint.trace_id.is_some() {
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

    let mut accepted_per_author: HashMap<String, u32> = HashMap::new();
    for (file_path, line_attrs) in &file_line_attrs {
        for la in line_attrs {
            *accepted_per_author.entry(la.author_id.clone()).or_insert(0) +=
                la.end_line - la.start_line + 1;
        }
        if let Some(file_att) = build_file_attestation_from_line_attributions(file_path, line_attrs)
        {
            authorship_log.attestations.push(file_att);
            has_ai_content = true;
        }
    }

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

/// Migrate working log from original_head SHA to new_head SHA.
fn migrate_working_log(
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
        repo.storage
            .delete_working_log_for_base_commit(original_head)?;
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_diff_transfer_squash_closing_brace() {
        let old_content = "func serve() {\n    listen()\n    handle()\n}\n";
        let new_content =
            "func serve() {\n    listen()\n    handle()\n    logMetrics()\n    shutdown()\n}\n";

        // All 4 lines in old are AI-attributed (1-indexed)
        let old_attrs = vec![
            LineAttribution {
                start_line: 1,
                end_line: 1,
                author_id: "ai1".to_string(),
                overrode: None,
            },
            LineAttribution {
                start_line: 2,
                end_line: 2,
                author_id: "ai1".to_string(),
                overrode: None,
            },
            LineAttribution {
                start_line: 3,
                end_line: 3,
                author_id: "ai1".to_string(),
                overrode: None,
            },
            LineAttribution {
                start_line: 4,
                end_line: 4,
                author_id: "ai1".to_string(),
                overrode: None,
            },
        ];

        let result = diff_based_line_attribution_transfer(old_content, new_content, &old_attrs);

        // Print what we get
        for attr in &result {
            eprintln!(
                "Line {}-{}: {}",
                attr.start_line, attr.end_line, attr.author_id
            );
        }

        // The test expects:
        // Lines 1-3: AI (equal, transferred)
        // Lines 4-5: nothing (inserted)
        // Line 6 (}): should be AI (equal, transferred from old line 4)
        assert_eq!(result.len(), 4, "Should have 4 transferred attributions");
    }

    #[test]
    fn test_diff_transfer_prepend_header() {
        // Simulates rebase where upstream prepends a header line
        let old_content =
            "fn base() {}\nfn a1() {}\nfn a2() {}\nfn a3() {}\nfn b1() {}\nfn b2() {}\nfn b3() {}";
        let new_content = "// header\nfn base() {}\nfn a1() {}\nfn a2() {}\nfn a3() {}\nfn b1() {}\nfn b2() {}\nfn b3() {}";

        // Lines 2-7 in old are AI (1-indexed)
        let old_attrs = vec![
            LineAttribution {
                start_line: 2,
                end_line: 2,
                author_id: "ai1".to_string(),
                overrode: None,
            },
            LineAttribution {
                start_line: 3,
                end_line: 3,
                author_id: "ai1".to_string(),
                overrode: None,
            },
            LineAttribution {
                start_line: 4,
                end_line: 4,
                author_id: "ai1".to_string(),
                overrode: None,
            },
            LineAttribution {
                start_line: 5,
                end_line: 5,
                author_id: "ai1".to_string(),
                overrode: None,
            },
            LineAttribution {
                start_line: 6,
                end_line: 6,
                author_id: "ai1".to_string(),
                overrode: None,
            },
            LineAttribution {
                start_line: 7,
                end_line: 7,
                author_id: "ai1".to_string(),
                overrode: None,
            },
        ];

        let result = diff_based_line_attribution_transfer(old_content, new_content, &old_attrs);

        // All 6 AI lines should transfer, shifted by +1
        assert_eq!(
            result.len(),
            6,
            "Should have 6 transferred attributions (all shifted by +1)"
        );
        assert_eq!(result[0].start_line, 3); // fn a1 at line 3
        assert_eq!(result[5].start_line, 8); // fn b3 at line 8
    }

    #[test]
    fn test_diff_transfer_with_trailing_newline_mismatch() {
        // Original has no trailing newline, new has trailing newline (common after git merge)
        let old_content =
            "fn base() {}\nfn a1() {}\nfn a2() {}\nfn a3() {}\nfn b1() {}\nfn b2() {}\nfn b3() {}";
        let new_content = "// header\nfn base() {}\nfn a1() {}\nfn a2() {}\nfn a3() {}\nfn b1() {}\nfn b2() {}\nfn b3() {}\n";

        let old_attrs = vec![LineAttribution {
            start_line: 2,
            end_line: 7,
            author_id: "ai1".to_string(),
            overrode: None,
        }];

        let result = diff_based_line_attribution_transfer(old_content, new_content, &old_attrs);
        for attr in &result {
            eprintln!(
                "Line {}-{}: {}",
                attr.start_line, attr.end_line, attr.author_id
            );
        }
        // Should still transfer all 6 lines
        let total_lines: u32 = result.iter().map(|a| a.end_line - a.start_line + 1).sum();
        assert_eq!(
            total_lines, 6,
            "Should transfer all 6 AI lines even with trailing newline mismatch"
        );
    }
}
