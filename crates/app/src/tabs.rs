//! Several repositories open at once, one per tab.
//!
//! The active tab's state lives directly in [`ReviewApp`]; inactive tabs keep theirs in a
//! [`TabState`]. Switching swaps the two, so the rest of the app only ever sees one repository.

use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::Arc;

use review_core::ObjectId;
use review_core::settings::GLOBAL;
use review_core::tasks::{CancelToken, LruCache, TaskCtx, TaskPool, TaskStatus};

use super::ReviewApp;
use crate::state::{JobResult, RepoState};

/// Background jobs, tagged with the tab that started them.
pub struct Jobs {
    pool: TaskPool<(u64, JobResult)>,
    /// The active tab; new jobs belong to it.
    pub tab: u64,
}

impl Jobs {
    pub fn new(pool: TaskPool<(u64, JobResult)>) -> Self {
        Jobs { pool, tab: 0 }
    }

    pub fn spawn(&self, label: impl Into<String>, f: impl FnOnce(&TaskCtx, &dyn Fn(JobResult)) + Send + 'static) -> CancelToken {
        let tab = self.tab;
        self.pool.spawn(label, move |ctx, send| f(ctx, &|r| send((tab, r))))
    }

    pub fn try_recv(&self) -> Option<(u64, JobResult)> {
        self.pool.try_recv()
    }

    pub fn running(&self) -> Vec<TaskStatus> {
        self.pool.running()
    }
}

macro_rules! tab_fields {
    ($($field:ident : $ty:ty = $init:expr),* $(,)?) => {
        /// Everything that belongs to one repository tab.
        pub struct TabState {
            pub id: u64,
            /// Job results that finished while the tab was in the background.
            pub deferred: Vec<JobResult>,
            $(pub $field: $ty,)*
        }

        impl TabState {
            pub fn new(id: u64) -> Self {
                TabState { id, deferred: Vec::new(), $($field: $init,)* }
            }
        }

        impl ReviewApp {
            /// Exchange the active tab's state with `t`.
            fn swap_tab(&mut self, t: &mut TabState) {
                std::mem::swap(&mut self.tab_id, &mut t.id);
                $(std::mem::swap(&mut self.$field, &mut t.$field);)*
            }
        }
    };
}

tab_fields! {
    rs: Option<RepoState> = None,
    indexes: LruCache<ObjectId, Arc<review_core::symbols::SymbolIndex>> = LruCache::new(8),
    index_jobs: HashMap<ObjectId, CancelToken> = HashMap::new(),
    blames: LruCache<crate::state::BlameKey, Arc<Result<review_core::blame::Blame, String>>> = LruCache::new(64),
    blame_pending: HashSet<crate::state::BlameKey> = HashSet::new(),
    ignore_revs: Arc<Vec<ObjectId>> = Arc::default(),
    file_filter: String = String::new(),
    peek: crate::state::Peek = Default::default(),
    comment_editor: Option<crate::state::CommentEditor> = None,
    compare: Option<crate::state::CompareDialog> = None,
    settings_dialog: Option<crate::state::SettingsDialog> = None,
    analyses: HashMap<crate::state::AnalysisKey, crate::state::AnalysisState> = HashMap::new(),
    secret_scans: HashMap<crate::state::AnalysisKey, Vec<(String, review_core::analysis::secrets::SecretFinding)>> = HashMap::new(),
    secret_pending: HashSet<crate::state::AnalysisKey> = HashSet::new(),
    coverage: Option<Arc<review_core::analysis::coverage::Coverage>> = None,
    coverage_note: Option<String> = None,
    lsp_servers: Arc<std::sync::Mutex<HashMap<ObjectId, review_core::lsp::LspClient>>> = Arc::default(),
    risks: HashMap<ObjectId, Vec<review_core::laravel::risk::Risk>> = HashMap::new(),
    risk_pending: HashSet<ObjectId> = HashSet::new(),
    history_panel: crate::state::HistoryPanel = Default::default(),
    bisect: review_core::bisect::Bisect = Default::default(),
    bisect_verdict: Option<Result<review_core::bisect::Verdict, String>> = None,
    laravel: crate::state::LaravelPanel = Default::default(),
}

impl ReviewApp {
    pub(super) fn active_tab_index(&self) -> usize {
        self.tabs.iter().position(|t| t.id == self.tab_id).unwrap_or(0)
    }

    /// Make tab `i` the active one.
    pub(super) fn switch_tab(&mut self, i: usize) {
        let current = self.active_tab_index();
        if i == current || i >= self.tabs.len() {
            return;
        }
        // Park the active state in its slot, then bring the other tab's state in.
        let mut parked = std::mem::replace(&mut self.tabs[current], TabState::new(u64::MAX));
        self.swap_tab(&mut parked);
        // `parked` now holds the active tab's state and `self` holds placeholders.
        let mut incoming = std::mem::replace(&mut self.tabs[i], TabState::new(u64::MAX));
        self.tabs[current] = parked;
        self.swap_tab(&mut incoming);
        // Keep a placeholder with the active id in the active slot.
        self.tabs[i] = TabState::new(self.tab_id);
        self.pool.tab = self.tab_id;
        for result in std::mem::take(&mut incoming.deferred) {
            self.handle_result(result);
        }
        if let Some(rs) = self.rs.as_mut() {
            rs.reveal_commit = true;
        }
        self.update_title();
        self.save_tabs();
    }

    /// Open a new, empty tab and make it active.
    pub(super) fn new_tab(&mut self) {
        let id = self.next_tab_id;
        self.next_tab_id += 1;
        self.tabs.push(TabState::new(id));
        let last = self.tabs.len() - 1;
        // A fresh tab has no state to swap in; switching parks the current one.
        self.switch_tab(last);
    }

    /// Open `path`, in a new tab unless the active one is empty. Reuses a tab already showing it.
    pub(super) fn open_repo_in_tab(&mut self, path: &Path) {
        let canonical = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
        let existing = self.tabs.iter().position(|t| {
            let root = if t.id == self.tab_id { self.rs.as_ref() } else { t.rs.as_ref() }.map(|rs| rs.repo.root().to_path_buf());
            root.is_some_and(|r| r.canonicalize().unwrap_or(r) == canonical)
        });
        if let Some(i) = existing {
            self.switch_tab(i);
            return;
        }
        if self.rs.is_some() {
            self.new_tab();
        }
        self.open_repo(path);
        self.save_tabs();
    }

    pub(super) fn close_tab(&mut self, i: usize) {
        if i >= self.tabs.len() {
            return;
        }
        if self.tabs.len() == 1 {
            // Closing the last tab leaves an empty one.
            self.shutdown_language_servers();
            let id = self.next_tab_id;
            self.next_tab_id += 1;
            let mut fresh = TabState::new(id);
            self.swap_tab(&mut fresh);
            self.tabs[0] = TabState::new(self.tab_id);
            self.pool.tab = self.tab_id;
            self.update_title();
            self.save_tabs();
            return;
        }
        if i == self.active_tab_index() {
            let next = if i + 1 < self.tabs.len() { i + 1 } else { i - 1 };
            self.switch_tab(next);
        }
        let closed = self.tabs.remove(i);
        if let Ok(mut servers) = closed.lsp_servers.lock() {
            for (_, c) in servers.drain() {
                c.shutdown();
            }
        }
        self.save_tabs();
    }

    /// Display names of all tabs, and which is active.
    pub(super) fn tab_labels(&self) -> (Vec<String>, usize) {
        let labels = self
            .tabs
            .iter()
            .map(|t| {
                let rs = if t.id == self.tab_id { self.rs.as_ref() } else { t.rs.as_ref() };
                rs.map_or_else(|| "New tab".to_string(), |rs| super::review_ui::repo_name(rs.repo.root()))
            })
            .collect();
        (labels, self.active_tab_index())
    }

    pub(super) fn update_title(&self) {
        let title = match &self.rs {
            Some(rs) => format!("{} — Simple Review", super::review_ui::repo_name(rs.repo.root())),
            None => "Simple Review".to_string(),
        };
        self.ctx.send_viewport_cmd(egui::ViewportCommand::Title(title));
    }

    /// Remember open repositories for the next start.
    pub(super) fn save_tabs(&self) {
        let Some(store) = &self.store else { return };
        let paths: Vec<String> = self
            .tabs
            .iter()
            .filter_map(|t| {
                let rs = if t.id == self.tab_id { self.rs.as_ref() } else { t.rs.as_ref() };
                rs.map(|rs| rs.repo.root().to_string_lossy().into_owned())
            })
            .collect();
        let _ = store.set(GLOBAL, "open_tabs", &paths.join("\n"));
        let _ = store.set(GLOBAL, "active_tab", &self.active_tab_index().to_string());
    }

    /// Reopen the repositories that were open last time.
    pub(super) fn restore_tabs(&mut self) {
        let Some(store) = &self.store else { return };
        let paths: Vec<String> = store
            .get(GLOBAL, "open_tabs")
            .ok()
            .flatten()
            .map(|v| v.lines().map(String::from).filter(|l| !l.is_empty()).collect())
            .unwrap_or_default();
        let active: usize = store.get(GLOBAL, "active_tab").ok().flatten().and_then(|v| v.parse().ok()).unwrap_or(0);
        for p in &paths {
            if Path::new(p).exists() {
                self.open_repo_in_tab(Path::new(p));
            }
        }
        if active < self.tabs.len() {
            self.switch_tab(active);
        }
    }
}
