use crate::repos::test_repo::TestRepo;
use std::fs;

fn write_base_files(repo: &TestRepo) {
    fs::write(repo.path().join("lines.md"), "base lines\n").expect("failed to write lines.md");
    fs::write(repo.path().join("alphabet.md"), "base alphabet\n")
        .expect("failed to write alphabet.md");
    repo.stage_all_and_commit("initial commit")
        .expect("initial commit should succeed");
}

#[test]
fn test_explicit_path_checkpoint_only_tracks_the_explicit_file() {
    let repo = TestRepo::new();
    write_base_files(&repo);

    fs::write(
        repo.path().join("lines.md"),
        "line touched by first checkpoint\n",
    )
    .expect("failed to update lines.md");
    repo.git_ai(&["checkpoint", "mock_ai", "lines.md"])
        .expect("first explicit checkpoint should succeed");

    fs::write(
        repo.path().join("alphabet.md"),
        "line touched by second checkpoint\n",
    )
    .expect("failed to update alphabet.md");
    repo.git_ai(&["checkpoint", "mock_ai", "alphabet.md"])
        .expect("second explicit checkpoint should succeed");

    let checkpoints = repo
        .current_working_logs()
        .read_all_checkpoints()
        .expect("checkpoints should be readable");
    let latest = checkpoints.last().expect("latest checkpoint should exist");
    let latest_files = latest
        .entries
        .iter()
        .map(|entry| entry.file.as_str())
        .collect::<Vec<_>>();

    assert_eq!(
        latest_files,
        vec!["alphabet.md"],
        "explicit path checkpoints must not expand to other dirty AI-touched files"
    );
}
