//! The application: owns state, runs background jobs and applies user actions.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use egui::{Key, Modifiers, RichText, Ui};
use review_core::ObjectId;
use review_core::diff::DiffOptions;
use review_core::diff::content::is_generated;
use review_core::file_view::{self, FileView, Placeholder};
use review_core::git::{CommitInfo, Repo};
use review_core::highlight::{Highlighted, Highlighter, ThemeKind};
use review_core::settings::{GLOBAL, Store};
use review_core::tasks::{CommitPager, LruCache, Notify, PagerEvent, TaskPool};

use crate::nav::{History, Location};
use crate::state::{
    FilesState, HighlightKey, HunkTarget, JobResult, RepoState, SelectedCommit, SelectedFile, ViewKey, ViewState,
};
use crate::ui::diff::{DiffInput, KeyScroll};
use crate::ui::{self, Action, human_size};

const FIRST_PAGE: usize = 300;
const NEXT_PAGE: usize = 1000;
const H_SCROLL_STEP: f32 = 60.0;

pub struct ReviewApp {
    ctx: egui::Context,
    store: Option<Store>,
    pool: TaskPool<JobResult>,
    notify: Notify,
    highlighter: Arc<Highlighter>,
    rs: Option<RepoState>,
    recent: Vec<PathBuf>,
    opts: DiffOptions,
    unified: bool,
    show_help: bool,
    error: Option<String>,
    view_cache: LruCache<ViewKey, Arc<FileView>>,
    hl_cache: LruCache<HighlightKey, Arc<Highlighted>>,
    hl_pending: HashSet<HighlightKey>,
    key_scroll: Option<KeyScroll>,
    h_delta: f32,
}

impl ReviewApp {
    pub fn new(cc: &eframe::CreationContext<'_>, initial: Option<PathBuf>) -> Self {
        Self::with_store(cc, Store::open_default(), initial)
    }

    pub fn with_store(cc: &eframe::CreationContext<'_>, store: anyhow::Result<Store>, initial: Option<PathBuf>) -> Self {
        let ctx = cc.egui_ctx.clone();
        let repaint = ctx.clone();
        let notify: Notify = Arc::new(move || repaint.request_repaint());
        let workers = std::thread::available_parallelism().map_or(2, |n| n.get().clamp(2, 6));
        let (store, error) = match store {
            Ok(s) => (Some(s), None),
            Err(e) => (None, Some(format!("Settings unavailable: {e:#}"))),
        };
        let pref = |key: &str, default: bool| store.as_ref().map_or(default, |s| s.get_bool(GLOBAL, key, default));
        let opts = DiffOptions { ignore_whitespace: pref("ignore_whitespace", false), intra_line: pref("intra_line", true) };
        let unified = pref("unified", false);
        let recent = store.as_ref().and_then(|s| s.recent_repos(12).ok()).unwrap_or_default();

        // Load syntax definitions off the UI thread so the first diff doesn't pay for it.
        std::thread::spawn(Highlighter::global);

        let mut app = ReviewApp {
            ctx,
            store,
            pool: TaskPool::new(workers, notify.clone()),
            notify,
            highlighter: Highlighter::global(),
            rs: None,
            recent,
            opts,
            unified,
            show_help: false,
            error,
            view_cache: LruCache::new(64),
            hl_cache: LruCache::new(128),
            hl_pending: HashSet::new(),
            key_scroll: None,
            h_delta: 0.0,
        };
        if let Some(path) = initial {
            app.open_repo(&path);
        }
        app
    }

    fn theme(&self) -> ThemeKind {
        if self.ctx.global_style().visuals.dark_mode { ThemeKind::Dark } else { ThemeKind::Light }
    }

    fn save_pref(&self, key: &str, value: bool) {
        if let Some(store) = &self.store {
            let _ = store.set_bool(GLOBAL, key, value);
        }
    }

    // ---- repository and selection ----

    fn open_repo(&mut self, path: &Path) {
        let repo = match Repo::open(path) {
            Ok(r) => r,
            Err(e) => {
                self.error = Some(format!("{e:#}"));
                return;
            }
        };
        let root = repo.root().to_path_buf();
        let settings = self.store.as_ref().and_then(|s| s.repo_settings(&root).ok()).unwrap_or_default();
        if let Some(store) = &self.store {
            let _ = store.touch_recent(&root);
            self.recent = store.recent_repos(12).unwrap_or_default();
        }
        let head = match repo.head_id() {
            Ok(h) => h,
            Err(e) => {
                self.error = Some(format!("{e:#}"));
                None
            }
        };
        let pager = head.map(|tip| {
            let p = CommitPager::start(repo.clone(), tip, self.notify.clone());
            p.request(FIRST_PAGE);
            p
        });
        let name = root.file_name().map_or_else(|| root.display().to_string(), |n| n.to_string_lossy().into_owned());
        self.ctx.send_viewport_cmd(egui::ViewportCommand::Title(format!("{name} — Simple Review")));
        self.rs = Some(RepoState {
            repo,
            settings,
            commits: Vec::new(),
            pager_waiting: pager.is_some(),
            pager_done: pager.is_none(),
            pager,
            ai_only: false,
            visible: Vec::new(),
            commit: None,
            history: History::default(),
            reveal_commit: false,
        });
        self.error = None;
    }

    fn select_commit(&mut self, info: CommitInfo, parent_idx: usize, path: Option<String>, push: bool) {
        let Some(rs) = self.rs.as_mut() else { return };
        let parent_idx = parent_idx.min(info.parents.len().saturating_sub(1));
        let reviewed = self
            .store
            .as_ref()
            .and_then(|s| s.reviewed_files(rs.repo.root(), &info.id.to_string()).ok())
            .unwrap_or_default();
        if push {
            rs.history.push(Location { commit: info.id, parent_idx, path: None });
        }
        let sc = SelectedCommit {
            info,
            parent_idx,
            files: FilesState::Loading,
            reviewed,
            file: None,
            pending_path: path,
            pending_target: HunkTarget::First,
            reveal_file: true,
        };
        let (commit, parent) = (sc.info.id, sc.parent());
        let repo = rs.repo.clone();
        self.pool.spawn(format!("Listing files of {}", sc.info.short_id()), move |ctx, send| {
            let result = repo.changed_files(parent, commit).map_err(|e| format!("{e:#}"));
            if !ctx.is_cancelled() {
                send(JobResult::Files { commit, parent, result });
            }
        });
        rs.commit = Some(sc);
    }

    fn select_commit_by_id(&mut self, id: ObjectId, parent_idx: usize, path: Option<String>, push: bool) {
        let Some(rs) = self.rs.as_mut() else { return };
        let info = match rs.commits.iter().find(|c| c.id == id) {
            Some(c) => c.clone(),
            None => match rs.repo.commit(id) {
                Ok(c) => c,
                Err(e) => {
                    self.error = Some(format!("{e:#}"));
                    return;
                }
            },
        };
        rs.reveal_commit = true;
        self.select_commit(info, parent_idx, path, push);
    }

    fn select_file(&mut self, index: usize, target: HunkTarget, push: bool) {
        let Some(rs) = self.rs.as_mut() else { return };
        let Some(sc) = rs.commit.as_mut() else { return };
        let Some(change) = sc.files().get(index).cloned() else { return };
        if let Some(old) = sc.file.as_ref().and_then(|f| f.token.as_ref()) {
            old.cancel();
        }
        let path = change.path().to_string();
        let mut sf = SelectedFile::new(index, change, target);
        if rs.settings.is_ignored(&path) {
            sf.view = ViewState::Collapsed("ignored path");
        } else if is_generated(&path) {
            sf.view = ViewState::Collapsed("generated file");
        } else {
            request_view(&self.pool, &rs.repo, &mut sf, self.opts, &mut self.view_cache);
        }
        if push {
            rs.history.push(Location { commit: sc.info.id, parent_idx: sc.parent_idx, path: Some(path) });
        }
        sc.reveal_file = true;
        sc.file = Some(sf);
    }

    fn reload_view(&mut self) {
        let Some(rs) = self.rs.as_mut() else { return };
        let Some(sf) = rs.commit.as_mut().and_then(|c| c.file.as_mut()) else { return };
        if matches!(sf.view, ViewState::Collapsed(_)) {
            return;
        }
        sf.target = HunkTarget::First;
        request_view(&self.pool, &rs.repo, sf, self.opts, &mut self.view_cache);
    }

    fn navigate_to(&mut self, loc: Location) {
        let Some(rs) = self.rs.as_mut() else { return };
        let same_commit = rs.commit.as_ref().is_some_and(|c| c.info.id == loc.commit && c.parent_idx == loc.parent_idx);
        if !same_commit {
            self.select_commit_by_id(loc.commit, loc.parent_idx, loc.path, false);
            return;
        }
        let sc = rs.commit.as_mut().expect("checked above");
        match loc.path {
            None => sc.file = None,
            Some(path) => match &sc.files {
                FilesState::Ready(files) => {
                    if let Some(i) = files.iter().position(|f| f.path() == path) {
                        self.select_file(i, HunkTarget::First, false);
                    }
                }
                _ => sc.pending_path = Some(path),
            },
        }
    }

    // ---- background results ----

    fn poll(&mut self) {
        if let Some(rs) = self.rs.as_mut() {
            let mut first_batch = false;
            while let Some(ev) = rs.pager.as_ref().and_then(CommitPager::try_recv) {
                match ev {
                    PagerEvent::Batch(batch) => {
                        first_batch |= rs.commits.is_empty();
                        rs.commits.extend(batch);
                        rs.pager_waiting = false;
                    }
                    PagerEvent::Done => {
                        rs.pager_done = true;
                        rs.pager_waiting = false;
                    }
                    PagerEvent::Error(e) => {
                        rs.pager_done = true;
                        self.error = Some(e);
                    }
                }
                rs.refilter();
            }
            if first_batch
                && rs.commit.is_none()
                && let Some(first) = rs.commits.first().cloned()
            {
                self.select_commit(first, 0, None, true);
            }
        }

        while let Some(result) = self.pool.try_recv() {
            match result {
                JobResult::Files { commit, parent, result } => self.on_files(commit, parent, result),
                JobResult::View { key, result } => {
                    if let Ok(view) = &result {
                        self.view_cache.insert(key.clone(), view.clone());
                    }
                    let sf = self.rs.as_mut().and_then(|rs| rs.commit.as_mut()).and_then(|c| c.file.as_mut());
                    if let Some(sf) = sf
                        && sf.key.as_ref() == Some(&key)
                    {
                        match result {
                            Ok(view) => {
                                sf.set_view(view);
                                sf.reveal_current_hunk(self.unified);
                            }
                            Err(e) => sf.view = ViewState::Error(e),
                        }
                    }
                }
                JobResult::Highlight { key, result } => {
                    self.hl_pending.remove(&key);
                    if let Some(h) = result {
                        self.hl_cache.insert(key, h);
                    }
                }
            }
        }
        self.ensure_highlighting();
    }

    fn on_files(&mut self, commit: ObjectId, parent: Option<ObjectId>, result: Result<Vec<review_core::git::FileChange>, String>) {
        let Some(sc) = self.rs.as_mut().and_then(|rs| rs.commit.as_mut()) else { return };
        if sc.info.id != commit || sc.parent() != parent {
            return;
        }
        match result {
            Ok(files) => {
                let pending = sc.pending_path.take();
                let target = sc.pending_target;
                let index = pending.as_ref().and_then(|p| files.iter().position(|f| f.path() == p));
                let empty = files.is_empty();
                sc.files = FilesState::Ready(files);
                match index {
                    Some(i) => self.select_file(i, target, false),
                    None if !empty => self.select_file(0, target, pending.is_none()),
                    None => {}
                }
            }
            Err(e) => sc.files = FilesState::Error(e),
        }
    }

    /// Queue syntax highlighting for both sides of the visible file, if not done yet.
    fn ensure_highlighting(&mut self) {
        let theme = self.theme();
        let Some(sf) = self.rs.as_ref().and_then(|rs| rs.commit.as_ref()).and_then(|c| c.file.as_ref()) else {
            return;
        };
        let ViewState::Ready(view) = &sf.view else { return };
        if !matches!(view.as_ref(), FileView::Text { .. }) {
            return;
        }
        for (old_side, side) in [(true, &sf.change.old), (false, &sf.change.new)] {
            let Some(side) = side else { continue };
            let key = (side.id, theme);
            if self.hl_pending.contains(&key) || self.hl_cache.get(&key).is_some() {
                continue;
            }
            self.hl_pending.insert(key);
            let view = view.clone();
            let path = side.path.clone();
            let highlighter = self.highlighter.clone();
            self.pool.spawn(format!("Highlighting {path}"), move |ctx, send| {
                let FileView::Text { diff, .. } = view.as_ref() else { return };
                let lines = if old_side { &diff.old.lines } else { &diff.new.lines };
                let result = highlighter.highlight(&path, lines, theme, &|| ctx.is_cancelled()).map(Arc::new);
                send(JobResult::Highlight { key, result });
            });
        }
    }

    // ---- actions ----

    fn apply(&mut self, action: Action) {
        match action {
            Action::OpenDialog => {
                if let Some(path) = rfd::FileDialog::new().set_title("Open repository").pick_folder() {
                    self.open_repo(&path);
                }
            }
            Action::OpenRepo(path) => self.open_repo(&path),
            Action::Reload => {
                let Some(rs) = self.rs.as_ref() else { return };
                let root = rs.repo.root().to_path_buf();
                let current = rs.commit.as_ref().map(|c| {
                    (c.info.id, c.parent_idx, c.file.as_ref().map(|f| f.change.path().to_string()))
                });
                self.open_repo(&root);
                if let Some((id, parent, path)) = current {
                    self.select_commit_by_id(id, parent, path, true);
                }
            }
            Action::SelectCommit(idx) => {
                if let Some(info) = self.rs.as_ref().and_then(|rs| rs.commits.get(idx).cloned()) {
                    self.select_commit(info, 0, None, true);
                }
            }
            Action::NextCommit | Action::PrevCommit => {
                let Some(rs) = self.rs.as_mut() else { return };
                let next = match (rs.selected_visible_index(), &action) {
                    (None, _) => 0,
                    (Some(i), Action::NextCommit) => i + 1,
                    (Some(i), _) => i.saturating_sub(1),
                };
                if next + 20 >= rs.visible.len() {
                    request_more(rs);
                }
                if let Some(&idx) = rs.visible.get(next) {
                    rs.reveal_commit = true;
                    let info = rs.commits[idx].clone();
                    self.select_commit(info, 0, None, true);
                }
            }
            Action::SelectFile(i) => self.select_file(i, HunkTarget::First, true),
            Action::NextFile | Action::PrevFile => {
                let target = HunkTarget::First;
                self.step_file(matches!(action, Action::NextFile), target);
            }
            Action::SetReviewed(i, reviewed) => self.set_reviewed(i, reviewed),
            Action::ToggleReviewed => {
                let Some(sc) = self.rs.as_ref().and_then(|rs| rs.commit.as_ref()) else { return };
                let Some(sf) = &sc.file else { return };
                let now = sc.reviewed.contains(sf.change.path());
                self.set_reviewed(sf.index, !now);
            }
            Action::SetParent(i) => self.set_parent(Some(i)),
            Action::CycleParent => self.set_parent(None),
            Action::NextChange => self.step_change(true),
            Action::PrevChange => self.step_change(false),
            Action::ClickRow(row) => {
                let unified = self.unified;
                if let Some(sf) = self.current_file_mut()
                    && let Some(h) = sf.layout(unified).and_then(|l| l.hunks.iter().position(|r| r.contains(&row)))
                {
                    sf.current_hunk = Some(h);
                }
            }
            Action::Back | Action::Forward => {
                let Some(rs) = self.rs.as_mut() else { return };
                let loc = if action == Action::Back { rs.history.back() } else { rs.history.forward() };
                if let Some(loc) = loc {
                    self.navigate_to(loc);
                }
            }
            Action::ToggleWhitespace => {
                self.opts.ignore_whitespace = !self.opts.ignore_whitespace;
                self.save_pref("ignore_whitespace", self.opts.ignore_whitespace);
                self.reload_view();
            }
            Action::ToggleIntraLine => {
                self.opts.intra_line = !self.opts.intra_line;
                self.save_pref("intra_line", self.opts.intra_line);
                self.reload_view();
            }
            Action::ToggleUnified => {
                self.unified = !self.unified;
                self.save_pref("unified", self.unified);
                let unified = self.unified;
                if let Some(sf) = self.current_file_mut() {
                    sf.reveal_current_hunk(unified);
                }
            }
            Action::ToggleAiOnly => {
                let Some(rs) = self.rs.as_mut() else { return };
                rs.ai_only = !rs.ai_only;
                rs.refilter();
                rs.reveal_commit = true;
                if rs.visible.len() < 50 {
                    request_more(rs);
                }
            }
            Action::ExpandFile => {
                let Some(rs) = self.rs.as_mut() else { return };
                let Some(sf) = rs.commit.as_mut().and_then(|c| c.file.as_mut()) else { return };
                let expand = match &sf.view {
                    ViewState::Collapsed(_) => true,
                    ViewState::Ready(v) => {
                        let large = matches!(v.as_ref(), FileView::Placeholder(Placeholder::TooLarge { .. }));
                        sf.force |= large;
                        large
                    }
                    _ => false,
                };
                if expand {
                    request_view(&self.pool, &rs.repo, sf, self.opts, &mut self.view_cache);
                }
            }
            Action::ToggleHelp => self.show_help = !self.show_help,
            Action::LoadMoreCommits => {
                if let Some(rs) = self.rs.as_mut() {
                    request_more(rs);
                }
            }
        }
    }

    fn current_file_mut(&mut self) -> Option<&mut SelectedFile> {
        self.rs.as_mut()?.commit.as_mut()?.file.as_mut()
    }

    fn step_file(&mut self, forward: bool, target: HunkTarget) -> bool {
        let Some(sc) = self.rs.as_ref().and_then(|rs| rs.commit.as_ref()) else { return false };
        let count = sc.files().len();
        let next = match (sc.file.as_ref().map(|f| f.index), forward) {
            (None, _) if count > 0 => 0,
            (Some(i), true) if i + 1 < count => i + 1,
            (Some(i), false) if i > 0 => i - 1,
            _ => return false,
        };
        self.select_file(next, target, true);
        true
    }

    /// Move to the next/previous change, continuing into the neighbouring file at the ends.
    fn step_change(&mut self, forward: bool) {
        let unified = self.unified;
        if let Some(sf) = self.current_file_mut()
            && matches!(sf.view, ViewState::Ready(_))
        {
            let count = sf.hunk_count();
            let next = match (sf.current_hunk, forward) {
                (None, true) if count > 0 => Some(0),
                (None, false) if count > 0 => Some(count - 1),
                (Some(h), true) if h + 1 < count => Some(h + 1),
                (Some(h), false) if h > 0 => Some(h - 1),
                _ => None,
            };
            if let Some(h) = next {
                sf.current_hunk = Some(h);
                sf.reveal_current_hunk(unified);
                return;
            }
        }
        let target = if forward { HunkTarget::First } else { HunkTarget::Last };
        self.step_file(forward, target);
    }

    fn set_reviewed(&mut self, index: usize, reviewed: bool) {
        let Some(rs) = self.rs.as_mut() else { return };
        let Some(sc) = rs.commit.as_mut() else { return };
        let Some(path) = sc.files().get(index).map(|f| f.path().to_string()) else { return };
        if let Some(store) = &self.store
            && let Err(e) = store.set_reviewed(rs.repo.root(), &sc.info.id.to_string(), &path, reviewed)
        {
            self.error = Some(format!("{e:#}"));
        }
        if reviewed {
            sc.reviewed.insert(path);
        } else {
            sc.reviewed.remove(&path);
        }
    }

    /// Switch the parent a merge commit is compared against; `None` cycles to the next one.
    fn set_parent(&mut self, parent: Option<usize>) {
        let Some(sc) = self.rs.as_ref().and_then(|rs| rs.commit.as_ref()) else { return };
        let n = sc.info.parents.len();
        if n < 2 {
            return;
        }
        let idx = parent.unwrap_or((sc.parent_idx + 1) % n);
        if idx == sc.parent_idx {
            return;
        }
        let path = sc.file.as_ref().map(|f| f.change.path().to_string());
        let info = sc.info.clone();
        self.select_commit(info, idx, path, true);
    }

    // ---- input ----

    fn keyboard(&mut self, ctx: &egui::Context) -> Vec<Action> {
        let mut a = Vec::new();
        if ctx.text_edit_focused() {
            return a;
        }
        let mut scroll = None;
        let mut h_delta = 0.0;
        ctx.input_mut(|i| {
            let none = Modifiers::NONE;
            if i.consume_key(Modifiers::COMMAND, Key::O) {
                a.push(Action::OpenDialog);
            }
            if i.consume_key(Modifiers::ALT, Key::ArrowLeft) {
                a.push(Action::Back);
            }
            if i.consume_key(Modifiers::ALT, Key::ArrowRight) {
                a.push(Action::Forward);
            }
            if i.consume_key(Modifiers::ALT, Key::ArrowDown) {
                a.push(Action::NextFile);
            }
            if i.consume_key(Modifiers::ALT, Key::ArrowUp) {
                a.push(Action::PrevFile);
            }
            if i.consume_key(Modifiers::SHIFT, Key::F7) || i.consume_key(Modifiers::SHIFT, Key::N) {
                a.push(Action::PrevChange);
            }
            if i.consume_key(none, Key::F7) || i.consume_key(none, Key::N) {
                a.push(Action::NextChange);
            }
            let simple = [
                (Key::P, Action::PrevChange),
                (Key::CloseBracket, Action::NextFile),
                (Key::OpenBracket, Action::PrevFile),
                (Key::J, Action::NextCommit),
                (Key::K, Action::PrevCommit),
                (Key::U, Action::ToggleUnified),
                (Key::W, Action::ToggleWhitespace),
                (Key::I, Action::ToggleIntraLine),
                (Key::R, Action::ToggleReviewed),
                (Key::M, Action::CycleParent),
                (Key::A, Action::ToggleAiOnly),
                (Key::Enter, Action::ExpandFile),
                (Key::F5, Action::Reload),
                (Key::F1, Action::ToggleHelp),
                (Key::Questionmark, Action::ToggleHelp),
            ];
            for (key, action) in simple {
                if i.consume_key(none, key) {
                    a.push(action);
                }
            }
            if i.consume_key(none, Key::Escape) && self.show_help {
                a.push(Action::ToggleHelp);
            }
            for (key, s) in [
                (Key::ArrowDown, KeyScroll::Lines(1.0)),
                (Key::ArrowUp, KeyScroll::Lines(-1.0)),
                (Key::PageDown, KeyScroll::Pages(1.0)),
                (Key::Space, KeyScroll::Pages(1.0)),
                (Key::PageUp, KeyScroll::Pages(-1.0)),
                (Key::Home, KeyScroll::Top),
                (Key::End, KeyScroll::Bottom),
            ] {
                if i.consume_key(none, key) {
                    scroll = Some(s);
                }
            }
            if i.consume_key(none, Key::ArrowRight) {
                h_delta += H_SCROLL_STEP;
            }
            if i.consume_key(none, Key::ArrowLeft) {
                h_delta -= H_SCROLL_STEP;
            }
        });
        self.key_scroll = scroll;
        self.h_delta = h_delta;
        a
    }

    // ---- drawing ----

    fn status_bar(&mut self, ui: &mut Ui) {
        ui.horizontal(|ui| {
            let running = self.pool.running();
            let loading_commits = self.rs.as_ref().is_some_and(|rs| rs.pager_waiting);
            if running.is_empty() && !loading_commits {
                ui.weak("Ready");
            } else {
                ui.spinner();
                let mut labels: Vec<String> = running
                    .iter()
                    .map(|t| match t.progress {
                        Some(p) => format!("{} {:.0}%", t.label, p * 100.0),
                        None => t.label.clone(),
                    })
                    .collect();
                if loading_commits {
                    labels.insert(0, "Loading commits".into());
                }
                ui.weak(labels.join(" · "));
            }
            if let Some(err) = self.error.clone() {
                ui.separator();
                if ui
                    .label(RichText::new(format!("⚠ {err}")).color(ui.visuals().error_fg_color))
                    .on_hover_text("Click to dismiss")
                    .clicked()
                {
                    self.error = None;
                }
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if let Some(rs) = &self.rs {
                    ui.weak(rs.repo.root().display().to_string());
                }
            });
        });
    }

    fn welcome(&mut self, ui: &mut Ui, actions: &mut Vec<Action>) {
        ui.vertical_centered(|ui| {
            ui.add_space(ui.available_height() / 4.0);
            ui.heading("Simple Review");
            ui.label("Review code changes commit by commit.");
            ui.add_space(12.0);
            if ui.button(RichText::new("Open repository…").size(16.0)).clicked() {
                actions.push(Action::OpenDialog);
            }
            if !self.recent.is_empty() {
                ui.add_space(16.0);
                ui.weak("Recent");
                for path in &self.recent {
                    if ui.link(path.display().to_string()).clicked() {
                        actions.push(Action::OpenRepo(path.clone()));
                    }
                }
            }
        });
    }

    fn central(&mut self, ui: &mut Ui, actions: &mut Vec<Action>) {
        let theme = self.theme();
        let unified = self.unified;
        let key_scroll = self.key_scroll.take();
        let h_delta = std::mem::take(&mut self.h_delta);
        let Some(rs) = self.rs.as_mut() else {
            self.welcome(ui, actions);
            return;
        };
        let Some(sc) = rs.commit.as_mut() else {
            ui.centered_and_justified(|ui| {
                if rs.pager_done && rs.commits.is_empty() {
                    ui.label("This repository has no commits.");
                } else {
                    ui.spinner();
                }
            });
            return;
        };
        ui::header::commit_details(ui, sc, actions);
        ui.separator();
        let reviewed = sc.file.as_ref().is_some_and(|f| sc.reviewed.contains(f.change.path()));
        let Some(sf) = sc.file.as_mut() else {
            ui.centered_and_justified(|ui| match &sc.files {
                FilesState::Loading => {
                    ui.spinner();
                }
                FilesState::Ready(f) if f.is_empty() => {
                    ui.label("No files changed against this parent.");
                }
                _ => {
                    ui.label("Select a file.");
                }
            });
            return;
        };
        ui::header::file_header(ui, sf, reviewed, unified, actions);
        ui.separator();

        let message = |ui: &mut Ui, text: String, button: Option<&str>, actions: &mut Vec<Action>| {
            ui.vertical_centered(|ui| {
                ui.add_space(ui.available_height() / 3.0);
                ui.label(text);
                if let Some(label) = button
                    && ui.button(label).clicked()
                {
                    actions.push(Action::ExpandFile);
                }
            });
        };
        match &sf.view {
            ViewState::Collapsed(reason) => {
                message(ui, format!("Diff hidden: {reason}."), Some("Show diff (Enter)"), actions);
            }
            ViewState::Loading => {
                ui.centered_and_justified(|ui| ui.spinner());
            }
            ViewState::Error(e) => {
                ui.colored_label(ui.visuals().error_fg_color, e);
            }
            ViewState::Ready(view) => match view.as_ref() {
                FileView::Placeholder(p) => {
                    let (text, button) = placeholder_text(p);
                    message(ui, text, button, actions);
                }
                FileView::Text { .. } => {
                    let Some(layout) = sf.layout(unified) else { return };
                    if layout.rows.is_empty() {
                        message(ui, "Empty file.".into(), None, actions);
                        return;
                    }
                    if layout.hunks.is_empty() {
                        let why = if self.opts.ignore_whitespace { " (whitespace only)" } else { "" };
                        ui.weak(format!("No content changes{why}."));
                    }
                    let rows = layout.rows.clone();
                    let hunks = layout.hunks.clone();
                    let hl = |side: &Option<review_core::git::FileSide>, cache: &mut LruCache<HighlightKey, Arc<Highlighted>>| {
                        side.as_ref().and_then(|s| cache.get(&(s.id, theme)))
                    };
                    let hl_old = hl(&sf.change.old, &mut self.hl_cache);
                    let hl_new = hl(&sf.change.new, &mut self.hl_cache);
                    let view = view.clone();
                    let FileView::Text { diff, .. } = view.as_ref() else { unreachable!() };
                    let input = DiffInput {
                        rows: &rows,
                        hunks: &hunks,
                        old: &diff.old.lines,
                        new: &diff.new.lines,
                        hl_old: hl_old.as_deref(),
                        hl_new: hl_new.as_deref(),
                        unified,
                        current_hunk: sf.current_hunk,
                        max_line_chars: sf.max_line_chars,
                    };
                    sf.h_scroll += h_delta;
                    ui::diff::show(ui, &input, &mut sf.scroll_to_row, &mut sf.h_scroll, key_scroll, actions);
                }
            },
        }
    }
}

fn placeholder_text(p: &Placeholder) -> (String, Option<&'static str>) {
    let sizes = |old: &Option<u64>, new: &Option<u64>| match (old, new) {
        (Some(o), Some(n)) => format!("{} → {}", human_size(*o), human_size(*n)),
        (None, Some(n)) => format!("added, {}", human_size(*n)),
        (Some(o), None) => format!("deleted, was {}", human_size(*o)),
        (None, None) => String::new(),
    };
    match p {
        Placeholder::Binary { old_size, new_size } => (format!("Binary file ({})", sizes(old_size, new_size)), None),
        Placeholder::Image { old_size, new_size } => (format!("Image ({})", sizes(old_size, new_size)), None),
        Placeholder::LfsPointer { oid, size } => (format!("Git LFS object {oid} ({})", human_size(*size)), None),
        Placeholder::TooLarge { bytes, lines } => (
            format!("Large file: {} in {lines} lines.", human_size(*bytes as u64)),
            Some("Render anyway (Enter)"),
        ),
        Placeholder::Submodule { old, new } => (
            format!(
                "Submodule {} → {}",
                old.as_deref().unwrap_or("(none)"),
                new.as_deref().unwrap_or("(removed)")
            ),
            None,
        ),
    }
}

fn request_more(rs: &mut RepoState) {
    if rs.pager_done || rs.pager_waiting {
        return;
    }
    if let Some(p) = &rs.pager {
        p.request(NEXT_PAGE);
        rs.pager_waiting = true;
    }
}

/// Start computing the view for `sf`, or take it from the cache.
fn request_view(
    pool: &TaskPool<JobResult>,
    repo: &Repo,
    sf: &mut SelectedFile,
    opts: DiffOptions,
    cache: &mut LruCache<ViewKey, Arc<FileView>>,
) {
    if let Some(token) = sf.token.take() {
        token.cancel();
    }
    let key = ViewKey::for_change(&sf.change, opts, sf.force);
    sf.key = Some(key.clone());
    if let Some(view) = cache.get(&key) {
        sf.set_view(view);
        sf.reveal_current_hunk(false);
        return;
    }
    sf.view = ViewState::Loading;
    let repo = repo.clone();
    let change = sf.change.clone();
    let force = sf.force;
    sf.token = Some(pool.spawn(format!("Diffing {}", change.path()), move |ctx, send| {
        let result = file_view::load(&repo, &change, opts, force).map(Arc::new).map_err(|e| format!("{e:#}"));
        if !ctx.is_cancelled() {
            send(JobResult::View { key, result });
        }
    }));
}

impl eframe::App for ReviewApp {
    fn ui(&mut self, ui: &mut Ui, _frame: &mut eframe::Frame) {
        self.poll();
        let ctx = ui.ctx().clone();
        let mut actions = self.keyboard(&ctx);

        let toolbar = ui::header::ToolbarState {
            repo_name: self.rs.as_ref().map(|rs| {
                rs.repo.root().file_name().map_or_else(|| rs.repo.root().display().to_string(), |n| n.to_string_lossy().into_owned())
            }),
            recent: &self.recent,
            can_back: self.rs.as_ref().is_some_and(|rs| rs.history.can_back()),
            can_forward: self.rs.as_ref().is_some_and(|rs| rs.history.can_forward()),
            opts: self.opts,
            unified: self.unified,
        };
        egui::Panel::top("toolbar").show(ui, |ui| {
            ui.add_space(3.0);
            ui::header::toolbar(ui, &toolbar, &mut actions);
            ui.add_space(1.0);
        });
        egui::Panel::bottom("status").show(ui, |ui| self.status_bar(ui));

        if let Some(rs) = self.rs.as_mut() {
            egui::Panel::left("sidebar").default_size(400.0).min_size(260.0).show(ui, |ui| {
                if let Some(sc) = rs.commit.as_mut() {
                    let settings = &rs.settings;
                    egui::Panel::bottom("files_panel")
                        .resizable(true)
                        .default_size(ui.available_height() * 0.45)
                        .min_size(120.0)
                        .show(ui, |ui| ui::files::show(ui, sc, settings, &mut actions));
                }
                ui::commits::show(ui, rs, &mut actions);
            });
        }
        egui::CentralPanel::default().show(ui, |ui| self.central(ui, &mut actions));
        ui::help::show(&ctx, &mut self.show_help);

        for action in actions {
            self.apply(action);
        }
    }
}

#[cfg(test)]
#[path = "smoke_test.rs"]
mod smoke_test;
