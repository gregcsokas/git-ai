use std::io::{BufRead, Write};
use std::net::Shutdown;
use std::os::unix::net::UnixListener;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::Duration;

use super::checkpoint_worker;
use super::protocol::{ControlRequest, ControlResponse};

pub struct ControlSocket {
    listener: UnixListener,
    shutdown: Arc<AtomicBool>,
}

impl ControlSocket {
    pub fn bind(socket_path: &Path, shutdown: Arc<AtomicBool>) -> std::io::Result<Self> {
        if socket_path.exists() {
            std::fs::remove_file(socket_path)?;
        }

        if let Some(parent) = socket_path.parent() {
            std::fs::create_dir_all(parent)?;
        }

        let listener = UnixListener::bind(socket_path)?;
        listener.set_nonblocking(true)?;

        Ok(Self { listener, shutdown })
    }

    pub fn run(&self) {
        let poll_interval = Duration::from_millis(100);

        while !self.shutdown.load(Ordering::Relaxed) {
            match self.listener.accept() {
                Ok((stream, _addr)) => {
                    let shutdown = Arc::clone(&self.shutdown);
                    thread::spawn(move || {
                        handle_connection(stream, shutdown);
                    });
                }
                Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    thread::sleep(poll_interval);
                }
                Err(e) => {
                    eprintln!("[git-ai daemon] control accept error: {}", e);
                    thread::sleep(poll_interval);
                }
            }
        }
    }
}

fn handle_connection(stream: std::os::unix::net::UnixStream, shutdown: Arc<AtomicBool>) {
    let _ = stream.set_nonblocking(false);
    if let Err(e) = stream.set_read_timeout(Some(Duration::from_secs(30))) {
        eprintln!("[git-ai daemon] control: failed to set read timeout: {}", e);
        return;
    }

    let reader = std::io::BufReader::new(&stream);
    let mut writer = std::io::BufWriter::new(&stream);

    for line_result in reader.lines() {
        if shutdown.load(Ordering::Relaxed) {
            break;
        }

        match line_result {
            Ok(line) => {
                let line = line.trim().to_string();
                if line.is_empty() {
                    continue;
                }

                let response = handle_request(&line, &shutdown);
                let is_shutdown = matches!(
                    serde_json::from_str::<ControlRequest>(&line),
                    Ok(ControlRequest::Shutdown)
                );

                let response_json = serde_json::to_string(&response)
                    .unwrap_or_else(|_| r#"{"ok":false}"#.to_string());
                let _ = writeln!(writer, "{}", response_json);
                let _ = writer.flush();

                if is_shutdown {
                    break;
                }
            }
            Err(ref e)
                if e.kind() == std::io::ErrorKind::WouldBlock
                    || e.kind() == std::io::ErrorKind::TimedOut =>
            {
                break;
            }
            Err(_) => {
                break;
            }
        }
    }

    let _ = stream.shutdown(Shutdown::Both);
}

fn handle_request(line: &str, shutdown: &Arc<AtomicBool>) -> ControlResponse {
    let request: ControlRequest = match serde_json::from_str(line) {
        Ok(r) => r,
        Err(e) => {
            return ControlResponse::err(format!("invalid request JSON: {}", e));
        }
    };

    match request {
        ControlRequest::Ping => ControlResponse::ok_pong(),
        ControlRequest::Shutdown => {
            shutdown.store(true, Ordering::Relaxed);
            ControlResponse::ok_shutdown()
        }
        ControlRequest::Checkpoint(req) => match checkpoint_worker::process_checkpoint(&req) {
            Ok(count) => ControlResponse::ok_processed(count),
            Err(e) => ControlResponse::err(e),
        },
        ControlRequest::Status(req) => match checkpoint_worker::get_status(&req) {
            Ok(status) => ControlResponse::ok_status(status),
            Err(e) => ControlResponse::err(e),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read as _;
    use std::os::unix::net::UnixStream;

    #[test]
    fn ping_pong() {
        let dir = tempfile::tempdir().unwrap();
        let socket_path = dir.path().join("control.sock");
        let shutdown = Arc::new(AtomicBool::new(false));

        let ctrl = ControlSocket::bind(&socket_path, Arc::clone(&shutdown)).expect("bind failed");

        let shutdown_clone = Arc::clone(&shutdown);
        let handle = thread::spawn(move || {
            ctrl.run();
        });

        thread::sleep(Duration::from_millis(100));

        let mut client = UnixStream::connect(&socket_path).expect("connect failed");
        client
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        writeln!(client, r#"{{"type":"ping"}}"#).unwrap();
        client.flush().unwrap();

        let mut response = String::new();
        let mut buf = [0u8; 4096];
        let n = client.read(&mut buf).unwrap();
        response.push_str(&String::from_utf8_lossy(&buf[..n]));

        let resp: serde_json::Value = serde_json::from_str(response.trim()).unwrap();
        assert_eq!(resp["ok"], true);
        assert!(resp["version"].is_string());
        assert!(resp["pid"].is_number());

        shutdown_clone.store(true, Ordering::Relaxed);
        handle.join().unwrap();
    }

    #[test]
    fn shutdown_via_control() {
        let dir = tempfile::tempdir().unwrap();
        let socket_path = dir.path().join("control.sock");
        let shutdown = Arc::new(AtomicBool::new(false));

        let ctrl = ControlSocket::bind(&socket_path, Arc::clone(&shutdown)).expect("bind failed");

        let shutdown_clone = Arc::clone(&shutdown);
        let handle = thread::spawn(move || {
            ctrl.run();
        });

        thread::sleep(Duration::from_millis(100));

        let mut client = UnixStream::connect(&socket_path).expect("connect failed");
        client
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        writeln!(client, r#"{{"type":"shutdown"}}"#).unwrap();
        client.flush().unwrap();

        // Read response
        let mut buf = [0u8; 4096];
        let _ = client.read(&mut buf);

        // The daemon should have set shutdown flag
        thread::sleep(Duration::from_millis(200));
        assert!(shutdown_clone.load(Ordering::Relaxed));

        handle.join().unwrap();
    }

    #[test]
    fn checkpoint_via_control() {
        let dir = tempfile::tempdir().unwrap();
        let socket_path = dir.path().join("control.sock");
        let shutdown = Arc::new(AtomicBool::new(false));

        let ctrl = ControlSocket::bind(&socket_path, Arc::clone(&shutdown)).expect("bind failed");

        let shutdown_clone = Arc::clone(&shutdown);
        let handle = thread::spawn(move || {
            ctrl.run();
        });

        thread::sleep(Duration::from_millis(100));

        // Create a test git repo
        let repo_dir = tempfile::tempdir().unwrap();
        let repo_path = repo_dir.path();
        let git = "git";

        std::process::Command::new(git)
            .args(["init", repo_path.to_str().unwrap()])
            .env("GIT_TRACE2_EVENT", "0")
            .output()
            .unwrap();
        std::process::Command::new(git)
            .current_dir(repo_path)
            .args(["config", "user.name", "Test"])
            .env("GIT_TRACE2_EVENT", "0")
            .output()
            .unwrap();
        std::process::Command::new(git)
            .current_dir(repo_path)
            .args(["config", "user.email", "test@example.com"])
            .env("GIT_TRACE2_EVENT", "0")
            .output()
            .unwrap();

        // Create an initial commit
        std::fs::write(repo_path.join("init.txt"), "init\n").unwrap();
        std::process::Command::new(git)
            .current_dir(repo_path)
            .args(["add", "-A"])
            .env("GIT_TRACE2_EVENT", "0")
            .output()
            .unwrap();
        std::process::Command::new(git)
            .current_dir(repo_path)
            .args(["commit", "-m", "initial"])
            .env("GIT_TRACE2_EVENT", "0")
            .output()
            .unwrap();

        // Write a file to checkpoint
        std::fs::write(repo_path.join("hello.txt"), "Hello from AI\n").unwrap();

        // Send checkpoint request
        let mut client = UnixStream::connect(&socket_path).expect("connect failed");
        client
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();

        let request = serde_json::json!({
            "type": "checkpoint",
            "repo_dir": repo_path.to_str().unwrap(),
            "kind": "ai",
            "files": [{"path": "hello.txt"}],
            "agent": {"tool": "test-agent", "id": "session-1", "model": "test-model"}
        });
        writeln!(client, "{}", request).unwrap();
        client.flush().unwrap();

        let mut buf = [0u8; 4096];
        let n = client.read(&mut buf).unwrap();
        let response: serde_json::Value =
            serde_json::from_str(&String::from_utf8_lossy(&buf[..n]).trim()).unwrap();

        assert_eq!(response["ok"], true);
        assert_eq!(response["processed"], 1);

        // Verify checkpoint was written by querying status
        drop(client);
        let mut client2 = UnixStream::connect(&socket_path).expect("connect failed");
        client2
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();

        let status_req = serde_json::json!({
            "type": "status",
            "repo_dir": repo_path.to_str().unwrap()
        });
        writeln!(client2, "{}", status_req).unwrap();
        client2.flush().unwrap();

        let n = client2.read(&mut buf).unwrap();
        let status_resp: serde_json::Value =
            serde_json::from_str(&String::from_utf8_lossy(&buf[..n]).trim()).unwrap();

        assert_eq!(status_resp["ok"], true);
        assert_eq!(status_resp["status"]["checkpoint_count"], 1);
        assert_eq!(status_resp["status"]["files"][0], "hello.txt");

        shutdown_clone.store(true, Ordering::Relaxed);
        handle.join().unwrap();
    }
}
