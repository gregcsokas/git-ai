pub mod commit_detector;
pub mod event_loop;
pub mod lifecycle;
pub mod post_commit_worker;
pub mod run;
pub mod trace2_events;

#[cfg(unix)]
pub mod trace2_listener;

#[cfg(windows)]
pub mod trace2_listener_win;

pub use lifecycle::{DaemonPaths, DaemonPid};
pub use run::run_daemon;
