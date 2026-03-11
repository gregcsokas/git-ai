#[macro_use]
mod repos;

use git_ai::authorship::working_log::CheckpointKind;
use git_ai::daemon::{ControlRequest, ControlResponse, send_control_request};
use repos::test_repo::{GitTestMode, TestRepo, get_binary_path};
use serde_json::Value;
use serial_test::serial;
use std::fs;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::thread;
use std::time::Duration;

fn git_common_dir(repo: &TestRepo) -> PathBuf {
    let common_dir = PathBuf::from(
        repo.git(&["rev-parse", "--git-common-dir"])
            .expect("failed to resolve git common dir")
            .trim(),
    );
    if common_dir.is_absolute() {
        common_dir
    } else {
        repo.path().join(common_dir)
    }
}

fn family_state_path(repo: &TestRepo) -> PathBuf {
    git_common_dir(repo)
        .join("ai")
        .join("state")
        .join("daemon-v1")
        .join("family_state.json")
}

fn daemon_control_socket_path(repo: &TestRepo) -> PathBuf {
    repo.test_home_path()
        .join(".git-ai")
        .join("internal")
        .join("daemon")
        .join("control.sock")
}

fn repo_workdir_string(repo: &TestRepo) -> String {
    repo.path().to_string_lossy().to_string()
}

struct DaemonGuard {
    child: Child,
    control_socket_path: PathBuf,
    repo_working_dir: String,
}

impl DaemonGuard {
    fn start(repo: &TestRepo, mode: &str) -> Self {
        let mut command = Command::new(get_binary_path());
        command
            .arg("daemon")
            .arg("start")
            .arg("--mode")
            .arg(mode)
            .current_dir(repo.path())
            .env("HOME", repo.test_home_path())
            .env(
                "GIT_CONFIG_GLOBAL",
                repo.test_home_path().join(".gitconfig"),
            )
            .env("GIT_AI_TEST_DB_PATH", repo.test_db_path())
            .env("GITAI_TEST_DB_PATH", repo.test_db_path())
            .stdout(Stdio::null())
            .stderr(Stdio::null());

        let child = command
            .spawn()
            .expect("failed to spawn git-ai daemon subprocess");
        let mut daemon = Self {
            child,
            control_socket_path: daemon_control_socket_path(repo),
            repo_working_dir: repo_workdir_string(repo),
        };
        daemon.wait_until_ready();
        daemon
    }

    fn request(&self, request: ControlRequest) -> ControlResponse {
        send_control_request(&self.control_socket_path, &request)
            .unwrap_or_else(|e| panic!("control request failed: {}", e))
    }

    fn latest_seq_and_wait_idle(&self) -> u64 {
        let status = self.request(ControlRequest::StatusFamily {
            repo_working_dir: self.repo_working_dir.clone(),
        });
        assert!(status.ok, "status request should succeed");
        let latest_seq = status
            .data
            .as_ref()
            .and_then(|v| v.get("latest_seq"))
            .and_then(Value::as_u64)
            .unwrap_or(0);
        if latest_seq > 0 {
            let barrier = self.request(ControlRequest::BarrierAppliedThroughSeq {
                repo_working_dir: self.repo_working_dir.clone(),
                seq: latest_seq,
            });
            assert!(barrier.ok, "barrier request should succeed");
        }
        latest_seq
    }

    fn wait_until_ready(&mut self) {
        for _ in 0..200 {
            if let Some(status) = self
                .child
                .try_wait()
                .expect("failed to poll daemon process status")
            {
                panic!("daemon exited before becoming ready: {}", status);
            }
            if self.control_socket_path.exists() {
                let status = send_control_request(
                    &self.control_socket_path,
                    &ControlRequest::StatusFamily {
                        repo_working_dir: self.repo_working_dir.clone(),
                    },
                );
                if status.is_ok() {
                    return;
                }
            }
            thread::sleep(Duration::from_millis(25));
        }
        panic!(
            "daemon did not become ready at {}",
            self.control_socket_path.display()
        );
    }

    fn shutdown(&mut self) {
        if self
            .child
            .try_wait()
            .expect("failed polling daemon process")
            .is_some()
        {
            return;
        }

        let _ = send_control_request(&self.control_socket_path, &ControlRequest::Shutdown);

        for _ in 0..200 {
            if self
                .child
                .try_wait()
                .expect("failed polling daemon process")
                .is_some()
            {
                return;
            }
            thread::sleep(Duration::from_millis(10));
        }

        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl Drop for DaemonGuard {
    fn drop(&mut self) {
        self.shutdown();
    }
}

#[test]
#[serial]
fn checkpoint_delegate_falls_back_when_daemon_is_unavailable() {
    let repo = TestRepo::new_with_mode(GitTestMode::Wrapper);

    fs::write(repo.path().join("delegate-fallback.txt"), "base\n").expect("failed to write base");
    repo.git(&["add", "delegate-fallback.txt"])
        .expect("add should succeed");
    repo.stage_all_and_commit("base commit")
        .expect("base commit should succeed");

    fs::write(
        repo.path().join("delegate-fallback.txt"),
        "base\nchanged without daemon\n",
    )
    .expect("failed to write updated file");

    repo.git_ai_with_env(
        &["checkpoint", "mock_ai", "delegate-fallback.txt"],
        &[("GIT_AI_DAEMON_CHECKPOINT_DELEGATE", "true")],
    )
    .expect("checkpoint should fall back to local mode");

    let checkpoints = repo
        .current_working_logs()
        .read_all_checkpoints()
        .expect("checkpoints should be readable");
    assert!(
        checkpoints
            .iter()
            .any(|checkpoint| checkpoint.kind == CheckpointKind::AiAgent),
        "local fallback should write ai_agent checkpoint when daemon is unavailable"
    );
}

#[test]
#[serial]
fn daemon_write_mode_applies_delegated_checkpoint_and_updates_state() {
    let repo = TestRepo::new_with_mode(GitTestMode::Wrapper);
    let daemon = DaemonGuard::start(&repo, "write");

    fs::write(repo.path().join("delegate-write.txt"), "base\n").expect("failed to write base");
    repo.git(&["add", "delegate-write.txt"])
        .expect("add should succeed");
    repo.stage_all_and_commit("base commit")
        .expect("base commit should succeed");

    fs::write(
        repo.path().join("delegate-write.txt"),
        "base\nwritten by delegated checkpoint\n",
    )
    .expect("failed to write updated file");

    repo.git_ai_with_env(
        &["checkpoint", "mock_ai", "delegate-write.txt"],
        &[("GIT_AI_DAEMON_CHECKPOINT_DELEGATE", "true")],
    )
    .expect("delegated checkpoint should succeed");

    daemon.latest_seq_and_wait_idle();

    let checkpoints = repo
        .current_working_logs()
        .read_all_checkpoints()
        .expect("checkpoints should be readable");
    assert!(
        checkpoints
            .iter()
            .any(|checkpoint| checkpoint.kind == CheckpointKind::AiAgent),
        "write-mode daemon should execute checkpoint side effect"
    );

    let family_state_raw = fs::read_to_string(family_state_path(&repo))
        .expect("family state should exist after delegated checkpoint");
    let family_state: Value =
        serde_json::from_str(&family_state_raw).expect("family state should be valid json");
    let checkpoints_map = family_state
        .get("checkpoints")
        .and_then(Value::as_object)
        .expect("family state should contain checkpoints map");
    assert!(
        !checkpoints_map.is_empty(),
        "daemon family state should record delegated checkpoint summary"
    );
}

#[test]
#[serial]
fn daemon_shadow_mode_tracks_checkpoint_without_applying_side_effects() {
    let repo = TestRepo::new_with_mode(GitTestMode::Wrapper);
    let daemon = DaemonGuard::start(&repo, "shadow");

    fs::write(repo.path().join("delegate-shadow.txt"), "base\n").expect("failed to write base");
    repo.git(&["add", "delegate-shadow.txt"])
        .expect("add should succeed");
    repo.stage_all_and_commit("base commit")
        .expect("base commit should succeed");

    fs::write(
        repo.path().join("delegate-shadow.txt"),
        "base\ntracked in shadow mode only\n",
    )
    .expect("failed to write updated file");

    repo.git_ai_with_env(
        &["checkpoint", "mock_ai", "delegate-shadow.txt"],
        &[("GIT_AI_DAEMON_CHECKPOINT_DELEGATE", "true")],
    )
    .expect("shadow delegated checkpoint should succeed");

    daemon.latest_seq_and_wait_idle();

    let checkpoints = repo
        .current_working_logs()
        .read_all_checkpoints()
        .expect("checkpoints should be readable");
    assert!(
        checkpoints.is_empty(),
        "shadow-mode daemon should not apply checkpoint side effects"
    );

    let family_state_raw = fs::read_to_string(family_state_path(&repo))
        .expect("family state should exist after shadow delegated checkpoint");
    let family_state: Value =
        serde_json::from_str(&family_state_raw).expect("family state should be valid json");
    let checkpoints_map = family_state
        .get("checkpoints")
        .and_then(Value::as_object)
        .expect("family state should contain checkpoints map");
    assert!(
        !checkpoints_map.is_empty(),
        "shadow-mode daemon should still track checkpoint summaries in state"
    );
}

#[test]
#[serial]
fn daemon_trace_mirror_preserves_amend_rewrite_parity_and_records_command() {
    let repo = TestRepo::new_with_mode(GitTestMode::Wrapper);
    let daemon = DaemonGuard::start(&repo, "write");
    let control_socket = daemon_control_socket_path(&repo);
    let control_socket_str = control_socket.to_string_lossy().to_string();
    let daemon_env = [
        ("GIT_AI_DAEMON_MIRROR_TRACE", "true"),
        ("GIT_AI_DAEMON_CONTROL_SOCKET", control_socket_str.as_str()),
    ];

    fs::write(repo.path().join("trace-mirror.txt"), "line 1\n").expect("failed to write file");
    repo.git_with_env(&["add", "trace-mirror.txt"], &daemon_env, None)
        .expect("add should succeed");
    repo.git_with_env(&["commit", "-m", "initial"], &daemon_env, None)
        .expect("initial commit should succeed");

    fs::write(repo.path().join("trace-mirror.txt"), "line 1\nline 2\n")
        .expect("failed to update file");
    repo.git_with_env(&["add", "trace-mirror.txt"], &daemon_env, None)
        .expect("add before amend should succeed");
    repo.git_with_env(
        &["commit", "--amend", "-m", "initial amended"],
        &daemon_env,
        None,
    )
    .expect("amend commit should succeed");

    let latest_seq = daemon.latest_seq_and_wait_idle();
    assert!(
        latest_seq >= 3,
        "trace mirror should append start/cmd_name/exit events"
    );

    let rewrite_log_path = git_common_dir(&repo).join("ai").join("rewrite_log");
    let rewrite_log =
        fs::read_to_string(&rewrite_log_path).expect("rewrite log should exist after amend");
    let amend_events = rewrite_log
        .lines()
        .filter(|line| line.contains("\"commit_amend\""))
        .count();
    assert_eq!(
        amend_events, 1,
        "daemon trace mirroring in write mode should not duplicate commit_amend rewrite events"
    );

    let family_state_raw = fs::read_to_string(family_state_path(&repo))
        .expect("family state should exist after mirrored trace events");
    let family_state: Value =
        serde_json::from_str(&family_state_raw).expect("family state should be valid json");
    let saw_commit = family_state
        .get("commands")
        .and_then(Value::as_array)
        .map(|commands| {
            commands.iter().any(|command| {
                command.get("name").and_then(Value::as_str) == Some("commit")
                    && command.get("exit_code").and_then(Value::as_i64) == Some(0)
            })
        })
        .unwrap_or(false);
    assert!(
        saw_commit,
        "daemon family state should record successful mirrored commit command"
    );
}
