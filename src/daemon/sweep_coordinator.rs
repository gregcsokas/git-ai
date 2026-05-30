// src/daemon/sweep_coordinator.rs

use crate::transcripts::agent::{Agent, get_all_agents};
use crate::transcripts::db::{SessionRecord, TranscriptsDatabase};
use crate::transcripts::sweep::{DiscoveredSession, SweepStrategy};
use crate::transcripts::types::TranscriptError;
use std::path::PathBuf;
use std::sync::Arc;

/// Orchestrates periodic sweeps across all registered agents.
pub struct SweepCoordinator {
    transcripts_db: Arc<TranscriptsDatabase>,
    agent_registry: Vec<(String, Box<dyn Agent>)>,
}

impl SweepCoordinator {
    pub fn new(transcripts_db: Arc<TranscriptsDatabase>) -> Self {
        Self {
            transcripts_db,
            agent_registry: get_all_agents(),
        }
    }

    /// Run a full sweep across all agents.
    ///
    /// Returns sessions that need processing (new or behind).
    pub fn run_sweep(&self) -> Result<Vec<SessionToProcess>, TranscriptError> {
        let mut sessions_to_process = Vec::new();

        for (agent_type, agent) in &self.agent_registry {
            // Skip agents that don't support periodic sweeps
            if !matches!(agent.sweep_strategy(), SweepStrategy::Periodic(_)) {
                continue;
            }

            // Discover all sessions for this agent
            let discovered = match agent.discover_sessions() {
                Ok(sessions) => sessions,
                Err(e) => {
                    tracing::error!(
                        agent_type = %agent_type,
                        error = %e,
                        "agent discovery failed during sweep, skipping"
                    );
                    continue;
                }
            };

            for session in discovered {
                let canonical = Self::canonicalize_path(&session.transcript_path);
                let canonical_str = canonical.display().to_string();
                // Check against transcripts-db
                match self.transcripts_db.get_session(
                    &session.session_id,
                    "transcript",
                    &canonical_str,
                )? {
                    None => {
                        // New session - queue for processing (worker creates DB record)
                        sessions_to_process.push(SessionToProcess {
                            session_id: session.session_id.clone(),
                            tool: session.tool.clone(),
                            canonical_path: canonical,
                            external_session_id: session.external_session_id.clone(),
                            external_parent_session_id: session.external_parent_session_id.clone(),
                        });
                    }
                    Some(existing) => {
                        // Session exists - check if it's behind
                        if self.is_session_behind(&session, &existing)? {
                            sessions_to_process.push(SessionToProcess {
                                session_id: session.session_id.clone(),
                                tool: session.tool.clone(),
                                canonical_path: canonical,
                                external_session_id: session.external_session_id.clone(),
                                external_parent_session_id: session
                                    .external_parent_session_id
                                    .clone(),
                            });
                        }
                    }
                }
            }
        }

        Ok(sessions_to_process)
    }

    fn is_session_behind(
        &self,
        discovered: &DiscoveredSession,
        existing: &SessionRecord,
    ) -> Result<bool, TranscriptError> {
        let metadata = std::fs::metadata(&discovered.transcript_path).map_err(|e| {
            TranscriptError::Transient {
                message: format!("failed to stat file: {}", e),
                retry_after: std::time::Duration::from_secs(5),
            }
        })?;

        let file_size = metadata.len() as i64;
        let modified = Self::get_modified_timestamp(&metadata);

        Ok(file_size != existing.last_known_size
            || (modified.is_some() && modified != existing.last_modified))
    }

    fn canonicalize_path(path: &PathBuf) -> PathBuf {
        std::fs::canonicalize(path).unwrap_or_else(|_| path.clone())
    }

    fn get_modified_timestamp(metadata: &std::fs::Metadata) -> Option<i64> {
        metadata
            .modified()
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_secs() as i64)
    }
}

/// A session that needs processing.
#[derive(Debug, Clone)]
pub struct SessionToProcess {
    pub session_id: String,
    pub tool: String,
    pub canonical_path: PathBuf,
    pub external_session_id: String,
    pub external_parent_session_id: Option<String>,
}
