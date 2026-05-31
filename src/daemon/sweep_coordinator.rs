use crate::authorship::authorship_log_serialization::generate_session_id;
use crate::transcripts::agent::{Agent, StreamDescriptor, get_all_agents};
use crate::transcripts::db::{SessionRecord, TranscriptsDatabase};
use crate::transcripts::sweep::{DiscoveredSession, SweepStrategy};
use crate::transcripts::types::TranscriptError;
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// Orchestrates periodic sweeps across all registered agents.
///
/// Discovers sessions via each agent's filesystem scan, then checks whether
/// any of the session's stream files (transcript, OTEL DB, etc.) have changed
/// since last processing. Only sessions with at least one stale stream are
/// returned to the worker for re-processing.
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
    /// Returns sessions that need processing (new or with stale streams).
    pub fn run_sweep(&self) -> Result<Vec<SessionToProcess>, TranscriptError> {
        let mut sessions_to_process = Vec::new();

        for (agent_type, agent) in &self.agent_registry {
            if !matches!(agent.sweep_strategy(), SweepStrategy::Periodic(_)) {
                continue;
            }

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

            let streams = agent.streams();

            for session in discovered {
                let canonical = Self::canonicalize_path(&session.transcript_path);

                if self.any_stream_stale(&session, &canonical, &streams)? {
                    sessions_to_process.push(SessionToProcess {
                        session_id: session.session_id.clone(),
                        tool: session.tool.clone(),
                        canonical_path: canonical,
                        external_session_id: session.external_session_id.clone(),
                        external_parent_session_id: session.external_parent_session_id.clone(),
                    });
                }
            }
        }

        Ok(sessions_to_process)
    }

    /// Returns true if any stream file for this session is new or has changed
    /// since it was last processed.
    fn any_stream_stale(
        &self,
        session: &DiscoveredSession,
        canonical_path: &Path,
        streams: &[StreamDescriptor],
    ) -> Result<bool, TranscriptError> {
        for stream in streams {
            let Some(path) = stream.resolve_path(canonical_path) else {
                continue;
            };
            if !path.exists() {
                continue;
            }

            let session_id = if stream.shared {
                generate_session_id(&path.display().to_string(), &session.tool)
            } else {
                session.session_id.clone()
            };

            let path_str = path.display().to_string();
            match self
                .transcripts_db
                .get_session(&session_id, stream.stream_kind, &path_str)?
            {
                None => return Ok(true),
                Some(existing) => {
                    if Self::is_file_stale(&path, &existing)? {
                        return Ok(true);
                    }
                }
            }
        }
        Ok(false)
    }

    fn is_file_stale(path: &Path, existing: &SessionRecord) -> Result<bool, TranscriptError> {
        let metadata = std::fs::metadata(path).map_err(|e| TranscriptError::Transient {
            message: format!("failed to stat {}: {}", path.display(), e),
            retry_after: std::time::Duration::from_secs(5),
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
