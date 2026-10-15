//! Full copies of a commit's files in the app cache, for tools that need a directory tree.
//!
//! Files are written from git objects; the user's repository is never modified. Dependencies that
//! are not in git (`vendor/`, `node_modules/`, `.env`) are linked from the working copy.

use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use gix::ObjectId;

use crate::git::{EntryType, Repo};

/// Linked from the working copy when the commit's tree doesn't contain them.
const LINKED: &[&str] = &["vendor", "node_modules", ".env"];
const COMPLETE_MARKER: &str = ".simple-review-snapshot";
/// Written into a snapshot's `vendor/` built by [`link_vendor`].
const LINK_MARKER: &str = ".simple-review-linked";

#[derive(Debug, Clone)]
pub struct Snapshot {
    pub dir: PathBuf,
    pub commit: ObjectId,
    /// `vendor/` was linked from the working copy.
    pub vendor_linked: bool,
    /// The commit's `composer.lock` differs from the working copy's, so the linked `vendor/` may
    /// not match what the commit expects.
    pub vendor_mismatch: bool,
}

/// `<cache>/snapshots/<repo key>`; `cache` is normally the app's cache directory.
pub fn repo_dir(cache: &Path, repo: &Repo) -> PathBuf {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    repo.root().hash(&mut h);
    cache.join("snapshots").join(format!("{:016x}", h.finish()))
}

/// The app's cache directory, or `$SIMPLE_REVIEW_CACHE` when set (used by tests).
pub fn default_cache_dir() -> Result<PathBuf> {
    if let Some(dir) = std::env::var_os("SIMPLE_REVIEW_CACHE") {
        return Ok(PathBuf::from(dir));
    }
    let dirs = directories::ProjectDirs::from("dev", "simple-review", "Simple Review")
        .context("cannot determine the cache directory")?;
    Ok(dirs.cache_dir().to_path_buf())
}

/// Create (or reuse) the snapshot of `commit`.
pub fn ensure(repo: &Repo, commit: ObjectId, cache: &Path, cancelled: &dyn Fn() -> bool) -> Result<Snapshot> {
    let base = repo_dir(cache, repo);
    let dir = base.join(commit.to_string());
    if !dir.join(COMPLETE_MARKER).exists() {
        std::fs::create_dir_all(&base)?;
        let partial = base.join(format!(
            "{commit}.partial-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&partial);
        write_tree(repo, commit, &partial, cancelled)?;
        if std::fs::rename(&partial, &dir).is_err() {
            // Another job finished the same snapshot first.
            let _ = std::fs::remove_dir_all(&partial);
            if !dir.join(COMPLETE_MARKER).exists() {
                bail!("could not create snapshot at {}", dir.display());
            }
        }
    }
    // Touch for least-recently-used pruning.
    let _ = std::fs::write(dir.join(COMPLETE_MARKER), commit.to_string());

    let vendor_linked = dir.join("vendor").join(LINK_MARKER).exists()
        || std::fs::symlink_metadata(dir.join("vendor")).is_ok_and(|m| m.file_type().is_symlink());
    let vendor_mismatch = vendor_linked && {
        let in_commit = repo.find_file(commit, "composer.lock").ok().flatten().and_then(|f| repo.read_blob(f.id).ok());
        let in_worktree = std::fs::read(repo.root().join("composer.lock")).ok();
        in_commit != in_worktree
    };
    Ok(Snapshot { dir, commit, vendor_linked, vendor_mismatch })
}

fn write_tree(repo: &Repo, commit: ObjectId, dir: &Path, cancelled: &dyn Fn() -> bool) -> Result<()> {
    std::fs::create_dir_all(dir)?;
    let files = repo.list_files(commit)?;
    for f in &files {
        if cancelled() {
            let _ = std::fs::remove_dir_all(dir);
            bail!("cancelled");
        }
        let target = dir.join(&f.path);
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent)?;
        }
        match f.entry {
            EntryType::Submodule => std::fs::create_dir_all(&target)?,
            EntryType::Symlink => {
                let link = String::from_utf8_lossy(&repo.read_blob(f.id)?).into_owned();
                #[cfg(unix)]
                std::os::unix::fs::symlink(&link, &target)?;
                #[cfg(not(unix))]
                std::fs::write(&target, link)?;
            }
            EntryType::File | EntryType::Executable => {
                std::fs::write(&target, repo.read_blob(f.id)?)?;
                #[cfg(unix)]
                if f.entry == EntryType::Executable {
                    use std::os::unix::fs::PermissionsExt;
                    std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o755))?;
                }
            }
        }
    }
    for name in LINKED {
        let source = repo.root().join(name);
        let target = dir.join(name);
        if *name == "vendor" && source.is_dir() && std::fs::symlink_metadata(&target).is_err() {
            link_vendor(&source, &target)?;
            continue;
        }
        if source.exists() && std::fs::symlink_metadata(&target).is_err() {
            #[cfg(unix)]
            let _ = std::os::unix::fs::symlink(&source, &target);
            #[cfg(windows)]
            let _ = if source.is_dir() {
                std::os::windows::fs::symlink_dir(&source, &target)
            } else {
                std::os::windows::fs::symlink_file(&source, &target)
            };
        }
    }
    std::fs::write(dir.join(COMPLETE_MARKER), commit.to_string())?;
    Ok(())
}

/// Mirror the working copy's `vendor/` into the snapshot with hard links.
///
/// Tools must see the snapshot as the project root: Composer's autoloader and PHPStan locate
/// files from their own `__DIR__`, and PHP resolves symlinks there, so a symlinked `vendor/`
/// (or symlinked packages) would load the working copy's classes, or Composer twice. Hard links
/// give every file a path inside the snapshot without using extra space; across file systems
/// files are copied instead.
fn link_vendor(source: &Path, target: &Path) -> Result<()> {
    mirror(source, target)?;
    std::fs::write(target.join(LINK_MARKER), "")?;
    Ok(())
}

fn mirror(from: &Path, to: &Path) -> Result<()> {
    std::fs::create_dir_all(to)?;
    for entry in std::fs::read_dir(from)? {
        let entry = entry?;
        let (src, dst) = (entry.path(), to.join(entry.file_name()));
        let meta = std::fs::symlink_metadata(&src)?;
        if meta.file_type().is_symlink() {
            // Keep the link as written (usually relative, so it resolves inside the mirror).
            #[cfg(unix)]
            std::os::unix::fs::symlink(std::fs::read_link(&src)?, &dst)?;
            #[cfg(not(unix))]
            if std::fs::metadata(&src).is_ok_and(|m| m.is_dir()) {
                mirror(&src, &dst)?;
            } else {
                std::fs::copy(&src, &dst)?;
            }
        } else if meta.is_dir() {
            mirror(&src, &dst)?;
        } else if std::fs::hard_link(&src, &dst).is_err() {
            std::fs::copy(&src, &dst)?;
        }
    }
    Ok(())
}

/// Delete all but the `keep` most recently used snapshots of a repository.
pub fn prune(cache: &Path, repo: &Repo, keep: usize) -> Result<usize> {
    let base = repo_dir(cache, repo);
    let Ok(entries) = std::fs::read_dir(&base) else { return Ok(0) };
    let mut snapshots: Vec<(std::time::SystemTime, PathBuf)> = entries
        .flatten()
        .filter(|e| e.path().is_dir())
        .map(|e| {
            let used = std::fs::metadata(e.path().join(COMPLETE_MARKER))
                .and_then(|m| m.modified())
                .unwrap_or(std::time::UNIX_EPOCH);
            (used, e.path())
        })
        .collect();
    snapshots.sort_by_key(|s| std::cmp::Reverse(s.0));
    let mut removed = 0;
    for (_, path) in snapshots.into_iter().skip(keep) {
        if std::fs::remove_dir_all(&path).is_ok() {
            removed += 1;
        }
    }
    Ok(removed)
}
