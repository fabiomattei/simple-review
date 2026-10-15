//! Compare two points in history: `base..tip`, `base...tip` (from the merge base), or a branch
//! against the base branch.

use anyhow::{Context, Result, bail};
use gix::ObjectId;

use crate::git::Repo;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommitRange {
    /// The old side of the comparison (the merge base for `...`).
    pub base: ObjectId,
    pub tip: ObjectId,
    /// What the user typed, normalised, e.g. `main...feature`.
    pub label: String,
}

impl CommitRange {
    /// Key for per-file review marks.
    pub fn key(&self) -> String {
        format!("{}..{}", self.base, self.tip)
    }
}

/// Parse `A..B`, `A...B`, or a single revision `B` (meaning `B` against its first parent).
pub fn resolve(repo: &Repo, spec: &str) -> Result<CommitRange> {
    let spec = spec.trim();
    if spec.is_empty() {
        bail!("enter a range such as main...feature");
    }
    if let Some((a, b)) = spec.split_once("...") {
        let (a, b) = (non_empty(a, "HEAD"), non_empty(b, "HEAD"));
        let (ia, ib) = (repo.resolve(a)?, repo.resolve(b)?);
        let base = repo.merge_base(ia, ib)?.with_context(|| format!("{a} and {b} have no common ancestor"))?;
        return Ok(CommitRange { base, tip: ib, label: format!("{a}...{b}") });
    }
    if let Some((a, b)) = spec.split_once("..") {
        let (a, b) = (non_empty(a, "HEAD"), non_empty(b, "HEAD"));
        return Ok(CommitRange { base: repo.resolve(a)?, tip: repo.resolve(b)?, label: format!("{a}..{b}") });
    }
    let tip = repo.resolve(spec)?;
    let parent = repo.commit(tip)?.parents.first().copied().with_context(|| format!("{spec} has no parent"))?;
    Ok(CommitRange { base: parent, tip, label: format!("{spec}^..{spec}") })
}

fn non_empty<'a>(s: &'a str, default: &'a str) -> &'a str {
    let s = s.trim();
    if s.is_empty() { default } else { s }
}

/// The configured base branch if it exists, else `main`, else `master`.
pub fn base_branch(repo: &Repo, configured: &str) -> Option<String> {
    [configured, "main", "master"]
        .into_iter()
        .filter(|b| !b.is_empty())
        .find(|b| repo.resolve(b).is_ok())
        .map(str::to_string)
}
