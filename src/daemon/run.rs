use std::fs;
use std::sync::atomic::AtomicBool;
use std::sync::mpsc;
use std::sync::{Arc, OnceLock};
use std::thread;
use std::time::Duration;

use super::event_loop;
use super::lifecycle::{
    self, DaemonPaths, Error, acquire_lock, install_signal_handlers, read_pid_file,
    redirect_stderr_to_log, write_pid_file,
};
use super::trace2_events::Trace2Event;

#[cfg(unix)]
use super::lifecycle::daemonize;
#[cfg(windows)]
use super::lifecycle::daemonize_windows;

static SHUTDOWN: OnceLock<Arc<AtomicBool>> = OnceLock::new();

pub fn shutdown_flag() -> &'static Arc<AtomicBool> {
    SHUTDOWN.get_or_init(|| Arc::new(AtomicBool::new(false)))
}

pub fn run_daemon(foreground: bool) -> Result<(), Error> {
    let paths = DaemonPaths::resolve();
    paths.ensure_dirs()?;

    if !foreground {
        #[cfg(unix)]
        daemonize()?;
        #[cfg(windows)]
        daemonize_windows()?;
        #[cfg(not(any(unix, windows)))]
        return Err(Error::Generic(
            "daemonize is only supported on unix and windows".to_string(),
        ));
    }

    let _lock = acquire_lock(&paths.lock_file)?;

    write_pid_file(&paths.pid_file)?;

    let shutdown = shutdown_flag().clone();
    install_signal_handlers(shutdown.clone());

    redirect_stderr_to_log(&paths.log_file)?;

    disable_trace2_for_self();

    eprintln!("[git-ai] daemon started (pid {})", std::process::id());

    // Create channel for trace2 events
    let (event_tx, event_rx) = mpsc::channel::<Trace2Event>();

    // Start the trace2 socket listener thread
    let listener_handle = start_trace2_listener(&paths, event_tx, shutdown.clone())?;

    // Run the event loop on the main thread (blocks until shutdown)
    event_loop::run_event_loop(event_rx, shutdown.clone());

    // Wait for listener thread to finish
    if let Some(handle) = listener_handle {
        let _ = handle.join();
    }

    eprintln!("[git-ai] daemon shutting down");
    let _ = fs::remove_file(&paths.pid_file);

    Ok(())
}

/// Start the trace2 listener on a background thread.
/// Returns the thread handle (None on platforms where the listener isn't supported yet).
fn start_trace2_listener(
    paths: &DaemonPaths,
    event_tx: mpsc::Sender<Trace2Event>,
    shutdown: Arc<AtomicBool>,
) -> Result<Option<thread::JoinHandle<()>>, Error> {
    #[cfg(unix)]
    {
        use super::trace2_listener::Trace2Listener;

        let listener = Trace2Listener::bind(&paths.trace2_sock, shutdown.clone())
            .map_err(|e| Error::Generic(format!("failed to bind trace2 socket: {}", e)))?;

        eprintln!(
            "[git-ai] trace2 listener bound to {}",
            paths.trace2_sock.display()
        );

        let handle = thread::Builder::new()
            .name("trace2-listener".to_string())
            .spawn(move || {
                listener.run(event_tx);
            })
            .map_err(|e| Error::Generic(format!("failed to spawn listener thread: {}", e)))?;

        Ok(Some(handle))
    }

    #[cfg(windows)]
    {
        use super::trace2_listener_win::Trace2ListenerWin;

        let listener = Trace2ListenerWin::bind(&paths.trace2_sock, shutdown.clone())
            .map_err(|e| Error::Generic(format!("failed to bind trace2 named pipe: {}", e)))?;

        eprintln!(
            "[git-ai] trace2 listener bound to {}",
            paths.trace2_sock.display()
        );

        let handle = thread::Builder::new()
            .name("trace2-listener".to_string())
            .spawn(move || {
                listener.run(event_tx);
            })
            .map_err(|e| Error::Generic(format!("failed to spawn listener thread: {}", e)))?;

        Ok(Some(handle))
    }

    #[cfg(not(any(unix, windows)))]
    {
        let _ = (paths, event_tx, shutdown);
        eprintln!("[git-ai] trace2 listener not yet supported on this platform");
        Ok(None)
    }
}

fn disable_trace2_for_self() {
    unsafe {
        std::env::set_var("GIT_TRACE2_EVENT", "0");
    }
}

pub fn stop_daemon() -> Result<(), Error> {
    let paths = DaemonPaths::resolve();

    let daemon_pid = match read_pid_file(&paths.pid_file) {
        Some(p) => p,
        None => {
            eprintln!("[git-ai] daemon is not running");
            return Ok(());
        }
    };

    if !lifecycle::is_process_alive(daemon_pid.pid) {
        let _ = fs::remove_file(&paths.pid_file);
        eprintln!("[git-ai] daemon is not running (stale pid file removed)");
        return Ok(());
    }

    #[cfg(unix)]
    {
        unsafe {
            libc::kill(daemon_pid.pid as i32, libc::SIGTERM);
        }

        for _ in 0..50 {
            thread::sleep(Duration::from_millis(100));
            if !lifecycle::is_process_alive(daemon_pid.pid) {
                eprintln!("[git-ai] daemon stopped");
                return Ok(());
            }
        }

        eprintln!(
            "[git-ai] daemon (pid {}) did not exit within 5s",
            daemon_pid.pid
        );
    }

    #[cfg(not(unix))]
    {
        eprintln!("[git-ai] stop is only supported on unix");
    }

    Ok(())
}

pub fn print_status() {
    let paths = DaemonPaths::resolve();

    match read_pid_file(&paths.pid_file) {
        Some(info) => {
            if lifecycle::is_process_alive(info.pid) {
                eprintln!(
                    "[git-ai] daemon running (pid {}, started {}, version {})",
                    info.pid, info.started_at, info.version
                );
            } else {
                eprintln!(
                    "[git-ai] daemon not running (stale pid file, last pid {})",
                    info.pid
                );
            }
        }
        None => {
            eprintln!("[git-ai] daemon not running");
        }
    }
}
