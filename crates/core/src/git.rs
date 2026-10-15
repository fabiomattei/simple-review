//! Read-only access to a git repository: commits, changed files and blobs.
//!
//! Everything is read straight from the object database; the user's working copy is never touched.

use std::ops::ControlFlow;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow};
use gix::ObjectId;
use gix::bstr::ByteSlice;
use gix::objs::tree::EntryKind;
use gix::object::tree::diff::ChangeDetached;
use gix::revision::walk::Sorting;
use gix::traverse::commit::simple::CommitTimeOrder;

/// A handle to a repository that can be shared between threads.
#[derive(Clone)]
pub struct Repo {
    inner: gix::ThreadSafeRepository,
    root: PathBuf,
}

/// A person and a moment, as recorded in a commit header.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Signature {
    pub name: String,
    pub email: String,
    /// Seconds since the unix epoch.
    pub time: i64,
    /// Offset from UTC in seconds.
    pub offset: i32,
}

#[derive(Debug, Clone)]
pub struct CommitInfo {
    pub id: ObjectId,
    pub parents: Vec<ObjectId>,
    pub summary: String,
    pub message: String,
    pub author: Signature,
    pub committer: Signature,
    /// Names/emails from `Co-Authored-By:` trailers.
    pub co_authors: Vec<String>,
    /// True if a co-author trailer names Claude.
    pub ai_assisted: bool,
}

impl CommitInfo {
    pub fn short_id(&self) -> String {
        self.id.to_hex_with_len(8).to_string()
    }

    pub fn is_merge(&self) -> bool {
        self.parents.len() > 1
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChangeStatus {
    Added,
    Modified,
    Deleted,
    Renamed,
    Copied,
}

impl ChangeStatus {
    pub fn letter(self) -> char {
        match self {
            ChangeStatus::Added => 'A',
            ChangeStatus::Modified => 'M',
            ChangeStatus::Deleted => 'D',
            ChangeStatus::Renamed => 'R',
            ChangeStatus::Copied => 'C',
        }
    }
}

/// What kind of tree entry one side of a change is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntryType {
    File,
    Executable,
    Symlink,
    Submodule,
}

impl From<EntryKind> for EntryType {
    fn from(kind: EntryKind) -> Self {
        match kind {
            EntryKind::BlobExecutable => EntryType::Executable,
            EntryKind::Link => EntryType::Symlink,
            EntryKind::Commit => EntryType::Submodule,
            EntryKind::Blob | EntryKind::Tree => EntryType::File,
        }
    }
}

/// One side of a file change. Absent for the old side of additions and the new side of deletions.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileSide {
    pub path: String,
    pub id: ObjectId,
    pub entry: EntryType,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileChange {
    pub status: ChangeStatus,
    pub old: Option<FileSide>,
    pub new: Option<FileSide>,
}

impl FileChange {
    /// The path to show for this change: the new path, or the old one for deletions.
    pub fn path(&self) -> &str {
        self.new
            .as_ref()
            .or(self.old.as_ref())
            .map(|s| s.path.as_str())
            .unwrap_or_default()
    }

    pub fn is_submodule(&self) -> bool {
        [&self.old, &self.new]
            .into_iter()
            .flatten()
            .any(|s| s.entry == EntryType::Submodule)
    }
}

impl Repo {
    /// Open the repository containing `path`.
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        let repo = gix::discover(path).with_context(|| format!("no git repository at {}", path.display()))?;
        let root = repo
            .workdir()
            .map(Path::to_path_buf)
            .unwrap_or_else(|| repo.git_dir().to_path_buf());
        Ok(Repo {
            inner: repo.into_sync(),
            root,
        })
    }

    /// The working directory (or the git dir for bare repositories).
    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn git_dir(&self) -> PathBuf {
        self.inner.git_dir().to_path_buf()
    }

    fn local(&self) -> gix::Repository {
        let mut repo = self.inner.to_thread_local();
        repo.object_cache_size_if_unset(16 * 1024 * 1024);
        repo
    }

    pub fn head_id(&self) -> Result<Option<ObjectId>> {
        let repo = self.local();
        let head = repo.head().context("reading HEAD")?;
        Ok(head.id().map(|id| id.detach()))
    }

    /// Resolve a revision spec such as `HEAD~2`, a branch name or a hash.
    pub fn resolve(&self, spec: &str) -> Result<ObjectId> {
        let repo = self.local();
        let id = repo
            .rev_parse_single(spec)
            .with_context(|| format!("cannot resolve '{spec}'"))?;
        Ok(id.object()?.peel_to_commit()?.id)
    }

    /// Walk history from `tip`, newest first, calling `f` for each commit until it returns `Break`.
    pub fn walk_commits(
        &self,
        tip: ObjectId,
        mut f: impl FnMut(CommitInfo) -> ControlFlow<()>,
    ) -> Result<()> {
        let repo = self.local();
        let walk = repo
            .rev_walk([tip])
            .sorting(Sorting::ByCommitTime(CommitTimeOrder::NewestFirst))
            .all()?;
        for info in walk {
            let info = info?;
            let commit = repo.find_commit(info.id)?;
            if f(commit_info(&commit)?).is_break() {
                break;
            }
        }
        Ok(())
    }

    pub fn commit(&self, id: ObjectId) -> Result<CommitInfo> {
        let repo = self.local();
        commit_info(&repo.find_commit(id)?)
    }

    /// Files changed between `parent` (or the empty tree) and `commit`, with renames detected.
    pub fn changed_files(&self, parent: Option<ObjectId>, commit: ObjectId) -> Result<Vec<FileChange>> {
        let repo = self.local();
        let new_tree = repo.find_commit(commit)?.tree()?;
        let old_tree = match parent {
            Some(p) => repo.find_commit(p)?.tree()?,
            None => repo.empty_tree(),
        };
        let options = gix::diff::Options::default().with_rewrites(Some(gix::diff::Rewrites::default()));
        let changes = repo.diff_tree_to_tree(&old_tree, &new_tree, options)?;
        let mut out: Vec<FileChange> = changes.into_iter().filter_map(file_change).collect();
        out.sort_by(|a, b| a.path().cmp(b.path()));
        Ok(out)
    }

    pub fn read_blob(&self, id: ObjectId) -> Result<Vec<u8>> {
        let repo = self.local();
        let mut blob = repo.find_blob(id)?;
        Ok(blob.take_data())
    }

    /// Size of an object without decompressing it.
    pub fn object_size(&self, id: ObjectId) -> Result<u64> {
        let repo = self.local();
        Ok(repo.find_header(id)?.size())
    }
}

fn signature(sig: gix::actor::SignatureRef<'_>) -> Signature {
    let sig = sig.trim();
    let time = sig.time().unwrap_or_default();
    Signature {
        name: sig.name.to_str_lossy().into_owned(),
        email: sig.email.to_str_lossy().into_owned(),
        time: time.seconds,
        offset: time.offset,
    }
}

fn commit_info(commit: &gix::Commit<'_>) -> Result<CommitInfo> {
    let decoded = commit.decode()?;
    let message = decoded.message.to_str_lossy().into_owned();
    let co_authors = co_authors(&message);
    let ai_assisted = co_authors.iter().any(|c| is_ai_identity(c));
    Ok(CommitInfo {
        id: commit.id,
        parents: decoded.parents().collect(),
        summary: decoded.message_summary().to_str_lossy().into_owned(),
        message,
        author: signature(decoded.author()?),
        committer: signature(decoded.committer()?),
        co_authors,
        ai_assisted,
    })
}

/// Values of all `Co-Authored-By:` trailers (case-insensitive), in order.
pub fn co_authors(message: &str) -> Vec<String> {
    message
        .lines()
        .filter_map(|line| {
            let (key, value) = line.split_once(':')?;
            key.trim()
                .eq_ignore_ascii_case("co-authored-by")
                .then(|| value.trim().to_string())
        })
        .filter(|v| !v.is_empty())
        .collect()
}

fn is_ai_identity(co_author: &str) -> bool {
    let lower = co_author.to_ascii_lowercase();
    lower.contains("claude") || lower.contains("noreply@anthropic.com")
}

fn file_change(change: ChangeDetached) -> Option<FileChange> {
    let side = |path: gix::bstr::BString, id: ObjectId, mode: gix::objs::tree::EntryMode| FileSide {
        path: path.to_str_lossy().into_owned(),
        id,
        entry: mode.kind().into(),
    };
    Some(match change {
        ChangeDetached::Addition { location, entry_mode, id, .. } => {
            if entry_mode.is_tree() {
                return None;
            }
            FileChange { status: ChangeStatus::Added, old: None, new: Some(side(location, id, entry_mode)) }
        }
        ChangeDetached::Deletion { location, entry_mode, id, .. } => {
            if entry_mode.is_tree() {
                return None;
            }
            FileChange { status: ChangeStatus::Deleted, old: Some(side(location, id, entry_mode)), new: None }
        }
        ChangeDetached::Modification { location, previous_entry_mode, previous_id, entry_mode, id } => {
            if entry_mode.is_tree() || previous_entry_mode.is_tree() {
                return None;
            }
            FileChange {
                status: ChangeStatus::Modified,
                old: Some(side(location.clone(), previous_id, previous_entry_mode)),
                new: Some(side(location, id, entry_mode)),
            }
        }
        ChangeDetached::Rewrite {
            source_location, source_entry_mode, source_id, entry_mode, id, location, copy, ..
        } => {
            if entry_mode.is_tree() {
                return None;
            }
            FileChange {
                status: if copy { ChangeStatus::Copied } else { ChangeStatus::Renamed },
                old: Some(side(source_location, source_id, source_entry_mode)),
                new: Some(side(location, id, entry_mode)),
            }
        }
    })
}

/// Format a signature's time as `YYYY-MM-DD HH:MM` in its own timezone.
pub fn format_time(sig: &Signature) -> String {
    let local = sig.time + i64::from(sig.offset);
    let days = local.div_euclid(86_400);
    let secs = local.rem_euclid(86_400);
    let (y, m, d) = civil_from_days(days);
    format!("{y:04}-{m:02}-{d:02} {:02}:{:02}", secs / 3600, (secs % 3600) / 60)
}

/// Howard Hinnant's days-to-civil algorithm.
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let y = yoe + era * 400 + i64::from(m <= 2);
    (y, m, d)
}

impl std::fmt::Debug for Repo {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Repo").field("root", &self.root).finish()
    }
}

/// Parse a hex object id.
pub fn parse_id(hex: &str) -> Result<ObjectId> {
    ObjectId::from_hex(hex.as_bytes()).map_err(|e| anyhow!("invalid object id '{hex}': {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_co_author_trailers() {
        let msg = "Fix it\n\nBody\n\nCo-Authored-By: Claude Opus <noreply@anthropic.com>\nco-authored-by: Ann <a@x>\n";
        let co = co_authors(msg);
        assert_eq!(co, vec!["Claude Opus <noreply@anthropic.com>", "Ann <a@x>"]);
        assert!(is_ai_identity(&co[0]));
        assert!(!is_ai_identity(&co[1]));
    }

    #[test]
    fn formats_dates() {
        let sig = Signature { name: String::new(), email: String::new(), time: 1_700_000_000, offset: 3600 };
        assert_eq!(format_time(&sig), "2023-11-14 23:13");
    }
}
