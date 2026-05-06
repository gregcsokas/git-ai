use crate::config;
#[cfg(windows)]
use crate::utils::CREATE_NO_WINDOW;
#[cfg(windows)]
use crate::utils::is_interactive_terminal;
#[cfg(unix)]
use std::os::unix::process::CommandExt;
#[cfg(unix)]
use std::os::unix::process::ExitStatusExt;
#[cfg(windows)]
use std::os::windows::process::CommandExt;
use std::process::Command;
#[cfg(unix)]
use std::sync::atomic::{AtomicI32, Ordering};

#[cfg(unix)]
static CHILD_PGID: AtomicI32 = AtomicI32::new(0);

#[cfg(unix)]
extern "C" fn forward_signal_handler(sig: libc::c_int) {
    let pgid = CHILD_PGID.load(Ordering::Relaxed);
    if pgid > 0 {
        unsafe {
            let _ = libc::kill(-pgid, sig);
        }
    }
}

#[cfg(unix)]
fn install_forwarding_handlers() {
    unsafe {
        let handler = forward_signal_handler as *const () as usize;
        let _ = libc::signal(libc::SIGTERM, handler);
        let _ = libc::signal(libc::SIGINT, handler);
        let _ = libc::signal(libc::SIGHUP, handler);
        let _ = libc::signal(libc::SIGQUIT, handler);
    }
}

#[cfg(unix)]
fn uninstall_forwarding_handlers() {
    unsafe {
        let _ = libc::signal(libc::SIGTERM, libc::SIG_DFL);
        let _ = libc::signal(libc::SIGINT, libc::SIG_DFL);
        let _ = libc::signal(libc::SIGHUP, libc::SIG_DFL);
        let _ = libc::signal(libc::SIGQUIT, libc::SIG_DFL);
    }
}

pub fn handle_git(args: &[String]) {
    let exit_status = proxy_to_git(args);
    exit_with_status(exit_status);
}

fn proxy_to_git(args: &[String]) -> std::process::ExitStatus {
    #[cfg(unix)]
    {
        let is_interactive = unsafe { libc::isatty(libc::STDIN_FILENO) == 1 };
        let should_setpgid = !is_interactive;

        let mut cmd = Command::new(config::Config::get().git_cmd());
        cmd.args(args);
        unsafe {
            let setpgid_flag = should_setpgid;
            cmd.pre_exec(move || {
                if setpgid_flag {
                    let _ = libc::setpgid(0, 0);
                }
                Ok(())
            });
        }
        match cmd.spawn() {
            Ok(mut child) => {
                if should_setpgid {
                    let pgid: i32 = child.id() as i32;
                    CHILD_PGID.store(pgid, Ordering::Relaxed);
                    install_forwarding_handlers();
                }
                match child.wait() {
                    Ok(status) => {
                        if should_setpgid {
                            CHILD_PGID.store(0, Ordering::Relaxed);
                            uninstall_forwarding_handlers();
                        }
                        status
                    }
                    Err(e) => {
                        if should_setpgid {
                            CHILD_PGID.store(0, Ordering::Relaxed);
                            uninstall_forwarding_handlers();
                        }
                        eprintln!("Failed to wait for git process: {}", e);
                        std::process::exit(1);
                    }
                }
            }
            Err(e) => {
                eprintln!("Failed to execute git command: {}", e);
                std::process::exit(1);
            }
        }
    }

    #[cfg(not(unix))]
    {
        let mut cmd = Command::new(config::Config::get().git_cmd());
        cmd.args(args);

        #[cfg(windows)]
        {
            if !is_interactive_terminal() {
                cmd.creation_flags(CREATE_NO_WINDOW);
            }
        }

        match cmd.spawn() {
            Ok(mut child) => match child.wait() {
                Ok(status) => status,
                Err(e) => {
                    eprintln!("Failed to wait for git process: {}", e);
                    std::process::exit(1);
                }
            },
            Err(e) => {
                eprintln!("Failed to execute git command: {}", e);
                std::process::exit(1);
            }
        }
    }
}

fn exit_with_status(status: std::process::ExitStatus) -> ! {
    #[cfg(unix)]
    {
        if let Some(sig) = status.signal() {
            unsafe {
                libc::signal(sig, libc::SIG_DFL);
                libc::raise(sig);
            }
            unreachable!();
        }
    }
    std::process::exit(status.code().unwrap_or(1));
}
