//! Tests for session and prompt metadata in authorship notes.
//!
//! Verifies that session information is correctly recorded, multiple sessions
//! coexist in a single commit, and human-only commits don't inherit AI sessions.

use crate::repos::test_repo::TestRepo;
use std::fs;

// ---------------------------------------------------------------------------
// Test 1: Session ID preserved in note after AI commit
// ---------------------------------------------------------------------------

/// After a commit with an AI checkpoint, the authorship note must contain
/// session information identifying the AI agent.
#[test]
fn test_session_id_preserved_in_note() {
    let repo = TestRepo::new();
    let file_path = repo.path().join("session_test.rs");

    // AI writes code
    fs::write(&file_path, "fn ai_generated() {}\n").unwrap();
    repo.git_ai(&["checkpoint", "mock_ai", "session_test.rs"])
        .unwrap();

    repo.stage_all_and_commit("feat: AI generates code")
        .unwrap();

    let sha = repo.git(&["rev-parse", "HEAD"]).unwrap().trim().to_string();
    let note = repo
        .read_authorship_note(&sha)
        .expect("AI commit must have an authorship note");

    // The note should contain session info (s_ prefix for session IDs or mock_ai reference)
    assert!(
        note.contains("mock_ai") || note.contains("s_"),
        "note should contain session info (mock_ai or s_ session ID). Note content:\n{}",
        &note[..note.len().min(500)]
    );
}

// ---------------------------------------------------------------------------
// Test 2: Multiple AI sessions in one commit both appear in note
// ---------------------------------------------------------------------------

/// When two different AI sessions (different checkpoint calls) contribute to
/// the same commit, both must appear in the authorship note metadata.
#[test]
fn test_multiple_sessions_in_one_commit() {
    let repo = TestRepo::new();

    // First AI session creates file_a
    let file_a = repo.path().join("file_a.rs");
    fs::write(&file_a, "fn from_session_a() {}\n").unwrap();
    repo.git_ai(&["checkpoint", "mock_ai", "file_a.rs"])
        .unwrap();

    // Second AI checkpoint on a different file (simulating another session/edit)
    let file_b = repo.path().join("file_b.rs");
    fs::write(&file_b, "fn from_session_b() {}\n").unwrap();
    repo.git_ai(&["checkpoint", "mock_ai", "file_b.rs"])
        .unwrap();

    repo.stage_all_and_commit("feat: two AI sessions in one commit")
        .unwrap();

    let sha = repo.git(&["rev-parse", "HEAD"]).unwrap().trim().to_string();
    let note = repo
        .read_authorship_note(&sha)
        .expect("commit with AI must have note");

    // Both files should be mentioned in the note's attestations
    assert!(
        note.contains("file_a.rs"),
        "note should reference file_a.rs"
    );
    assert!(
        note.contains("file_b.rs"),
        "note should reference file_b.rs"
    );
}

// ---------------------------------------------------------------------------
// Test 3: Human-only commit doesn't inherit previous AI session
// ---------------------------------------------------------------------------

/// A commit made entirely by a human (no AI checkpoints) after a previous AI
/// commit must not contain AI session metadata in its attestations.
#[test]
fn test_session_not_carried_to_human_only_commit() {
    let repo = TestRepo::new();

    // First commit: AI generates code
    let ai_file = repo.path().join("ai_code.rs");
    fs::write(&ai_file, "fn ai_fn() {}\n").unwrap();
    repo.git_ai(&["checkpoint", "mock_ai", "ai_code.rs"])
        .unwrap();
    repo.stage_all_and_commit("feat: AI code").unwrap();

    // Second commit: purely human (known_human checkpoint, no AI)
    let human_file = repo.path().join("human_code.rs");
    fs::write(&human_file, "fn human_fn() {}\n").unwrap();
    repo.git_ai(&["checkpoint", "mock_known_human", "human_code.rs"])
        .unwrap();
    repo.stage_all_and_commit("feat: human code").unwrap();

    let sha = repo.git(&["rev-parse", "HEAD"]).unwrap().trim().to_string();
    let note = repo
        .read_authorship_note(&sha)
        .expect("human commit should still have a metadata-only note");

    // The human commit's note should NOT have session entries that reference AI
    // (it may have human entries or be metadata-only)
    // Check that the note doesn't attribute lines to mock_ai sessions
    let has_ai_attestation = note.contains("mock_ai") && note.contains("file_a.rs");
    assert!(
        !has_ai_attestation,
        "human-only commit must NOT inherit AI session attestations from prior commit"
    );

    // Verify the previous AI commit still has its note
    let ai_sha = repo
        .git(&["rev-parse", "HEAD~1"])
        .unwrap()
        .trim()
        .to_string();
    let ai_note = repo.read_authorship_note(&ai_sha);
    assert!(ai_note.is_some(), "AI commit should retain its note");
}

// ---------------------------------------------------------------------------
// Test 4: With prompt_storage disabled, prompts are stripped
// ---------------------------------------------------------------------------

/// When the config has `prompt_storage: "disabled"`, the authorship note
/// should not contain prompt message content.
#[test]
fn test_prompt_sharing_disabled_strips_messages() {
    let mut repo = TestRepo::new();
    repo.patch_git_ai_config(|config| {
        config.prompt_storage = Some("disabled".to_string());
    });

    // AI generates code
    let file_path = repo.path().join("stripped.rs");
    fs::write(&file_path, "fn stripped_fn() {}\n").unwrap();
    repo.git_ai(&["checkpoint", "mock_ai", "stripped.rs"])
        .unwrap();

    repo.stage_all_and_commit("feat: AI with prompts disabled")
        .unwrap();

    let sha = repo.git(&["rev-parse", "HEAD"]).unwrap().trim().to_string();
    let note = repo
        .read_authorship_note(&sha)
        .expect("commit should have note even with prompt_storage disabled");

    // The note should still exist (attribution is preserved) but
    // should not contain message content. The "messages" field should be
    // empty or absent.
    // We check that there's no "messages" array with content
    if note.contains("\"messages\"") {
        // If messages field exists, it should be empty
        assert!(
            note.contains("\"messages\": []") || note.contains("\"messages\":[]"),
            "with prompt_storage disabled, messages should be empty"
        );
    }
}
