use crate::repos::test_file::ExpectedLineExt;
use crate::repos::test_repo::TestRepo;
use std::fs;

/// Cherry-pick a single AI commit onto another branch, verify AI lines kept.
#[test]
fn test_cherry_pick_single_commit_preserves_ai_attribution() {
    let repo = TestRepo::new();

    // Create initial commit on default branch
    let mut file = repo.filename("file.txt");
    file.set_contents(crate::lines!["Initial content"]);
    repo.stage_all_and_commit("Initial commit").unwrap();

    let main_branch = repo.current_branch();

    // Create feature branch with AI-authored changes
    repo.git(&["checkout", "-b", "feature"]).unwrap();
    file.insert_at(1, crate::lines!["AI feature line".ai()]);
    repo.stage_all_and_commit("Add AI feature").unwrap();
    let feature_commit = repo.git(&["rev-parse", "HEAD"]).unwrap().trim().to_string();

    // Switch back to main and cherry-pick the feature commit
    repo.git(&["checkout", &main_branch]).unwrap();
    repo.git(&["cherry-pick", &feature_commit]).unwrap();

    // Verify AI attribution is preserved through cherry-pick
    file.assert_lines_and_blame(crate::lines![
        "Initial content".ai(),
        "AI feature line".ai(),
    ]);
}

/// Cherry-pick 2+ commits sequentially and verify attribution is preserved for each.
#[test]
fn test_cherry_pick_multiple_commits_preserves_attribution() {
    let repo = TestRepo::new();

    // Create initial commit
    let mut file = repo.filename("file.txt");
    file.set_contents(crate::lines!["Line 1", ""]);
    repo.stage_all_and_commit("Initial commit").unwrap();

    let main_branch = repo.current_branch();

    // Create feature branch with multiple AI-authored commits
    repo.git(&["checkout", "-b", "feature"]).unwrap();

    // First AI commit
    file.insert_at(1, crate::lines!["AI line 2".ai()]);
    repo.stage_all_and_commit("AI commit 1").unwrap();
    let commit1 = repo.git(&["rev-parse", "HEAD"]).unwrap().trim().to_string();

    // Second AI commit
    file.insert_at(2, crate::lines!["AI line 3".ai()]);
    repo.stage_all_and_commit("AI commit 2").unwrap();
    let commit2 = repo.git(&["rev-parse", "HEAD"]).unwrap().trim().to_string();

    // Third AI commit
    file.insert_at(3, crate::lines!["AI line 4".ai()]);
    repo.stage_all_and_commit("AI commit 3").unwrap();
    let commit3 = repo.git(&["rev-parse", "HEAD"]).unwrap().trim().to_string();

    // Switch back to main and cherry-pick all three commits
    repo.git(&["checkout", &main_branch]).unwrap();
    repo.git(&["cherry-pick", &commit1, &commit2, &commit3])
        .unwrap();

    // Verify all AI lines retained attribution
    file.assert_lines_and_blame(crate::lines![
        "Line 1".human(),
        "AI line 2".ai(),
        "AI line 3".ai(),
        "AI line 4".ai(),
    ]);
}

/// Cherry-pick that conflicts, resolve manually, --continue. Verify attribution survives.
#[test]
fn test_cherry_pick_with_conflict_and_continue() {
    let repo = TestRepo::new();

    // Create initial commit on default branch
    let mut file = repo.filename("file.txt");
    file.set_contents(crate::lines!["Line 1", "Line 2", "Line 3"]);
    repo.stage_all_and_commit("Initial commit").unwrap();

    let main_branch = repo.current_branch();

    // Create feature branch with AI changes (modifies Line 2)
    repo.git(&["checkout", "-b", "feature"]).unwrap();
    file.replace_at(1, "AI_FEATURE_VERSION".ai());
    repo.stage_all_and_commit("AI feature").unwrap();
    let feature_commit = repo.git(&["rev-parse", "HEAD"]).unwrap().trim().to_string();

    // Switch back to main and make conflicting change to Line 2
    repo.git(&["checkout", &main_branch]).unwrap();
    file.replace_at(1, "MAIN_BRANCH_VERSION".human());
    repo.stage_all_and_commit("Human change").unwrap();

    // Try to cherry-pick (should conflict on Line 2)
    let cherry_pick_result = repo.git(&["cherry-pick", &feature_commit]);
    assert!(cherry_pick_result.is_err(), "Should have conflict");

    // Resolve conflict by choosing the AI version
    let file_path = repo.path().join("file.txt");
    fs::write(&file_path, "Line 1\nAI_FEATURE_VERSION\nLine 3\n").unwrap();
    repo.git(&["add", "file.txt"]).unwrap();

    // Continue cherry-pick
    repo.git(&["cherry-pick", "--continue"]).unwrap();

    // Verify AI attribution is preserved after conflict resolution
    file.assert_lines_and_blame(crate::lines![
        "Line 1".human(),
        "AI_FEATURE_VERSION".ai(),
        "Line 3".human(),
    ]);
}

/// Cherry-pick --abort doesn't corrupt notes or leave stale state.
#[test]
fn test_cherry_pick_abort_returns_to_original_state() {
    let repo = TestRepo::new();

    // Create initial commit on default branch
    let mut file = repo.filename("file.txt");
    file.set_contents(crate::lines!["Line 1", "Line 2"]);
    repo.stage_all_and_commit("Initial commit").unwrap();

    let main_branch = repo.current_branch();

    // Create feature branch with AI changes (modify Line 2)
    repo.git(&["checkout", "-b", "feature"]).unwrap();
    file.replace_at(1, "AI modification of line 2".ai());
    repo.stage_all_and_commit("AI feature").unwrap();

    // Verify AI attribution on the feature branch
    file.assert_lines_and_blame(crate::lines![
        "Line 1".human(),
        "AI modification of line 2".ai(),
    ]);

    let feature_commit = repo.git(&["rev-parse", "HEAD"]).unwrap().trim().to_string();

    // Switch back to main and make conflicting change (also modify Line 2)
    repo.git(&["checkout", &main_branch]).unwrap();
    file.replace_at(1, "Human modification of line 2".human());
    repo.stage_all_and_commit("Human change").unwrap();

    // Record state before the cherry-pick attempt
    let head_before = repo.git(&["rev-parse", "HEAD"]).unwrap().trim().to_string();

    // Try to cherry-pick (should conflict)
    let cherry_pick_result = repo.git(&["cherry-pick", &feature_commit]);
    assert!(cherry_pick_result.is_err(), "Should have conflict");

    // Abort the cherry-pick
    repo.git(&["cherry-pick", "--abort"]).unwrap();

    // Verify HEAD is back to before the cherry-pick attempt
    let current_head = repo.git(&["rev-parse", "HEAD"]).unwrap().trim().to_string();
    assert_eq!(
        current_head, head_before,
        "HEAD should return to pre-cherry-pick state after abort"
    );

    // Verify file state is unchanged (should have human's version)
    file.assert_lines_and_blame(crate::lines![
        "Line 1".human(),
        "Human modification of line 2".human(),
    ]);

    // Verify that a subsequent valid cherry-pick still works correctly
    // (abort didn't corrupt internal state)
    repo.git(&["checkout", "-b", "feature2"]).unwrap();
    file.insert_at(2, crate::lines!["New AI line".ai()]);
    repo.stage_all_and_commit("Another AI commit").unwrap();
    let feature2_commit = repo.git(&["rev-parse", "HEAD"]).unwrap().trim().to_string();

    repo.git(&["checkout", &main_branch]).unwrap();
    repo.git(&["cherry-pick", &feature2_commit]).unwrap();

    file.assert_lines_and_blame(crate::lines![
        "Line 1".human(),
        "Human modification of line 2".human(),
        "New AI line".ai(),
    ]);
}

/// Cherry-pick a human-only commit (no AI). Verify no AI attribution appears.
#[test]
fn test_cherry_pick_human_only_commit() {
    let repo = TestRepo::new();

    // Create initial commit on default branch
    let mut file = repo.filename("file.txt");
    file.set_contents(crate::lines!["Line 1"]);
    repo.stage_all_and_commit("Initial commit").unwrap();

    let main_branch = repo.current_branch();

    // Create feature branch with human-only changes (no AI)
    repo.git(&["checkout", "-b", "feature"]).unwrap();
    file.insert_at(1, crate::lines!["Human line 2".human()]);
    repo.stage_all_and_commit("Human feature").unwrap();
    let feature_commit = repo.git(&["rev-parse", "HEAD"]).unwrap().trim().to_string();

    // Switch back to main and cherry-pick
    repo.git(&["checkout", &main_branch]).unwrap();
    repo.git(&["cherry-pick", &feature_commit]).unwrap();

    // Verify no AI authorship on any line
    file.assert_lines_and_blame(crate::lines![
        "Line 1".human(),
        "Human line 2".human(),
    ]);

    // Verify that the authorship note exists (metadata-only note for human commit)
    let new_commit = repo.git(&["rev-parse", "HEAD"]).unwrap().trim().to_string();
    let note = repo.read_authorship_note(&new_commit);
    assert!(
        note.is_some(),
        "Cherry-picked human commit should still have an authorship note"
    );
}

/// --skip a conflicting commit, subsequent commits still get correct attribution.
#[test]
fn test_cherry_pick_skip_preserves_subsequent_attribution() {
    let repo = TestRepo::new();
    let mut file = repo.filename("file.txt");
    file.set_contents(crate::lines!["base line"]);
    repo.stage_all_and_commit("initial").unwrap();
    let main_branch = repo.current_branch();

    // Feature branch: three AI commits that each append one line.
    repo.git(&["checkout", "-b", "feature"]).unwrap();
    file.insert_at(1, crate::lines!["AI line 1".ai()]);
    repo.stage_all_and_commit("AI commit 1").unwrap();
    let sha1 = repo.git(&["rev-parse", "HEAD"]).unwrap().trim().to_string();

    file.insert_at(2, crate::lines!["AI line 2".ai()]);
    repo.stage_all_and_commit("AI commit 2").unwrap();
    let sha2 = repo.git(&["rev-parse", "HEAD"]).unwrap().trim().to_string();

    file.insert_at(3, crate::lines!["AI line 3".ai()]);
    repo.stage_all_and_commit("AI commit 3").unwrap();
    let sha3 = repo.git(&["rev-parse", "HEAD"]).unwrap().trim().to_string();

    repo.git(&["checkout", &main_branch]).unwrap();

    // Pre-apply sha1's change as a plain human commit so that cherry-picking sha1
    // results in an empty diff, forcing git to stop and require --skip.
    let mut main_file = repo.filename("file.txt");
    main_file.insert_at(1, crate::lines!["AI line 1"]); // no .ai() -- human commit
    repo.stage_all_and_commit("pre-apply sha1 as human")
        .unwrap();

    // Start cherry-picking all three. sha1 is now empty -> git stops with an error.
    let pick_result = repo.git(&["cherry-pick", &sha1, &sha2, &sha3]);
    assert!(
        pick_result.is_err(),
        "cherry-pick of an already-applied commit should require --skip"
    );

    // Skip the empty sha1 commit; git should then apply sha2 and sha3 automatically.
    repo.git(&["cherry-pick", "--skip"]).unwrap();

    // After skip + continuation: sha2 and sha3's AI attribution should be preserved.
    file.assert_lines_and_blame(crate::lines![
        "base line".human(),
        "AI line 1".ai(),
        "AI line 2".ai(),
        "AI line 3".ai(),
    ]);
}

/// Cherry-pick that results in no changes (already applied) is handled gracefully.
#[test]
fn test_cherry_pick_empty_commit_handled() {
    let repo = TestRepo::new();

    // Create initial commit
    let mut file = repo.filename("file.txt");
    file.set_contents(crate::lines!["Line 1"]);
    repo.stage_all_and_commit("Initial commit").unwrap();

    let main_branch = repo.current_branch();

    // Create feature branch with AI change
    repo.git(&["checkout", "-b", "feature"]).unwrap();
    file.insert_at(1, crate::lines!["Feature line".ai()]);
    repo.stage_all_and_commit("Add feature").unwrap();
    let feature_commit = repo.git(&["rev-parse", "HEAD"]).unwrap().trim().to_string();

    // Manually apply the same change to main (as human)
    repo.git(&["checkout", &main_branch]).unwrap();
    let mut file_on_main = repo.filename("file.txt");
    file_on_main.insert_at(1, crate::lines!["Feature line".human()]);
    repo.stage_all_and_commit("Apply feature manually").unwrap();

    // Try to cherry-pick the feature commit (should become empty or conflict)
    let result = repo.git(&["cherry-pick", &feature_commit]);

    // Git might succeed with an empty commit, or it might error.
    // The key assertion: no crash and no corruption of notes.
    match result {
        Ok(_) => {
            // Git handled the empty cherry-pick (possibly with --allow-empty)
        }
        Err(_) => {
            // Git reported an error (conflict or empty commit)
            // Abort the cherry-pick to clean up
            let _ = repo.git(&["cherry-pick", "--abort"]);
        }
    }

    // Verify file content is preserved regardless of outcome
    let actual_content = repo.read_file("file.txt").unwrap();
    assert!(
        actual_content.contains("Feature line"),
        "File content should contain 'Feature line' after cherry-pick or abort"
    );

    // Verify no corruption: can still make normal commits with attribution
    let mut new_file = repo.filename("new.txt");
    new_file.set_contents(crate::lines!["Post cherry-pick AI line".ai()]);
    repo.stage_all_and_commit("Post empty cherry-pick commit")
        .unwrap();
    new_file.assert_lines_and_blame(crate::lines!["Post cherry-pick AI line".ai()]);
}

crate::reuse_tests_in_worktree!(
    test_cherry_pick_single_commit_preserves_ai_attribution,
    test_cherry_pick_multiple_commits_preserves_attribution,
    test_cherry_pick_with_conflict_and_continue,
    test_cherry_pick_abort_returns_to_original_state,
    test_cherry_pick_human_only_commit,
    test_cherry_pick_skip_preserves_subsequent_attribution,
    test_cherry_pick_empty_commit_handled,
);
