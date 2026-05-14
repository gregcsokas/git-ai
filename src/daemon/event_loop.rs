//! Event processing loop for the daemon.
//!
//! Reads trace2 events from a channel, feeds them to CommitDetector,
//! and processes any detected commits via `post_commit_worker`.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Receiver;
use std::time::{Duration, Instant};

use super::commit_detector::CommitDetector;
use super::post_commit_worker;
use super::trace2_events::Trace2Event;

/// Run the event processing loop.
///
/// Reads events from the channel, feeds them to `CommitDetector`,
/// and processes any detected commits. Loops until `shutdown` is set.
///
/// This function blocks the calling thread.
pub fn run_event_loop(event_rx: Receiver<Trace2Event>, shutdown: Arc<AtomicBool>) {
    let mut detector = CommitDetector::new();
    let mut last_prune = Instant::now();
    let prune_interval = Duration::from_secs(60);
    let stale_threshold = Duration::from_secs(120);
    let recv_timeout = Duration::from_millis(100);

    eprintln!("[git-ai daemon] event loop started");

    loop {
        // Check shutdown flag
        if shutdown.load(Ordering::Relaxed) {
            eprintln!("[git-ai daemon] event loop shutting down");
            break;
        }

        // Try to receive an event with timeout
        match event_rx.recv_timeout(recv_timeout) {
            Ok(event) => {
                // Feed event to detector
                if let Some(detected) = detector.process_event(event) {
                    eprintln!(
                        "[git-ai daemon] commit detected in {}",
                        detected.repo_path.display()
                    );

                    // Process the detected commit
                    match post_commit_worker::process_commit(&detected.repo_path) {
                        Ok(true) => {
                            eprintln!(
                                "[git-ai daemon] successfully processed commit in {}",
                                detected.repo_path.display()
                            );
                        }
                        Ok(false) => {
                            eprintln!(
                                "[git-ai daemon] skipped commit in {} (already noted or no data)",
                                detected.repo_path.display()
                            );
                        }
                        Err(e) => {
                            eprintln!(
                                "[git-ai daemon] error processing commit in {}: {}",
                                detected.repo_path.display(),
                                e
                            );
                        }
                    }
                }
            }
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                // No event received; continue loop
            }
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                eprintln!("[git-ai daemon] event channel disconnected, shutting down");
                break;
            }
        }

        // Periodic pruning of stale entries in the detector
        if last_prune.elapsed() >= prune_interval {
            detector.prune_stale(stale_threshold);
            last_prune = Instant::now();
        }
    }

    eprintln!("[git-ai daemon] event loop exited");
}
