use crate::daemon::domain::{AliasResolution, FamilyKey, RefChange, RepoContext};
use crate::error::GitAiError;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReflogCut {
    pub ordinal: u64,
    pub hash: Option<String>,
}

pub trait GitBackend: Send + Sync + 'static {
    fn resolve_family(&self, worktree: &Path) -> Result<FamilyKey, GitAiError>;

    fn repo_context(&self, worktree: &Path) -> Result<RepoContext, GitAiError>;

    fn ref_snapshot(&self, family: &FamilyKey) -> Result<HashMap<String, String>, GitAiError>;

    fn reflog_cut(&self, family: &FamilyKey) -> Result<ReflogCut, GitAiError>;

    fn reflog_delta(
        &self,
        family: &FamilyKey,
        start: &ReflogCut,
        end: &ReflogCut,
    ) -> Result<Vec<RefChange>, GitAiError>;

    fn resolve_alias(
        &self,
        worktree: Option<&Path>,
        argv: &[String],
    ) -> Result<AliasResolution, GitAiError>;

    fn clone_target(&self, argv: &[String], cwd_hint: Option<&Path>) -> Option<PathBuf>;

    fn init_target(&self, argv: &[String], cwd_hint: Option<&Path>) -> Option<PathBuf>;
}

