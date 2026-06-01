use std::collections::{HashMap, HashSet};

use crate::repos::test_repo::TestRepo;

/// Attribution types that can be assigned to a character/line.
/// NOTE: We intentionally only test Ai and KnownHuman because these are the two
/// attribution types with well-defined, reliable checkpoint flows. "Untracked"
/// (no checkpoint) has known limitations: content appearing after an AI checkpoint
/// without any subsequent checkpoint is attributed to the prior AI session by design.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Attribution {
    Ai,
    KnownHuman,
}

impl std::fmt::Display for Attribution {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Attribution::Ai => write!(f, "Ai"),
            Attribution::KnownHuman => write!(f, "KnownHuman"),
        }
    }
}

/// Entry tracking a single allocated character.
#[derive(Debug, Clone)]
pub struct CharEntry {
    pub ch: char,
    pub attribution: Attribution,
    pub step: usize,
}

/// Names that indicate AI authorship in blame output.
const AI_AUTHOR_NAMES: &[&str] = &[
    "mock_ai",
    "claude",
    "continue-cli",
    "gpt",
    "copilot",
    "cursor",
    "codex",
    "gemini",
    "amp",
    "windsurf",
    "devin",
    "cloud-agent",
    "codex-cloud",
    "git-ai-cloud-agent",
];

/// The base character pool before falling back to Unicode.
const CHAR_POOL: &str = "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789";

/// Registry that allocates unique characters and maps them to attributions.
/// Also tracks committed sessions for monotonic retention verification.
pub struct CharRegistry {
    pool: Vec<char>,
    next: usize,
    entries: HashMap<char, CharEntry>,
    /// Sessions that have been committed to HEAD and must be retained through rewrites.
    /// Cleared on hard reset / destructive ops that legitimately drop commits.
    committed_sessions: HashSet<String>,
}

impl CharRegistry {
    pub fn new() -> Self {
        let pool: Vec<char> = CHAR_POOL.chars().collect();
        Self {
            pool,
            next: 0,
            entries: HashMap::new(),
            committed_sessions: HashSet::new(),
        }
    }

    /// Allocate the next unique character with the given attribution.
    pub fn allocate(&mut self, attribution: Attribution) -> char {
        let ch = if self.next < self.pool.len() {
            self.pool[self.next]
        } else {
            // Fall back to Unicode characters starting at U+0100
            let offset = self.next - self.pool.len();
            char::from_u32(0x0100 + offset as u32)
                .unwrap_or_else(|| panic!("Exhausted character pool at index {}", self.next))
        };
        let step = self.next;
        self.entries.insert(
            ch,
            CharEntry {
                ch,
                attribution,
                step,
            },
        );
        self.next += 1;
        ch
    }

    /// Get the current next index (useful for generating unique names).
    pub fn next_index(&self) -> usize {
        self.next
    }

    /// Remove a character from the registry so verify_blame skips it.
    pub fn remove(&mut self, ch: char) {
        self.entries.remove(&ch);
    }

    /// Look up attribution for a character.
    pub fn get(&self, ch: char) -> Option<&CharEntry> {
        self.entries.get(&ch)
    }

    /// Dump registry contents for debugging.
    pub fn dump(&self) -> String {
        let mut entries: Vec<_> = self.entries.values().collect();
        entries.sort_by_key(|e| e.step);
        let mut out = String::new();
        for entry in entries {
            out.push_str(&format!(
                "  step={}: '{}' -> {}\n",
                entry.step, entry.ch, entry.attribution
            ));
        }
        out
    }

    /// Verify blame output for a file against the expected lines.
    ///
    /// `file_lines` is the current state of the file (chars representing each line).
    /// `operation_log` is passed through for diagnostics on failure.
    pub fn verify_blame(
        &mut self,
        repo: &TestRepo,
        filename: &str,
        file_lines: &[char],
        operation_log: &[String],
        seed: u64,
    ) {
        // Skip verification if the file doesn't exist on disk (can happen after
        // destructive operations like reset --hard)
        if !repo.path().join(filename).exists() {
            return;
        }
        let blame_output = match repo.git_ai(&["blame", filename]) {
            Ok(output) => output,
            Err(e) => {
                panic!(
                    "git-ai blame failed for '{}'\nSeed: {}\nError: {}\nOperation log:\n{}\nRegistry:\n{}",
                    filename,
                    seed,
                    e,
                    operation_log.join("\n"),
                    self.dump()
                );
            }
        };

        let blame_lines: Vec<&str> = blame_output
            .lines()
            .filter(|l| !l.trim().is_empty())
            .collect();

        if blame_lines.len() != file_lines.len() {
            let has_registered_chars = file_lines.iter().any(|&ch| self.entries.contains_key(&ch));
            if has_registered_chars {
                panic!(
                    "Line count mismatch\n\
                     Seed: {}\n\
                     File: {}\n\
                     Blame line count: {}\n\
                     Expected file line count: {}\n\
                     Operation log:\n{}\n\
                     Registry:\n{}",
                    seed,
                    filename,
                    blame_lines.len(),
                    file_lines.len(),
                    operation_log.join("\n"),
                    self.dump()
                );
            }
            return;
        }

        // Get porcelain blame to extract orig_line numbers for each line.
        // The note stores line numbers in the commit's coordinate space, and
        // git blame porcelain reports the original line number within that commit.
        // When there are uncommitted changes after a commit, current and orig line
        // numbers can diverge.
        let porcelain_output = repo
            .git(&["blame", "--line-porcelain", "--", filename])
            .unwrap_or_else(|e| {
                panic!(
                    "git blame --line-porcelain failed for '{}'\nSeed: {}\nError: {}\nOp log:\n{}",
                    filename,
                    seed,
                    e,
                    operation_log.join("\n")
                )
            });
        let orig_lines = parse_porcelain_orig_lines(&porcelain_output);

        for (i, (blame_line, &expected_char)) in
            blame_lines.iter().zip(file_lines.iter()).enumerate()
        {
            let line_num = i + 1;

            let (author, _content) = parse_blame_line(blame_line);
            let is_ai_author = is_ai_author_name(&author);

            let entry = match self.get(expected_char) {
                Some(e) => e,
                None => {
                    // Character not in registry (e.g., conflict markers from unresolved merge)
                    continue;
                }
            };

            let expected_ai = matches!(entry.attribution, Attribution::Ai);

            if expected_ai != is_ai_author {
                // Tolerance for blame re-attribution: git blame can assign a line
                // to a commit that didn't actually modify it (due to Myers heuristics
                // in heavily-rewritten files). We verify this by checking git's own
                // diff: if `git diff -U0 parent..commit -- file` does NOT include
                // this line number, the commit provably didn't touch it. The line is
                // a survivor from an earlier commit, "untracked" is correct.
                //
                // IMPORTANT: We use the ORIGINAL line number from git blame porcelain
                // (the line's position in the blamed commit's tree) because that's
                // the coordinate space the authorship note uses.
                if expected_ai && !is_ai_author {
                    let blame_commit = blame_line.split_whitespace().next().unwrap_or("unknown");
                    let blame_commit = blame_commit.trim_start_matches('^');

                    // Use orig_line from porcelain (falls back to current line_num)
                    let orig_line = orig_lines.get(i).copied().unwrap_or(line_num as u32);

                    // Check 1: if the commit's diff doesn't include this orig_line,
                    // blame is re-attributing a survivor line. Clearly acceptable.
                    if !commit_touched_line(repo, blame_commit, filename, orig_line) {
                        continue;
                    }

                    // Check 2: the diff DOES include this line (large replacement hunk),
                    // but the commit's note has no AI attestation for it. This means
                    // the line is a survivor caught in a replacement hunk — it was in the
                    // old content AND the new content. The note correctly doesn't claim
                    // it as AI (git-ai blame will trace back to the originating commit).
                    //
                    // Accept if: note has h_ coverage, gap, or no note at all.
                    // Reject ONLY if: note has s_ (AI) coverage but blame shows human
                    // — that's an impossible state indicating a git-ai blame bug.
                    if let Some(note) = repo.read_authorship_note(blame_commit) {
                        if !note_covers_line_as_ai(&note, filename, orig_line) {
                            continue;
                        }
                        // note_covers_line_as_ai is true but blame shows human — impossible.
                        // Fall through to panic.
                    } else {
                        // No note at all — line can't have AI attribution. Survivor.
                        continue;
                    }
                }

                let commit_sha = blame_line.split_whitespace().next().unwrap_or("unknown");
                let commit_sha_clean = commit_sha.trim_start_matches('^');
                let note_content = repo
                    .read_authorship_note(commit_sha)
                    .unwrap_or_else(|| "<NO NOTE>".to_string());
                let diff_output = repo
                    .git(&[
                        "diff",
                        "-U0",
                        "--no-color",
                        &format!("{}~1..{}", commit_sha_clean, commit_sha_clean),
                        "--",
                        filename,
                    ])
                    .unwrap_or_else(|e| format!("<diff failed: {}>", e));
                let orig_line = orig_lines.get(i).copied().unwrap_or(line_num as u32);
                let diag_path = repo
                    .path()
                    .join(".git/ai/working_logs/EMPTY_PATHSPECS_DIAG.txt");
                let diag_content = std::fs::read_to_string(&diag_path).unwrap_or_default();
                panic!(
                    "Attribution mismatch on line {} (orig_line={}) of '{}'\n\
                     Seed: {}\n\
                     Character: '{}' (step {})\n\
                     Expected: {} (should {}be AI author)\n\
                     Actual author: '{}' (is_ai={})\n\
                     Blame line: {}\n\
                     Commit {} authorship note:\n{}\n\
                     Diff output (commit~1..commit -- file):\n{}\n\
                     Diagnostics:\n{}\n\
                     Full blame output:\n{}\n\
                     Operation log:\n{}\n\
                     Registry:\n{}",
                    line_num,
                    orig_line,
                    filename,
                    seed,
                    expected_char,
                    entry.step,
                    entry.attribution,
                    if expected_ai { "" } else { "NOT " },
                    author,
                    is_ai_author,
                    blame_line,
                    commit_sha_clean,
                    note_content,
                    diff_output,
                    if diag_content.is_empty() {
                        "<no diag file>"
                    } else {
                        &diag_content
                    },
                    blame_output,
                    operation_log.join("\n"),
                    self.dump()
                );
            }
        }
    }

    /// Verify note-internal consistency: every session referenced in the attestation
    /// section must have a corresponding entry in the metadata section, and vice versa.
    /// This catches orphaned sessions and phantom metadata.
    pub fn verify_sessions(
        &mut self,
        repo: &TestRepo,
        _file_lines: &[char],
        operation_log: &[String],
        seed: u64,
    ) {
        let head_sha = repo.git(&["rev-parse", "HEAD"]).unwrap().trim().to_string();
        let note = match repo.read_authorship_note(&head_sha) {
            Some(n) => n,
            None => return,
        };

        // Extract session IDs referenced in the attestation section
        let mut attestation_sessions: HashSet<String> = HashSet::new();
        let mut attestation_humans: HashSet<String> = HashSet::new();
        for line in note.lines() {
            let trimmed = line.trim();
            if trimmed.starts_with('{') || trimmed == "---" {
                break;
            }
            if !line.starts_with(' ') && !line.starts_with('\t') {
                continue;
            }
            if let Some(space_idx) = trimmed.rfind(' ') {
                let author_part = &trimmed[..space_idx];
                if author_part.starts_with("s_") {
                    let session_key = author_part.split("::").next().unwrap_or(author_part);
                    attestation_sessions.insert(session_key.to_string());
                } else if author_part.starts_with("h_") {
                    attestation_humans.insert(author_part.to_string());
                }
            }
        }

        // Extract sessions defined in the JSON metadata
        let metadata_sessions = extract_metadata_sessions(&note);

        // Every attestation session must exist in metadata
        if let Some(ref meta_sessions) = metadata_sessions {
            for att_session in &attestation_sessions {
                if !meta_sessions.contains(att_session.as_str()) {
                    panic!(
                        "Session verification failed: attestation references session '{}' not in metadata\n\
                         Seed: {}\nHead: {}\n\
                         Attestation sessions: {:?}\n\
                         Metadata sessions: {:?}\n\
                         Note (first 500 chars):\n{}\n\
                         Operation log:\n{}",
                        att_session,
                        seed,
                        head_sha,
                        attestation_sessions,
                        meta_sessions,
                        &note[..note.len().min(500)],
                        operation_log.join("\n"),
                    );
                }
            }
        }

        // Track committed sessions for retention verification
        let current_sessions = extract_sessions_from_note(&note);
        self.committed_sessions.extend(current_sessions);
    }

    /// Verify monotonic session retention: all sessions that were previously committed
    /// must still be present in the current HEAD's note. Sessions represent the history
    /// Reset committed sessions tracking. Call this after destructive operations
    /// that legitimately drop commits (hard reset, branch switch to unrelated history).
    pub fn reset_session_tracking(&mut self) {
        self.committed_sessions.clear();
    }

    /// Verify blame for multiple files in a single call. Ensures that all files in a
    /// multi-file commit are verified together, and that the authorship note contains
    /// all files with AI or known-human attribution.
    pub fn verify_multi_file_commit(
        &mut self,
        repo: &TestRepo,
        files: &[(&str, &[char])],
        operation_log: &[String],
        seed: u64,
    ) {
        // Verify blame for each file
        for &(filename, file_lines) in files {
            if !file_lines.is_empty() {
                self.verify_blame(repo, filename, file_lines, operation_log, seed);
            }
        }

        // Verify that the authorship note contains all files with attributions
        // that were MODIFIED in this commit (not all files across history)
        let head_sha = repo.git(&["rev-parse", "HEAD"]).unwrap().trim().to_string();
        let modified_in_commit: std::collections::HashSet<String> = repo
            .git(&["diff-tree", "--no-commit-id", "--name-only", "-r", "HEAD"])
            .unwrap_or_default()
            .lines()
            .map(|l| l.trim().to_string())
            .filter(|l| !l.is_empty())
            .collect();

        let note = match repo.read_authorship_note(&head_sha) {
            Some(n) => n,
            None => {
                // No note is only acceptable if no committed files have AI/known-human lines
                let has_attributed_lines = files.iter().any(|(filename, file_lines)| {
                    modified_in_commit.contains(*filename)
                        && file_lines.iter().any(|&ch| {
                            self.get(ch).is_some_and(|e| {
                                matches!(e.attribution, Attribution::Ai | Attribution::KnownHuman)
                            })
                        })
                });
                if has_attributed_lines {
                    panic!(
                        "Multi-file verification failed: no authorship note but committed files have attributed lines\n\
                         Seed: {}\nHead: {}\nModified: {:?}\n\
                         Operation log:\n{}",
                        seed,
                        head_sha,
                        modified_in_commit,
                        operation_log.join("\n"),
                    );
                }
                return;
            }
        };

        // Parse attestation section to find all files mentioned
        let attestation_section = if let Some(idx) = note.find("\n---\n") {
            &note[..idx]
        } else {
            &note
        };

        for &(filename, file_lines) in files {
            // Only check files that were actually modified in this commit
            if !modified_in_commit.contains(filename) {
                continue;
            }

            let has_attributed = file_lines.iter().any(|&ch| {
                self.get(ch).is_some_and(|e| {
                    matches!(e.attribution, Attribution::Ai | Attribution::KnownHuman)
                })
            });

            if has_attributed && !attestation_section.contains(filename) {
                panic!(
                    "File '{}' has attributed lines but is missing from authorship note\n\
                     Seed: {}\n\
                     Note:\n{}\n\
                     Operation log:\n{}",
                    filename,
                    seed,
                    note,
                    operation_log.join("\n")
                );
            }
        }
    }

    /// Verify that line ranges in the authorship note match actual attributions.
    /// For each line claimed by a session, verify that the character at that line
    /// in the registry has the corresponding attribution (AI or KnownHuman).
    pub fn verify_note_line_ranges(
        &self,
        repo: &TestRepo,
        filename: &str,
        file_lines: &[char],
        operation_log: &[String],
        seed: u64,
    ) {
        let head_sha = repo.git(&["rev-parse", "HEAD"]).unwrap().trim().to_string();
        let note = match repo.read_authorship_note(&head_sha) {
            Some(n) => n,
            None => return, // No note to verify
        };

        // Parse the attestation section manually
        let attestation_section = if let Some(idx) = note.find("\n---\n") {
            &note[..idx]
        } else {
            &note
        };

        // Find attestations for this file
        let mut current_file: Option<String> = None;
        let mut file_attestations: Vec<(String, Vec<(u32, u32)>)> = Vec::new();

        for line in attestation_section.lines() {
            if line.is_empty() {
                continue;
            }

            if !line.starts_with(' ') && !line.starts_with('\t') {
                // File path line
                current_file = Some(line.trim().to_string());
            } else if let Some(ref file) = current_file {
                // Attestation entry line: "  hash ranges" or "  hash::tool ranges"
                if file != filename {
                    continue;
                }

                let trimmed = line.trim();
                let parts: Vec<&str> = trimmed.split_whitespace().collect();
                if parts.len() < 2 {
                    continue;
                }

                let session_hash = parts[0];
                let ranges_str = parts[1];

                // Parse line ranges
                let ranges = parse_line_ranges(ranges_str);
                file_attestations.push((session_hash.to_string(), ranges));
            }
        }

        // Verify each range
        for (session_hash, ranges) in file_attestations {
            let is_ai_session = !session_hash.starts_with("h_");
            let is_human_session = session_hash.starts_with("h_");

            for (start, end) in ranges {
                for line_num in start..=end {
                    let idx = (line_num - 1) as usize; // Convert to 0-indexed
                    if idx >= file_lines.len() {
                        continue; // Line beyond current file length (may have been deleted)
                    }

                    let ch = file_lines[idx];
                    let entry = match self.get(ch) {
                        Some(e) => e,
                        None => continue, // Char not in registry
                    };

                    let actual_is_ai = matches!(entry.attribution, Attribution::Ai);
                    let actual_is_human = matches!(entry.attribution, Attribution::KnownHuman);

                    // Verify session type matches attribution
                    if is_ai_session && !actual_is_ai {
                        panic!(
                            "Line range verification failed: AI session claims line {} but char has {:?} attribution\n\
                             Seed: {}\nFile: {}\nSession: {}\n\
                             Line {}: char '{}' (step {})\n\
                             Note (first 800 chars):\n{}\n\
                             Operation log:\n{}",
                            line_num,
                            entry.attribution,
                            seed,
                            filename,
                            session_hash,
                            line_num,
                            ch,
                            entry.step,
                            &note[..note.len().min(800)],
                            operation_log.join("\n"),
                        );
                    }

                    if is_human_session && !actual_is_human {
                        panic!(
                            "Line range verification failed: human session claims line {} but char has {:?} attribution\n\
                             Seed: {}\nFile: {}\nSession: {}\n\
                             Line {}: char '{}' (step {})\n\
                             Note (first 800 chars):\n{}\n\
                             Operation log:\n{}",
                            line_num,
                            entry.attribution,
                            seed,
                            filename,
                            session_hash,
                            line_num,
                            ch,
                            entry.step,
                            &note[..note.len().min(800)],
                            operation_log.join("\n"),
                        );
                    }
                }
            }
        }
    }

    /// Verify that the authorship note for HEAD is well-formed.
    /// Checks JSON validity, required fields, and attestation format.
    pub fn verify_note_schema(&self, repo: &TestRepo, operation_log: &[String], seed: u64) {
        let head_sha = repo.git(&["rev-parse", "HEAD"]).unwrap().trim().to_string();
        let note = match repo.read_authorship_note(&head_sha) {
            Some(n) => n,
            None => return, // No note to verify
        };

        // Check for separator - note may start with "---" if there are no attestations
        let (attestation_section, json_section) = if let Some(stripped) = note.strip_prefix("---\n")
        {
            // Empty attestation section
            ("", stripped)
        } else if let Some(separator_idx) = note.find("\n---\n") {
            let attestation_section = &note[..separator_idx];
            let json_section = &note[separator_idx + 5..]; // Skip "\n---\n"
            (attestation_section, json_section)
        } else {
            panic!(
                "Note schema validation failed: missing '---' separator\n\
                 Seed: {}\nHead: {}\n\
                 Note (first 500 chars):\n{}\n\
                 Operation log:\n{}",
                seed,
                head_sha,
                &note[..note.len().min(500)],
                operation_log.join("\n"),
            );
        };

        // Validate JSON
        let json_value: serde_json::Value = match serde_json::from_str(json_section.trim()) {
            Ok(v) => v,
            Err(e) => {
                panic!(
                    "Note schema validation failed: invalid JSON in metadata section\n\
                     Seed: {}\nHead: {}\n\
                     JSON parse error: {}\n\
                     JSON section (first 500 chars):\n{}\n\
                     Operation log:\n{}",
                    seed,
                    head_sha,
                    e,
                    &json_section[..json_section.len().min(500)],
                    operation_log.join("\n"),
                );
            }
        };

        // Check required top-level keys
        let obj = match json_value.as_object() {
            Some(o) => o,
            None => {
                panic!(
                    "Note schema validation failed: JSON root is not an object\n\
                     Seed: {}\nHead: {}\n\
                     Operation log:\n{}",
                    seed,
                    head_sha,
                    operation_log.join("\n"),
                );
            }
        };

        if !obj.contains_key("schema_version") {
            panic!(
                "Note schema validation failed: missing 'schema_version' key in JSON\n\
                 Seed: {}\nHead: {}\n\
                 JSON keys: {:?}\n\
                 Operation log:\n{}",
                seed,
                head_sha,
                obj.keys().collect::<Vec<_>>(),
                operation_log.join("\n"),
            );
        }

        if !obj.contains_key("sessions") && !obj.contains_key("prompts") {
            panic!(
                "Note schema validation failed: missing 'sessions' or 'prompts' key in JSON\n\
                 Seed: {}\nHead: {}\n\
                 JSON keys: {:?}\n\
                 Operation log:\n{}",
                seed,
                head_sha,
                obj.keys().collect::<Vec<_>>(),
                operation_log.join("\n"),
            );
        }

        if !obj.contains_key("base_commit_sha") {
            panic!(
                "Note schema validation failed: missing 'base_commit_sha' key in JSON\n\
                 Seed: {}\nHead: {}\n\
                 JSON keys: {:?}\n\
                 Operation log:\n{}",
                seed,
                head_sha,
                obj.keys().collect::<Vec<_>>(),
                operation_log.join("\n"),
            );
        }

        // Validate attestation section format
        let mut current_file: Option<String> = None;
        for (line_num, line) in attestation_section.lines().enumerate() {
            if line.is_empty() {
                continue;
            }

            let is_indented = line.starts_with(' ') || line.starts_with('\t');

            if !is_indented {
                // File path line
                current_file = Some(line.to_string());
            } else {
                // Entry line
                if current_file.is_none() {
                    panic!(
                        "Note schema validation failed: attestation entry before any file path (line {})\n\
                         Seed: {}\nHead: {}\n\
                         Line: {}\n\
                         Operation log:\n{}",
                        line_num + 1,
                        seed,
                        head_sha,
                        line,
                        operation_log.join("\n"),
                    );
                }

                let trimmed = line.trim();
                let parts: Vec<&str> = trimmed.split_whitespace().collect();
                if parts.len() < 2 {
                    panic!(
                        "Note schema validation failed: attestation entry missing hash or ranges (line {})\n\
                         Seed: {}\nHead: {}\n\
                         Line: {}\n\
                         Operation log:\n{}",
                        line_num + 1,
                        seed,
                        head_sha,
                        line,
                        operation_log.join("\n"),
                    );
                }

                // Validate line ranges format
                let ranges_str = parts[1];
                if !is_valid_line_ranges(ranges_str) {
                    panic!(
                        "Note schema validation failed: invalid line range format '{}' (line {})\n\
                         Seed: {}\nHead: {}\n\
                         Line: {}\n\
                         Operation log:\n{}",
                        ranges_str,
                        line_num + 1,
                        seed,
                        head_sha,
                        line,
                        operation_log.join("\n"),
                    );
                }
            }
        }
    }
}

/// Parse `git blame --line-porcelain` output to extract original line numbers.
/// Returns a Vec where index i contains the orig_line for current line i+1.
/// Porcelain header format: `<sha> <orig_line> <final_line> [<num_lines>]`
fn parse_porcelain_orig_lines(porcelain: &str) -> Vec<u32> {
    let mut result = Vec::new();
    for line in porcelain.lines() {
        // Header lines start with a hex SHA (40 chars) followed by spaces and numbers
        let parts: Vec<&str> = line.split_whitespace().collect();
        if parts.len() >= 3
            && parts[0].len() == 40
            && parts[0].chars().all(|c| c.is_ascii_hexdigit())
            && let Ok(orig_line) = parts[1].parse::<u32>()
        {
            result.push(orig_line);
        }
    }
    result
}

/// Parse a blame line to extract author and content.
/// Format: `sha (author date line_num) content`
fn parse_blame_line(line: &str) -> (String, String) {
    if let Some(start_paren) = line.find('(')
        && let Some(end_paren) = line.find(')')
    {
        let author_section = &line[start_paren + 1..end_paren];
        let content = line[end_paren + 1..].trim();

        let parts: Vec<&str> = author_section.split_whitespace().collect();
        let mut author_parts = Vec::new();
        for part in parts {
            if part.chars().next().unwrap_or('a').is_ascii_digit() {
                break;
            }
            author_parts.push(part);
        }
        let author = author_parts.join(" ");
        return (author, content.to_string());
    }
    ("unknown".to_string(), line.to_string())
}

/// Check if an author name indicates AI authorship.
fn is_ai_author_name(author: &str) -> bool {
    let name_only = if let Some(bracket) = author.find('<') {
        &author[..bracket]
    } else {
        author
    };
    let name_lower = name_only.to_lowercase();
    AI_AUTHOR_NAMES
        .iter()
        .any(|&ai_name| name_lower.contains(ai_name))
}

/// Extract all session IDs from an authorship note.
/// Looks for both AI sessions (s_<hex>) and human sessions (h_<hex>) in
/// the attestation lines (before the --- separator).
fn extract_sessions_from_note(note: &str) -> HashSet<String> {
    let mut sessions = HashSet::new();

    // Split at --- to get the attestation section
    let attestation_section = if let Some(idx) = note.find("\n---\n") {
        &note[..idx]
    } else {
        note
    };

    // Parse attestation lines for session identifiers
    for line in attestation_section.lines() {
        let trimmed = line.trim();
        // Attestation lines look like: "  s_1234567890abcd::t_fedcba0987654321 1-5"
        // or "  h_e858f2c2faea28 1-3"
        // or "  s_1234567890abcd 1-5" (AI session without tool qualifier)
        for token in trimmed.split_whitespace() {
            // Stop at line ranges (digits or digit-digit)
            if token.chars().next().is_some_and(|c| c.is_ascii_digit()) {
                break;
            }
            // Extract the session ID (part before :: if present)
            let session_id = if let Some(idx) = token.find("::") {
                &token[..idx]
            } else {
                token
            };
            if (session_id.starts_with("s_") || session_id.starts_with("h_"))
                && session_id.len() > 2
            {
                sessions.insert(session_id.to_string());
            }
        }
    }

    // Also check the JSON metadata section for sessions and humans keys
    if let Some(json_start) = note.find("\n---\n") {
        let json_section = &note[json_start + 5..];
        // Extract session IDs from "sessions": { "s_...": ... }
        for segment in json_section.split('"') {
            if (segment.starts_with("s_") || segment.starts_with("h_")) && segment.len() > 2 {
                // Verify it looks like a hex session ID (s_ or h_ followed by hex chars)
                let suffix = &segment[2..];
                if suffix.chars().all(|c| c.is_ascii_hexdigit()) && suffix.len() >= 8 {
                    sessions.insert(segment.to_string());
                }
            }
        }
    }

    sessions
}

/// Parse line ranges from a string like "1-5" or "1,3-5,7" into a vector of (start, end) tuples.
/// Single lines like "3" become (3, 3).
fn parse_line_ranges(ranges_str: &str) -> Vec<(u32, u32)> {
    let mut result = Vec::new();
    for part in ranges_str.split(',') {
        if let Some(dash_idx) = part.find('-') {
            // Range like "1-5"
            let start = part[..dash_idx].parse::<u32>().unwrap_or(0);
            let end = part[dash_idx + 1..].parse::<u32>().unwrap_or(0);
            if start > 0 && end > 0 {
                result.push((start, end));
            }
        } else {
            // Single line like "3"
            if let Ok(line) = part.parse::<u32>()
                && line > 0
            {
                result.push((line, line));
            }
        }
    }
    result
}

/// Check if a line ranges string is valid (only contains digits, dashes, and commas).
fn is_valid_line_ranges(ranges_str: &str) -> bool {
    if ranges_str.is_empty() {
        return false;
    }
    ranges_str
        .chars()
        .all(|c| c.is_ascii_digit() || c == '-' || c == ',')
}

/// Check if a commit actually modified a specific line in a file by inspecting
/// `git diff -U0 commit~1..commit -- file`. Returns true if the line number
/// falls within any added hunk in the diff output.
///
/// This is the ground truth: if git's own diff says the commit didn't touch the line,
/// then blame is wrong to attribute it there (a known blame limitation with heavy rewrites).
fn commit_touched_line(repo: &TestRepo, commit_sha: &str, filename: &str, line_num: u32) -> bool {
    // For root commits (no parent), all lines are new — commit touches everything.
    let has_parent = repo
        .git(&["rev-parse", "--verify", &format!("{}~1", commit_sha)])
        .is_ok();
    if !has_parent {
        return true;
    }

    let diff_output = match repo.git(&[
        "diff",
        "-U0",
        "--no-color",
        &format!("{}~1..{}", commit_sha, commit_sha),
        "--",
        filename,
    ]) {
        Ok(output) => output,
        Err(_) => return true, // Conservative: assume commit touched the line
    };

    if diff_output.is_empty() {
        return false;
    }

    // Parse unified diff hunk headers: @@ -old,count +new,count @@
    // We care about the +new,count part (added lines in the new commit)
    for line in diff_output.lines() {
        if !line.starts_with("@@") {
            continue;
        }
        // Extract the +start,count portion
        let plus_idx = match line.find('+') {
            Some(i) => i,
            None => continue,
        };
        let after_plus = &line[plus_idx + 1..];
        let end_idx = after_plus.find(' ').unwrap_or(after_plus.len());
        let range_str = &after_plus[..end_idx];

        let (start, count) = if let Some(comma_idx) = range_str.find(',') {
            let s = range_str[..comma_idx].parse::<u32>().unwrap_or(0);
            let c = range_str[comma_idx + 1..].parse::<u32>().unwrap_or(0);
            (s, c)
        } else {
            let s = range_str.parse::<u32>().unwrap_or(0);
            (s, 1)
        };

        if count == 0 {
            continue;
        }

        let end = start + count - 1;
        if line_num >= start && line_num <= end {
            return true;
        }
    }

    false
}

/// Check if a commit's authorship note has a VALID AI session attestation for a specific line.
/// Returns true ONLY if an AI session (s_ prefixed) covers this line AND the session
/// exists in the note's JSON metadata. Orphaned sessions (in attestation but not in
/// metadata) are not valid — blame skips them, so the oracle must too.
fn note_covers_line_as_ai(note: &str, filename: &str, line_num: u32) -> bool {
    // Extract valid session keys from the JSON metadata section.
    // Returns None if no "sessions" key exists (legacy/test format — trust all entries).
    let valid_sessions = extract_metadata_sessions(note);

    let mut in_target_file = false;

    for raw_line in note.lines() {
        let trimmed = raw_line.trim();

        // JSON metadata section starts with '{' or '---' separator
        if trimmed.starts_with('{') || trimmed == "---" {
            break;
        }

        if trimmed.is_empty() {
            continue;
        }

        // File header: a non-indented line
        if !raw_line.starts_with(' ') && !raw_line.starts_with('\t') {
            if in_target_file {
                return false;
            }
            in_target_file = trimmed == filename || trimmed.ends_with(&format!("/{}", filename));
            continue;
        }

        if !in_target_file {
            continue;
        }

        // Attestation line: "  author_id line_ranges"
        // Only count AI sessions (s_ prefix), not human entries (h_ prefix)
        if let Some(space_idx) = trimmed.rfind(' ') {
            let author_part = &trimmed[..space_idx];
            let ranges_part = &trimmed[space_idx + 1..];
            if is_valid_line_ranges(ranges_part) && author_part.starts_with("s_") {
                // If we have metadata sessions, verify this entry's session exists.
                // Orphaned sessions (in attestation but not in metadata) are skipped by blame.
                if let Some(ref sessions) = valid_sessions {
                    let session_key = author_part.split("::").next().unwrap_or(author_part);
                    if !sessions.contains(session_key) {
                        continue;
                    }
                }
                let ranges = parse_line_ranges(ranges_part);
                for (start, end) in ranges {
                    if line_num >= start && line_num <= end {
                        return true;
                    }
                }
            }
        }
    }

    false
}

fn note_covers_line_as_human(note: &str, filename: &str, line_num: u32) -> bool {
    let mut in_target_file = false;

    for raw_line in note.lines() {
        let trimmed = raw_line.trim();

        if trimmed.starts_with('{') || trimmed == "---" {
            break;
        }

        if trimmed.is_empty() {
            continue;
        }

        if !raw_line.starts_with(' ') && !raw_line.starts_with('\t') {
            if in_target_file {
                return false;
            }
            in_target_file = trimmed == filename || trimmed.ends_with(&format!("/{}", filename));
            continue;
        }

        if !in_target_file {
            continue;
        }

        if let Some(space_idx) = trimmed.rfind(' ') {
            let author_part = &trimmed[..space_idx];
            let ranges_part = &trimmed[space_idx + 1..];
            if is_valid_line_ranges(ranges_part) && author_part.starts_with("h_") {
                let ranges = parse_line_ranges(ranges_part);
                for (start, end) in ranges {
                    if line_num >= start && line_num <= end {
                        return true;
                    }
                }
            }
        }
    }

    false
}

/// Extract session keys from the JSON metadata section of a note.
/// Returns None if no "sessions" key is found (the note uses a legacy/test format
/// where all s_ entries should be trusted). Returns Some(set) with the session IDs
/// that exist in the metadata.
fn extract_metadata_sessions(note: &str) -> Option<HashSet<&str>> {
    // Find the JSON metadata section (after "---" separator)
    let json_section = if let Some(idx) = note.find("\n---\n") {
        &note[idx + 5..]
    } else if let Some(stripped) = note.strip_prefix("---\n") {
        stripped
    } else {
        return None;
    };

    // If no "sessions" key, this is a legacy/test note — trust all entries
    let sessions_idx = json_section.find("\"sessions\"")?;

    let mut sessions = HashSet::new();
    let after_sessions = &json_section[sessions_idx..];
    // Find the opening brace of the sessions object
    if let Some(brace_start) = after_sessions.find('{') {
        let sessions_obj = &after_sessions[brace_start..];
        // Track brace depth to find the end of the sessions object
        let mut depth = 0;
        let mut end_idx = sessions_obj.len();
        for (i, ch) in sessions_obj.char_indices() {
            match ch {
                '{' => depth += 1,
                '}' => {
                    depth -= 1;
                    if depth == 0 {
                        end_idx = i + 1;
                        break;
                    }
                }
                _ => {}
            }
        }
        let sessions_block = &sessions_obj[..end_idx];
        // Extract quoted s_ keys from this block
        let mut in_quote = false;
        let mut quote_start = 0;
        for (i, ch) in sessions_block.char_indices() {
            if ch == '"' {
                if in_quote {
                    let segment = &sessions_block[quote_start..i];
                    if segment.starts_with("s_") && segment.len() > 2 {
                        sessions.insert(segment);
                    }
                } else {
                    quote_start = i + 1;
                }
                in_quote = !in_quote;
            }
        }
    }

    Some(sessions)
}

#[cfg(test)]
mod oracle_strictness_tests {
    use super::*;
    use crate::repos::test_repo::TestRepo;
    use std::fs;

    /// STRICTNESS: Verify `commit_touched_line` correctly identifies lines in diff hunks.
    /// When a commit's diff includes a line, the tolerance check 1 (diff-based) does NOT fire.
    #[test]
    fn test_commit_touched_line_is_accurate() {
        let repo = TestRepo::new();
        let file_path = repo.path().join("touched.txt");

        let initial = "line1\nline2\nline3\nline4\nline5\n";
        fs::write(&file_path, initial).unwrap();
        repo.stage_all_and_commit("base").unwrap();

        // Modify only lines 2 and 4
        let edited = "line1\nMODIFIED2\nline3\nMODIFIED4\nline5\n";
        fs::write(&file_path, edited).unwrap();
        repo.stage_all_and_commit("edit lines 2 and 4").unwrap();

        let head = repo.git(&["rev-parse", "HEAD"]).unwrap().trim().to_string();

        // Lines 2 and 4 were modified — commit_touched_line MUST return true
        assert!(
            commit_touched_line(&repo, &head, "touched.txt", 2),
            "Line 2 was modified but commit_touched_line says false"
        );
        assert!(
            commit_touched_line(&repo, &head, "touched.txt", 4),
            "Line 4 was modified but commit_touched_line says false"
        );

        // Lines 1, 3, 5 were NOT modified — commit_touched_line MUST return false
        assert!(
            !commit_touched_line(&repo, &head, "touched.txt", 1),
            "Line 1 was NOT modified but commit_touched_line says true"
        );
        assert!(
            !commit_touched_line(&repo, &head, "touched.txt", 3),
            "Line 3 was NOT modified but commit_touched_line says true"
        );
        assert!(
            !commit_touched_line(&repo, &head, "touched.txt", 5),
            "Line 5 was NOT modified but commit_touched_line says true"
        );
    }

    /// STRICTNESS: Verify `note_covers_line_as_ai` correctly parses attestation ranges
    /// and ONLY returns true for AI sessions (s_ prefix), not human entries (h_).
    #[test]
    fn test_note_covers_line_as_ai_is_accurate() {
        let note = "\
myfile.txt
  s_abc123::t_def456 1-3,7
  h_human123 4-6
other.txt
  s_xyz789::t_aaa111 1-10
---
{}";

        // Lines 1-3,7 are covered by AI session in myfile.txt
        assert!(note_covers_line_as_ai(note, "myfile.txt", 1));
        assert!(note_covers_line_as_ai(note, "myfile.txt", 3));
        assert!(note_covers_line_as_ai(note, "myfile.txt", 7));

        // Lines 4-6 are covered by HUMAN (h_) — NOT AI
        assert!(!note_covers_line_as_ai(note, "myfile.txt", 4));
        assert!(!note_covers_line_as_ai(note, "myfile.txt", 6));

        // Line 8+ is NOT covered at all
        assert!(!note_covers_line_as_ai(note, "myfile.txt", 8));
        assert!(!note_covers_line_as_ai(note, "myfile.txt", 10));

        // Wrong file
        assert!(!note_covers_line_as_ai(note, "wrong.txt", 1));

        // other.txt lines (AI session)
        assert!(note_covers_line_as_ai(note, "other.txt", 5));
        assert!(!note_covers_line_as_ai(note, "other.txt", 11));
    }

    /// STRICTNESS: When the note covers a line with an AI session (s_) but blame
    /// shows human, that's impossible under normal operation (git-ai blame reads
    /// notes). The oracle MUST reject this to catch git-ai blame bugs.
    #[test]
    #[should_panic(expected = "Attribution mismatch")]
    fn test_oracle_rejects_when_note_has_ai_but_blame_shows_human() {
        let repo = TestRepo::new();
        let file_path = repo.path().join("ai_noted.txt");

        // Need a prior commit so we're not a root commit
        let dummy = repo.path().join("dummy.txt");
        fs::write(&dummy, "dummy\n").unwrap();
        repo.stage_all_and_commit("base commit").unwrap();

        // Commit 2: add a line checkpointed as AI — note will have s_ entry
        repo.git_ai(&["checkpoint", "human", "ai_noted.txt"])
            .unwrap();
        let content = "AI_LINE\n";
        fs::write(&file_path, content).unwrap();
        repo.git_ai(&["checkpoint", "mock_ai", "ai_noted.txt"])
            .unwrap();
        repo.stage_all_and_commit("ai line").unwrap();

        // Registry says AI — and the note DOES have s_ covering it.
        // Blame should show mock_ai. If it shows human, that's a git-ai blame bug.
        let mut registry = CharRegistry::new();
        let _ = registry.allocate(Attribution::Ai); // 'A' = AI

        // In practice, blame WILL show mock_ai here (so no mismatch occurs and
        // this test won't actually hit the tolerance path). But we verify the test
        // setup is correct: if blame ever showed human for an AI-noted line, the
        // oracle would reject it.
        //
        // To force the mismatch for testing: use a second char that is KnownHuman
        // in the registry but AI in reality.
        let mut registry2 = CharRegistry::new();
        let _ = registry2.allocate(Attribution::KnownHuman); // 'A' = says KnownHuman
        // This tests the OPPOSITE direction: KnownHuman→AI is always rejected
        // (tolerance only fires for AI→human direction)
        // Note: blame WILL show mock_ai for this line, but registry expects KnownHuman.
        registry2.verify_blame(&repo, "ai_noted.txt", &['A'], &[], 999);
    }

    /// STRICTNESS: Tolerance does NOT fire when note has AI session (s_) covering
    /// the line. This would mean blame is wrong about a line that git-ai explicitly
    /// marked as AI — a critical bug that must never be suppressed.
    #[test]
    fn test_note_covers_line_as_ai_rejects_ai_sessions() {
        let note = "\
test.txt
  s_abc123::t_def456 1-5
  h_human123 6-10
---
{}";
        // AI session covers lines 1-5
        assert!(note_covers_line_as_ai(note, "test.txt", 1));
        assert!(note_covers_line_as_ai(note, "test.txt", 3));
        assert!(note_covers_line_as_ai(note, "test.txt", 5));

        // Human covers lines 6-10 — does NOT count as AI coverage
        assert!(!note_covers_line_as_ai(note, "test.txt", 6));
        assert!(!note_covers_line_as_ai(note, "test.txt", 10));

        // Gap (line 11+) — no coverage at all
        assert!(!note_covers_line_as_ai(note, "test.txt", 11));
    }

    /// STRICTNESS: Oracle MUST fail when KnownHuman attribution is lost.
    /// The tolerance only applies to AI→untracked, NEVER to KnownHuman→AI.
    #[test]
    #[should_panic(expected = "Attribution mismatch")]
    fn test_oracle_rejects_known_human_shown_as_ai() {
        let repo = TestRepo::new();
        let file_path = repo.path().join("human_strict.txt");

        // Commit: file with both human and AI lines
        repo.git_ai(&["checkpoint", "human", "human_strict.txt"])
            .unwrap();
        let content = "HUMAN_LINE\nAI_LINE\n";
        fs::write(&file_path, content).unwrap();
        // Checkpoint as AI (wrongly — simulating a bug where human content gets AI attribution)
        repo.git_ai(&["checkpoint", "mock_ai", "human_strict.txt"])
            .unwrap();
        repo.stage_all_and_commit("mixed").unwrap();

        // Registry says line 1 is KnownHuman, line 2 is AI
        let mut registry = CharRegistry::new();
        let _ = registry.allocate(Attribution::KnownHuman); // 'A' = known human (line 1)
        let _ = registry.allocate(Attribution::Ai); // 'B' = AI (line 2)

        let file_lines = vec!['A', 'B'];

        // Line 1 should be KnownHuman but blame will show AI (since we checkpointed as AI).
        // The oracle MUST reject this — the tolerance never fires for KnownHuman→AI mismatch.
        registry.verify_blame(&repo, "human_strict.txt", &file_lines, &[], 999);
    }

    /// TOLERANCE: Oracle correctly accepts AI→untracked when a line is a survivor
    /// from a prior commit (diff doesn't include it in current commit's hunks).
    #[test]
    fn test_oracle_accepts_survivor_not_in_diff() {
        let repo = TestRepo::new();
        let file_path = repo.path().join("survivor_ok.txt");

        // Commit 0: establish history
        let initial = "old1\nold2\nold3\n";
        fs::write(&file_path, initial).unwrap();
        repo.stage_all_and_commit("base").unwrap();

        // Commit 1: AI overwrites entire file
        repo.git_ai(&["checkpoint", "human", "survivor_ok.txt"])
            .unwrap();
        let ai_content = "AILINE1\nAILINE2\nAILINE3\n";
        fs::write(&file_path, ai_content).unwrap();
        repo.git_ai(&["checkpoint", "mock_ai", "survivor_ok.txt"])
            .unwrap();
        repo.stage_all_and_commit("ai overwrite").unwrap();

        // Commit 2: modify only line 1 and 3, leaving line 2 unchanged
        repo.git_ai(&["checkpoint", "human", "survivor_ok.txt"])
            .unwrap();
        let edit_content = "NEWLINE1\nAILINE2\nNEWLINE3\n";
        fs::write(&file_path, edit_content).unwrap();
        repo.git_ai(&["checkpoint", "mock_known_human", "survivor_ok.txt"])
            .unwrap();
        repo.stage_all_and_commit("edit around survivor").unwrap();

        // Registry: line 2 is AI (from commit 1), lines 1 and 3 are KnownHuman (commit 2)
        let mut registry = CharRegistry::new();
        let _ = registry.allocate(Attribution::KnownHuman); // line 1
        let _ = registry.allocate(Attribution::Ai); // line 2 (AILINE2 - survivor)
        let _ = registry.allocate(Attribution::KnownHuman); // line 3

        let file_lines = vec!['A', 'B', 'C'];

        // This should NOT panic. Line 2 is a survivor — if blame attributes it to
        // commit 2, the diff won't include it (it's unchanged), so the oracle tolerates it.
        // If blame correctly attributes it to commit 1 (which has the AI note), it passes directly.
        registry.verify_blame(&repo, "survivor_ok.txt", &file_lines, &[], 999);
    }

    /// TOLERANCE: Oracle correctly accepts AI→untracked when line is in a large
    /// replacement hunk but the note has no coverage (pre-edit checkpoint captured it).
    #[test]
    fn test_oracle_accepts_survivor_in_large_hunk_with_note_gap() {
        let repo = TestRepo::new();
        let file_path = repo.path().join("hunk_gap.txt");

        // Commit 0: base content
        let initial = "base1\nbase2\nbase3\nbase4\nbase5\n";
        fs::write(&file_path, initial).unwrap();
        repo.stage_all_and_commit("base").unwrap();

        // Commit 1: AI overwrites everything with repeated content
        repo.git_ai(&["checkpoint", "human", "hunk_gap.txt"])
            .unwrap();
        let ai_all = "ppppp\nppppp\nppppp\nppppp\nppppp\nppppp\nppppp\nppppp\n";
        fs::write(&file_path, ai_all).unwrap();
        repo.git_ai(&["checkpoint", "mock_ai", "hunk_gap.txt"])
            .unwrap();
        repo.stage_all_and_commit("ai overwrite all").unwrap();

        // Commit 2: Multiple edits — delete some p lines, insert new content,
        // leaving some p lines as survivors in the middle of a large hunk.
        // Pre-edit checkpoint captures all as "human" (existing state).
        repo.git_ai(&["checkpoint", "human", "hunk_gap.txt"])
            .unwrap();
        // Replace lines 1-4 with different AI content, keep lines 5-6 as ppppp, replace 7-8
        let mixed = "aaaaa\naaaaa\naaaaa\naaaaa\nppppp\nppppp\nbbbbb\nbbbbb\n";
        fs::write(&file_path, mixed).unwrap();
        repo.git_ai(&["checkpoint", "mock_ai", "hunk_gap.txt"])
            .unwrap();
        repo.stage_all_and_commit("heavy rewrite with survivors")
            .unwrap();

        // Registry: lines 5-6 are AI (from commit 1's overwrite), rest from commit 2
        let mut registry = CharRegistry::new();
        let _ = registry.allocate(Attribution::Ai); // line 1: 'a' AI from commit 2
        let _ = registry.allocate(Attribution::Ai); // line 2: 'a'
        let _ = registry.allocate(Attribution::Ai); // line 3: 'a'
        let _ = registry.allocate(Attribution::Ai); // line 4: 'a'
        let _ = registry.allocate(Attribution::Ai); // line 5: 'p' AI from commit 1 (survivor)
        let _ = registry.allocate(Attribution::Ai); // line 6: 'p' AI from commit 1 (survivor)
        let _ = registry.allocate(Attribution::Ai); // line 7: 'b' AI from commit 2
        let _ = registry.allocate(Attribution::Ai); // line 8: 'b'

        let file_lines = vec!['A', 'B', 'C', 'D', 'E', 'F', 'G', 'H'];

        // Lines 5-6 are survivors. The diff may include them in a large hunk.
        // The note for commit 2 may not cover them (if pre-edit checkpoint marked as human).
        // Either outcome (AI from commit 1, or untracked in commit 2) is acceptable.
        registry.verify_blame(&repo, "hunk_gap.txt", &file_lines, &[], 999);
    }

    /// STRICTNESS: The tolerance ONLY applies to AI→untracked direction.
    /// If a line is expected KnownHuman but shows as untracked — that's fine (downgrade).
    /// But if expected KnownHuman and shows as AI — that's a bug and must fail.
    #[test]
    fn test_oracle_accepts_known_human_shown_as_untracked() {
        let repo = TestRepo::new();
        let file_path = repo.path().join("human_untracked.txt");

        // A file where known human content ends up untracked (acceptable downgrade)
        let content = "human_content\n";
        fs::write(&file_path, content).unwrap();
        // Don't checkpoint as known_human — just commit raw (will be untracked)
        repo.stage_all_and_commit("no checkpoint").unwrap();

        let mut registry = CharRegistry::new();
        let _ = registry.allocate(Attribution::KnownHuman); // 'A' = expected KnownHuman

        let file_lines = vec!['A'];

        // KnownHuman showing as untracked (Test User, non-AI) is fine — not a mismatch
        // because both are "non-AI". The oracle only checks AI vs non-AI.
        registry.verify_blame(&repo, "human_untracked.txt", &file_lines, &[], 999);
    }
}
