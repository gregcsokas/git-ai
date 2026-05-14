use super::attribution::{Attribution, LineAttribution};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::fs;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum CheckpointKind {
    Human,
    AiAgent,
    KnownHuman,
}

impl CheckpointKind {
    pub fn is_ai(self) -> bool {
        matches!(self, CheckpointKind::AiAgent)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentId {
    pub tool: String,
    pub id: String,
    pub model: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkingLogEntry {
    pub file: String,
    #[serde(default)]
    pub blob_sha: String,
    #[serde(default)]
    pub attributions: Vec<Attribution>,
    #[serde(default)]
    pub line_attributions: Vec<LineAttribution>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Checkpoint {
    pub kind: CheckpointKind,
    pub author: String,
    pub entries: Vec<WorkingLogEntry>,
    pub timestamp: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_id: Option<AgentId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trace_id: Option<String>,
}

impl Checkpoint {
    pub fn new(kind: CheckpointKind, author: String, entries: Vec<WorkingLogEntry>) -> Self {
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        Self {
            kind,
            author,
            entries,
            timestamp,
            agent_id: None,
            trace_id: None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionRecord {
    pub agent_id: AgentId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub human_author: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HumanRecord {
    pub author: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct InitialAttributions {
    pub files: HashMap<String, Vec<LineAttribution>>,
    #[serde(default)]
    pub sessions: HashMap<String, SessionRecord>,
    #[serde(default)]
    pub humans: HashMap<String, HumanRecord>,
}

// ---------------------------------------------------------------------------
// Path helpers
// ---------------------------------------------------------------------------

fn working_log_dir(repo_git_dir: &Path, base_commit: &str) -> PathBuf {
    repo_git_dir.join("ai").join("working_logs").join(base_commit)
}

fn checkpoints_path(repo_git_dir: &Path, base_commit: &str) -> PathBuf {
    working_log_dir(repo_git_dir, base_commit).join("checkpoints.jsonl")
}

fn blobs_dir(repo_git_dir: &Path, base_commit: &str) -> PathBuf {
    working_log_dir(repo_git_dir, base_commit).join("blobs")
}

fn initial_path(repo_git_dir: &Path, base_commit: &str) -> PathBuf {
    working_log_dir(repo_git_dir, base_commit).join("INITIAL")
}

// ---------------------------------------------------------------------------
// Storage operations
// ---------------------------------------------------------------------------

/// Read all checkpoints from the JSONL file for the given base commit.
/// Returns an empty vec if the file does not exist or cannot be read.
pub fn read_checkpoints(repo_git_dir: &Path, base_commit: &str) -> Vec<Checkpoint> {
    let path = checkpoints_path(repo_git_dir, base_commit);
    let file = match fs::File::open(&path) {
        Ok(f) => f,
        Err(_) => return Vec::new(),
    };

    let reader = BufReader::new(file);
    let mut checkpoints = Vec::new();

    for line in reader.lines() {
        let line = match line {
            Ok(l) => l,
            Err(_) => continue,
        };
        if line.trim().is_empty() {
            continue;
        }
        match serde_json::from_str::<Checkpoint>(&line) {
            Ok(cp) => checkpoints.push(cp),
            Err(_) => continue,
        }
    }

    checkpoints
}

/// Append a single checkpoint to the JSONL file.
/// Creates the directory structure if it does not exist.
pub fn append_checkpoint(repo_git_dir: &Path, base_commit: &str, checkpoint: &Checkpoint) {
    let dir = working_log_dir(repo_git_dir, base_commit);
    let _ = fs::create_dir_all(&dir);

    let path = checkpoints_path(repo_git_dir, base_commit);
    let json_line = match serde_json::to_string(checkpoint) {
        Ok(s) => s,
        Err(_) => return,
    };

    use std::io::Write;
    let mut file = match fs::OpenOptions::new().create(true).append(true).open(&path) {
        Ok(f) => f,
        Err(_) => return,
    };
    let _ = writeln!(file, "{}", json_line);
}

/// Save arbitrary content to the blobs directory, returning its SHA-256 hex digest.
pub fn save_blob(repo_git_dir: &Path, base_commit: &str, content: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(content);
    let sha = format!("{:x}", hasher.finalize());

    let dir = blobs_dir(repo_git_dir, base_commit);
    let _ = fs::create_dir_all(&dir);

    let blob_path = dir.join(&sha);
    let _ = fs::write(&blob_path, content);

    sha
}

/// Read a blob by its SHA-256 hex digest. Returns `None` if it does not exist.
pub fn read_blob(repo_git_dir: &Path, base_commit: &str, sha: &str) -> Option<String> {
    let blob_path = blobs_dir(repo_git_dir, base_commit).join(sha);
    fs::read_to_string(blob_path).ok()
}

/// Read the INITIAL attributions file for the given base commit.
pub fn read_initial_attributions(
    repo_git_dir: &Path,
    base_commit: &str,
) -> Option<InitialAttributions> {
    let path = initial_path(repo_git_dir, base_commit);
    let content = fs::read_to_string(path).ok()?;
    serde_json::from_str(&content).ok()
}

/// Write the INITIAL attributions file for the given base commit.
/// Creates the directory structure if needed.
pub fn write_initial_attributions(
    repo_git_dir: &Path,
    base_commit: &str,
    attrs: &InitialAttributions,
) {
    let dir = working_log_dir(repo_git_dir, base_commit);
    let _ = fs::create_dir_all(&dir);

    let path = initial_path(repo_git_dir, base_commit);
    let json = match serde_json::to_string_pretty(attrs) {
        Ok(s) => s,
        Err(_) => return,
    };
    let _ = fs::write(&path, json);
}

/// Delete the entire working log directory for the given base commit.
pub fn delete_working_log(repo_git_dir: &Path, base_commit: &str) {
    let dir = working_log_dir(repo_git_dir, base_commit);
    if dir.exists() {
        let _ = fs::remove_dir_all(&dir);
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    fn setup() -> (TempDir, PathBuf) {
        let tmp = TempDir::new().unwrap();
        let git_dir = tmp.path().join(".git");
        fs::create_dir_all(&git_dir).unwrap();
        (tmp, git_dir)
    }

    #[test]
    fn roundtrip_checkpoint() {
        let (_tmp, git_dir) = setup();
        let base = "abc123";

        let entry = WorkingLogEntry {
            file: "src/main.rs".into(),
            blob_sha: "deadbeef".into(),
            attributions: vec![],
            line_attributions: vec![],
        };
        let cp = Checkpoint::new(CheckpointKind::AiAgent, "claude".into(), vec![entry]);

        append_checkpoint(&git_dir, base, &cp);
        append_checkpoint(&git_dir, base, &cp);

        let loaded = read_checkpoints(&git_dir, base);
        assert_eq!(loaded.len(), 2);
        assert_eq!(loaded[0].author, "claude");
        assert_eq!(loaded[0].entries[0].file, "src/main.rs");
    }

    #[test]
    fn blob_save_and_read() {
        let (_tmp, git_dir) = setup();
        let base = "def456";

        let content = b"fn main() { println!(\"hello\"); }";
        let sha = save_blob(&git_dir, base, content);

        assert_eq!(sha.len(), 64); // SHA-256 hex is 64 chars
        let loaded = read_blob(&git_dir, base, &sha).unwrap();
        assert_eq!(loaded.as_bytes(), content);
    }

    #[test]
    fn initial_attributions_roundtrip() {
        let (_tmp, git_dir) = setup();
        let base = "789abc";

        let mut files = HashMap::new();
        files.insert(
            "lib.rs".into(),
            vec![LineAttribution {
                start_line: 1,
                end_line: 5,
                author_id: "h_alice".into(),
                overrode: None,
            }],
        );

        let attrs = InitialAttributions {
            files,
            sessions: HashMap::new(),
            humans: HashMap::new(),
        };

        write_initial_attributions(&git_dir, base, &attrs);
        let loaded = read_initial_attributions(&git_dir, base).unwrap();
        assert_eq!(loaded.files.len(), 1);
        assert_eq!(loaded.files["lib.rs"][0].end_line, 5);
    }

    #[test]
    fn delete_removes_directory() {
        let (_tmp, git_dir) = setup();
        let base = "to_delete";

        let cp = Checkpoint::new(CheckpointKind::Human, "user".into(), vec![]);
        append_checkpoint(&git_dir, base, &cp);

        let dir = working_log_dir(&git_dir, base);
        assert!(dir.exists());

        delete_working_log(&git_dir, base);
        assert!(!dir.exists());
    }

    #[test]
    fn read_checkpoints_empty_when_missing() {
        let (_tmp, git_dir) = setup();
        let loaded = read_checkpoints(&git_dir, "nonexistent");
        assert!(loaded.is_empty());
    }

    #[test]
    fn read_blob_returns_none_when_missing() {
        let (_tmp, git_dir) = setup();
        assert!(read_blob(&git_dir, "x", "nosuchsha").is_none());
    }
}
