//! Startup recovery and cleanup for the daemon.
//!
//! On startup, performs:
//! - Stale socket file cleanup
//! - Stale lock file detection (PID dead → break lock)
//! - Log rotation (rotate if > 10MB, keeping up to 3 rotated files)

use std::fs;
use std::path::Path;

use super::lifecycle::{DaemonPaths, Error, is_process_alive, read_pid_file};
use super::log_rotation;

/// Run all startup recovery checks before the daemon begins its main loop.
/// This should be called early in `run_daemon()`, after paths are resolved
/// but before acquiring the lock.
pub fn run_startup_recovery(paths: &DaemonPaths) -> Result<(), Error> {
    cleanup_stale_pid(paths)?;
    cleanup_stale_sockets(paths);
    log_rotation::rotate_logs_if_needed(&paths.log_file);
    Ok(())
}

/// If a PID file exists but the process is dead, remove it and the lock file
/// so the new daemon instance can start.
///
/// Also handles:
/// - PID recycling (process alive but not our daemon → verify via control socket)
/// - Missing PID file with orphaned lock file (crash without writing PID)
fn cleanup_stale_pid(paths: &DaemonPaths) -> Result<(), Error> {
    if let Some(daemon_pid) = read_pid_file(&paths.pid_file) {
        if !is_process_alive(daemon_pid.pid) {
            eprintln!(
                "[git-ai] removing stale pid file (pid {} is dead)",
                daemon_pid.pid
            );
            let _ = fs::remove_file(&paths.pid_file);
            let _ = fs::remove_file(&paths.lock_file);
        } else if !is_control_socket_responsive(&paths.control_sock) {
            // PID is alive but not responding on our control socket — PID was recycled
            // to a different process after macOS sleep/wake killed the daemon.
            eprintln!(
                "[git-ai] removing stale pid file (pid {} alive but not our daemon)",
                daemon_pid.pid
            );
            let _ = fs::remove_file(&paths.pid_file);
            let _ = fs::remove_file(&paths.lock_file);
        } else {
            return Err(Error::AlreadyRunning(daemon_pid.pid));
        }
    } else if paths.lock_file.exists() {
        // No PID file but lock file exists — try to acquire it.
        // If we can get the flock, the old daemon is truly dead.
        match try_break_orphaned_lock(&paths.lock_file) {
            true => {
                eprintln!("[git-ai] removed orphaned lock file (no pid file)");
            }
            false => {
                // Lock is actively held by some process — cannot start.
                return Err(Error::LockHeld);
            }
        }
    }
    Ok(())
}

/// Check if the control socket is responsive (proves our daemon owns the PID).
fn is_control_socket_responsive(control_sock: &Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::net::UnixStream;
        if !control_sock.exists() {
            return false;
        }
        UnixStream::connect(control_sock).is_ok()
    }
    #[cfg(not(unix))]
    {
        let _ = control_sock;
        // On Windows, if PID is alive assume it's ours (conservative)
        true
    }
}

/// Try to acquire the lock file to determine if it's truly orphaned.
/// If flock succeeds, the old holder is dead — remove the file and return true.
/// If flock fails, someone else holds it — return false.
fn try_break_orphaned_lock(lock_file: &Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::io::AsRawFd;
        let file = match std::fs::OpenOptions::new()
            .write(true)
            .create(false)
            .open(lock_file)
        {
            Ok(f) => f,
            Err(_) => {
                // Can't open → just remove it (permissions issue or race)
                let _ = fs::remove_file(lock_file);
                return true;
            }
        };
        let fd = file.as_raw_fd();
        let ret = unsafe { libc::flock(fd, libc::LOCK_EX | libc::LOCK_NB) };
        if ret == 0 {
            // We got the lock → old holder is dead. Drop file + remove.
            drop(file);
            let _ = fs::remove_file(lock_file);
            true
        } else {
            false
        }
    }
    #[cfg(not(unix))]
    {
        // On non-unix, if the PID file is missing, assume the lock is stale.
        let _ = fs::remove_file(lock_file);
        true
    }
}

/// Remove leftover socket files from a previous unclean shutdown.
/// The trace2 listener and control socket both remove stale sockets on bind,
/// but doing it here as well handles the case where bind itself failed previously.
fn cleanup_stale_sockets(paths: &DaemonPaths) {
    remove_socket_if_stale(&paths.trace2_sock);
    remove_socket_if_stale(&paths.control_sock);
}

fn remove_socket_if_stale(path: &Path) {
    #[cfg(unix)]
    {
        use std::os::unix::net::UnixStream;
        if path.exists() {
            // Try connecting — if it fails, the socket is stale
            if UnixStream::connect(path).is_err() {
                eprintln!("[git-ai] removing stale socket: {}", path.display());
                let _ = fs::remove_file(path);
            }
        }
    }
    #[cfg(not(unix))]
    {
        let _ = path;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn removes_stale_pid_file_when_process_dead() {
        let dir = tempfile::tempdir().unwrap();
        let pid_file = dir.path().join("daemon.pid.json");

        // Write a PID file with a definitely-dead PID
        let content = r#"{"pid":999999999,"started_at":"2024-01-01T00:00:00Z","version":"0.1.0"}"#;
        fs::write(&pid_file, content).unwrap();

        let paths = DaemonPaths {
            base_dir: dir.path().to_path_buf(),
            lock_file: dir.path().join("daemon.lock"),
            pid_file: pid_file.clone(),
            log_file: dir.path().join("daemon.log"),
            trace2_sock: dir.path().join("trace2.sock"),
            control_sock: dir.path().join("control.sock"),
        };

        let result = run_startup_recovery(&paths);
        assert!(result.is_ok());
        assert!(!pid_file.exists());
    }
}
