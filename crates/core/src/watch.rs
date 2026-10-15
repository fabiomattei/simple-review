//! Watch a repository's refs so new commits (e.g. from a Claude Code session) show up live.

use std::path::Path;
use std::sync::mpsc;
use std::time::Duration;

use anyhow::Result;
use notify::{RecursiveMode, Watcher};

use crate::tasks::Notify;

/// Changes within this window are reported once.
const DEBOUNCE: Duration = Duration::from_millis(400);

/// Keeps the watch alive; dropping it stops watching.
pub struct RepoWatcher {
    _watcher: notify::RecommendedWatcher,
    changed: crossbeam_channel::Receiver<()>,
}

impl RepoWatcher {
    /// Watch `HEAD`, `packed-refs` and `refs/` under `git_dir`.
    pub fn start(git_dir: &Path, notify: Notify) -> Result<Self> {
        let (raw_tx, raw_rx) = mpsc::channel::<()>();
        let mut watcher = notify::recommended_watcher(move |event: notify::Result<notify::Event>| {
            if let Ok(e) = event
                && !e.paths.iter().all(|p| p.extension().is_some_and(|x| x == "lock"))
            {
                let _ = raw_tx.send(());
            }
        })?;
        watcher.watch(&git_dir.join("refs"), RecursiveMode::Recursive)?;
        // HEAD and packed-refs are replaced by rename, so watch the directory non-recursively.
        watcher.watch(git_dir, RecursiveMode::NonRecursive)?;

        let (tx, changed) = crossbeam_channel::unbounded();
        std::thread::Builder::new().name("repo-watch".into()).spawn(move || {
            while raw_rx.recv().is_ok() {
                // Coalesce a burst (commit, ref update, reflog) into one notification.
                while raw_rx.recv_timeout(DEBOUNCE).is_ok() {}
                if tx.send(()).is_err() {
                    break;
                }
                notify();
            }
        })?;
        Ok(RepoWatcher { _watcher: watcher, changed })
    }

    /// True if refs changed since the last call.
    pub fn take_changed(&self) -> bool {
        let mut any = false;
        while self.changed.try_recv().is_ok() {
            any = true;
        }
        any
    }

    /// Blocking wait, for tests.
    pub fn wait(&self, timeout: Duration) -> bool {
        self.changed.recv_timeout(timeout).is_ok()
    }
}
