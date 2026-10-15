//! Comments, range review, export, live refresh and settings: the Phase 3 parts of the app.

use std::path::Path;
use std::sync::Arc;
use std::time::Instant;

use review_core::file_view::FileView;
use review_core::git::parse_id;
use review_core::range::{self, CommitRange};
use review_core::review::export::{self, ExportContext, FeedbackItem, Report, ReportFile};
use review_core::review::{Anchor, NewThread, Placement, ThreadFilter, ThreadSide, ThreadStatus, relocate};
use review_core::settings::RepoSettings;
use review_core::tasks::CommitPager;
use review_core::watch::RepoWatcher;

use super::{FIRST_PAGE, ReviewApp};
use crate::state::{
    CommentEditor, CommentScope, CompareDialog, EditorTarget, ExportDone, JobResult, PeekTab, PlacedThread,
    SettingsDialog, Side, ViewState,
};
use crate::ui::comments::{CommentsView, ThreadRow};
use crate::ui::diff::ThreadMark;

fn thread_side(side: Side) -> ThreadSide {
    match side {
        Side::Old => ThreadSide::Old,
        Side::New => ThreadSide::New,
    }
}

fn side_of(side: ThreadSide) -> Side {
    match side {
        ThreadSide::Old => Side::Old,
        ThreadSide::New => Side::New,
    }
}

impl ReviewApp {
    pub(super) fn toast(&mut self, message: impl Into<String>) {
        self.toast = Some((message.into(), Instant::now()));
    }

    // ---- threads ----

    pub(super) fn load_threads(&mut self) {
        let Some(rs) = self.rs.as_mut() else { return };
        let Some(store) = &self.store else { return };
        match store.threads(rs.repo.root(), &ThreadFilter::default()) {
            Ok(threads) => {
                rs.threads = threads;
                rs.threads_generation += 1;
            }
            Err(e) => self.error = Some(format!("Loading comments failed: {e:#}")),
        }
    }

    /// Lines of the current view on `side`.
    fn view_lines(&self, side: Side) -> Option<Vec<String>> {
        let sf = self.rs.as_ref()?.commit.as_ref()?.file.as_ref()?;
        let ViewState::Ready(view) = &sf.view else { return None };
        let FileView::Text { diff, .. } = view.as_ref() else { return None };
        Some(if side == Side::Old { diff.old.lines.clone() } else { diff.new.lines.clone() })
    }

    /// Commit time of the version shown on `side`.
    fn version_time(&self, side: Side) -> Option<i64> {
        let rs = self.rs.as_ref()?;
        let sc = rs.commit.as_ref()?;
        let sf = sc.file.as_ref()?;
        if let Some(b) = &sf.browse {
            return rs.repo.commit(b.commit).ok().map(|c| c.committer.time);
        }
        Some(if side == Side::Old { sc.base_time } else { sc.info.committer.time })
    }

    /// Work out where each thread on this file sits in the current view.
    pub(super) fn ensure_placed(&mut self) {
        let Some(rs) = self.rs.as_ref() else { return };
        let generation = rs.threads_generation;
        let Some(sf) = rs.commit.as_ref().and_then(|c| c.file.as_ref()) else { return };
        if !matches!(sf.view, ViewState::Ready(_)) || sf.placed.as_ref().is_some_and(|(g, _)| *g == generation) {
            return;
        }
        let browse = sf.browse.is_some();
        let sides: Vec<Side> = if browse { vec![Side::New] } else { vec![Side::Old, Side::New] };
        let mut info = Vec::new();
        for side in sides {
            if let (Some((version, path)), Some(lines), Some(time)) =
                (self.version_of(side), self.view_lines(side), self.version_time(side))
            {
                info.push((side, version.commit.to_string(), path, lines, time));
            }
        }
        let rs = self.rs.as_ref().expect("checked above");
        let mut placed = Vec::new();
        for t in &rs.threads {
            // Threads made on exactly this version and side.
            let own = info.iter().find(|(side, version, path, _, _)| {
                *version == t.version_id && *path == t.path && (browse || *side == side_of(t.side))
            });
            let target = own.or_else(|| {
                // Older threads on this file follow the code into the newer version.
                info.iter().rfind(|(side, _, path, _, time)| *side == Side::New && *path == t.path && t.version_time <= *time)
            });
            if let Some((side, _, _, lines, _)) = target {
                placed.push(PlacedThread { thread_id: t.id, side: *side, placement: relocate(&t.anchor, t.start_line, lines) });
            }
        }
        if let Some(sf) = self.rs.as_mut().and_then(|rs| rs.commit.as_mut()).and_then(|c| c.file.as_mut()) {
            sf.placed = Some((generation, placed));
        }
    }

    pub(super) fn thread_marks(&self) -> Vec<ThreadMark> {
        let Some(rs) = self.rs.as_ref() else { return Vec::new() };
        let Some((_, placed)) = rs.commit.as_ref().and_then(|c| c.file.as_ref()).and_then(|f| f.placed.as_ref()) else {
            return Vec::new();
        };
        placed
            .iter()
            .filter_map(|p| {
                let (start, end) = p.lines()?;
                let t = rs.threads.iter().find(|t| t.id == p.thread_id)?;
                Some(ThreadMark {
                    thread_id: t.id,
                    side: p.side,
                    start,
                    end,
                    comments: t.comments.len(),
                    open: t.status == ThreadStatus::Open,
                })
            })
            .collect()
    }

    /// The lines a new comment would attach to: the selection, or the cursor line.
    pub(super) fn comment_target(&self) -> Option<(Side, u32, u32)> {
        let sf = self.rs.as_ref()?.commit.as_ref()?.file.as_ref()?;
        if !matches!(sf.view, ViewState::Ready(_)) {
            return None;
        }
        sf.selection.or_else(|| sf.cursor.map(|c| (c.side, c.line, c.line)))
    }

    pub(super) fn start_comment(&mut self) {
        let Some((side, start, end)) = self.comment_target() else {
            self.toast("Click or select lines in the diff first.");
            return;
        };
        let Some((version, path)) = self.version_of(side) else { return };
        let context = format!(
            "{path}, {} {} ({})",
            if start == end { "line" } else { "lines" },
            review_core::review::model::line_label(start, end),
            version.label
        );
        self.comment_editor = Some(CommentEditor {
            target: EditorTarget::NewThread { side, start, end },
            context,
            text: String::new(),
            preview: false,
            error: None,
        });
    }

    pub(super) fn reply_to(&mut self, thread_id: i64) {
        let Some(t) = self.rs.as_ref().and_then(|rs| rs.threads.iter().find(|t| t.id == thread_id)) else { return };
        let context = format!("Reply on {}:{}", t.path, t.line_label());
        self.comment_editor =
            Some(CommentEditor { target: EditorTarget::Reply(thread_id), context, text: String::new(), preview: false, error: None });
    }

    pub(super) fn edit_comment(&mut self, comment_id: i64) {
        let Some(rs) = self.rs.as_ref() else { return };
        let Some(c) = rs.threads.iter().flat_map(|t| &t.comments).find(|c| c.id == comment_id) else { return };
        self.comment_editor = Some(CommentEditor {
            target: EditorTarget::Edit(comment_id),
            context: format!("Edit comment by {}", c.author_name),
            text: c.body.clone(),
            preview: false,
            error: None,
        });
    }

    pub(super) fn save_comment(&mut self) {
        let Some(editor) = self.comment_editor.as_ref() else { return };
        let (target, text) = (editor.target.clone(), editor.text.clone());
        let result = self.save_comment_inner(&target, &text);
        match result {
            Ok(thread) => {
                self.comment_editor = None;
                self.load_threads();
                if let Some(sf) = self.current_file_mut() {
                    sf.selection = None;
                }
                if let Some(id) = thread {
                    self.peek.open = true;
                    self.peek.tab = PeekTab::Comments;
                    self.peek.focused_thread = Some(id);
                    self.peek.reveal_thread = true;
                }
            }
            Err(e) => {
                if let Some(editor) = self.comment_editor.as_mut() {
                    editor.error = Some(format!("{e:#}"));
                }
            }
        }
    }

    fn save_comment_inner(&mut self, target: &EditorTarget, text: &str) -> anyhow::Result<Option<i64>> {
        let store = self.store.as_ref().ok_or_else(|| anyhow::anyhow!("comments need the settings database"))?;
        let rs = self.rs.as_ref().ok_or_else(|| anyhow::anyhow!("no repository"))?;
        let (name, email) = rs.identity.clone();
        match target {
            EditorTarget::NewThread { side, start, end } => {
                let (version, path) = self.version_of(*side).ok_or_else(|| anyhow::anyhow!("no file"))?;
                let lines = self.view_lines(*side).ok_or_else(|| anyhow::anyhow!("the file is not loaded"))?;
                let version_time = self.version_time(*side).unwrap_or_default();
                let sc = rs.commit.as_ref().ok_or_else(|| anyhow::anyhow!("no commit"))?;
                let browse = sc.file.as_ref().is_some_and(|f| f.browse.is_some());
                let new = NewThread {
                    commit_id: sc.info.id.to_string(),
                    version_id: version.commit.to_string(),
                    version_time,
                    path,
                    side: if browse { ThreadSide::New } else { thread_side(*side) },
                    start_line: *start,
                    end_line: *end,
                    anchor: Anchor::capture(&lines, *start, *end),
                    author_name: name,
                    author_email: email,
                    body: text.to_string(),
                };
                Ok(Some(store.create_thread(rs.repo.root(), &new)?))
            }
            EditorTarget::Reply(id) => {
                store.add_comment(*id, &name, &email, text)?;
                Ok(Some(*id))
            }
            EditorTarget::Edit(cid) => {
                store.edit_comment(*cid, text)?;
                Ok(None)
            }
        }
    }

    pub(super) fn delete_comment(&mut self, comment_id: i64) {
        if let Some(store) = &self.store
            && let Err(e) = store.delete_comment(comment_id)
        {
            self.error = Some(format!("{e:#}"));
        }
        self.load_threads();
    }

    pub(super) fn set_thread_status(&mut self, thread_id: i64, status: ThreadStatus) {
        if let Some(store) = &self.store
            && let Err(e) = store.set_thread_status(thread_id, status)
        {
            self.error = Some(format!("{e:#}"));
        }
        self.load_threads();
    }

    /// Show a thread: in the current view if it is placed there, otherwise at the version it was made on.
    pub(super) fn go_to_thread(&mut self, thread_id: i64) {
        self.peek.open = true;
        self.peek.tab = PeekTab::Comments;
        self.peek.focused_thread = Some(thread_id);
        self.peek.reveal_thread = true;
        let unified = self.unified;
        if let Some(sf) = self.current_file_mut()
            && let Some((_, placed)) = &sf.placed
            && let Some(p) = placed.iter().find(|p| p.thread_id == thread_id)
            && let Some((start, _)) = p.lines()
        {
            sf.pending_line = Some((p.side, start));
            sf.apply_pending_line(unified);
            return;
        }
        let Some(t) = self.rs.as_ref().and_then(|rs| rs.threads.iter().find(|t| t.id == thread_id)).cloned() else { return };
        let Ok(version) = parse_id(&t.version_id) else { return };
        let Ok(commit) = parse_id(&t.commit_id) else { return };
        let current = self.rs.as_ref().and_then(|rs| rs.commit.as_ref()).map(|c| c.info.id);
        if current != Some(commit) {
            // Open the reviewed commit first, then the file in it.
            let path = (t.side == ThreadSide::New).then(|| t.path.clone());
            self.select_commit_by_id(commit, 0, path, true);
            if let Some(sc) = self.rs.as_mut().and_then(|rs| rs.commit.as_mut())
                && t.side == ThreadSide::Old
            {
                sc.pending_browse = Some((version, t.path.clone()));
            }
            return;
        }
        self.open_location(version, &t.path, t.start_line);
    }

    /// Next/previous thread in the current view, by position.
    pub(super) fn step_thread(&mut self, forward: bool) {
        let unified = self.unified;
        let Some(sf) = self.current_file_mut() else { return };
        let Some((_, placed)) = &sf.placed else { return };
        let mut rows: Vec<(usize, i64)> = placed
            .iter()
            .filter_map(|p| Some((sf.row_for_line(unified, p.side, p.lines()?.0)?, p.thread_id)))
            .collect();
        rows.sort();
        let here = sf.cursor.and_then(|c| sf.row_for_line(unified, c.side, c.line));
        let next = match (here, forward) {
            (Some(h), true) => rows.iter().find(|(r, _)| *r > h).or(rows.first()),
            (Some(h), false) => rows.iter().rev().find(|(r, _)| *r < h).or(rows.last()),
            (None, true) => rows.first(),
            (None, false) => rows.last(),
        };
        if let Some(&(_, id)) = next {
            self.go_to_thread(id);
        }
    }

    pub(super) fn comments_view(&self) -> CommentsView {
        let rs = self.rs.as_ref();
        let sc = rs.and_then(|rs| rs.commit.as_ref());
        let placed: Vec<&PlacedThread> = sc
            .and_then(|c| c.file.as_ref())
            .and_then(|f| f.placed.as_ref())
            .map(|(_, p)| p.iter().collect())
            .unwrap_or_default();
        let mut rows: Vec<ThreadRow> = rs
            .map(|rs| {
                rs.threads
                    .iter()
                    .filter(|t| self.peek.status_filter.matches(t.status))
                    .filter_map(|t| {
                        let placement = placed.iter().find(|p| p.thread_id == t.id).map(|p| p.placement);
                        let keep = match self.peek.comment_scope {
                            CommentScope::File => placement.is_some(),
                            CommentScope::Commit => sc.is_some_and(|c| parse_id(&t.commit_id).is_ok_and(|id| id == c.info.id)),
                            CommentScope::All => true,
                        };
                        keep.then(|| ThreadRow { thread: t.clone(), placement })
                    })
                    .collect()
            })
            .unwrap_or_default();
        rows.sort_by_key(|r| (r.thread.path.clone(), r.placement.and_then(Placement::lines).map_or(r.thread.start_line, |l| l.0)));
        let target = self.comment_target().map(|(side, a, b)| {
            format!("{} {} ({})", if a == b { "line" } else { "lines" }, review_core::review::model::line_label(a, b), if side == Side::Old { "old" } else { "new" })
        });
        CommentsView {
            rows,
            scope: self.peek.comment_scope,
            status: self.peek.status_filter,
            focused: self.peek.focused_thread,
            reveal: self.peek.reveal_thread,
            target,
        }
    }

    // ---- export ----

    pub(super) fn export_feedback(&mut self, copy: bool) {
        let Some(rs) = self.rs.as_ref() else { return };
        let open: Vec<_> = rs.threads.iter().filter(|t| t.status == ThreadStatus::Open).cloned().collect();
        let repo = rs.repo.clone();
        let target = rs.repo.root().join(&rs.settings.feedback_path);
        let repo_name = repo_name(rs.repo.root());
        self.pool.spawn("Exporting review feedback", move |_, send| {
            let result = (|| -> anyhow::Result<ExportDone> {
                let head = repo.head_id()?.ok_or_else(|| anyhow::anyhow!("the repository has no commits"))?;
                let items: Vec<FeedbackItem> = open
                    .into_iter()
                    .map(|thread| {
                        let at_head = export::locate_at(&repo, head, &thread);
                        let commit_summary = parse_id(&thread.commit_id).ok().and_then(|id| repo.commit(id).ok()).map(|c| c.summary);
                        FeedbackItem { thread, commit_summary, at_head }
                    })
                    .collect();
                let ctx = ExportContext { repo_name, head: head.to_hex_with_len(8).to_string(), generated_at: review_core::settings::now() };
                let text = export::feedback_markdown(&ctx, &items);
                if copy {
                    return Ok(ExportDone::Copied { text, count: items.len() });
                }
                std::fs::write(&target, text)?;
                Ok(ExportDone::Written(format!("Wrote {} open comment(s) to {}", items.len(), target.display())))
            })();
            send(JobResult::Exported(result.map_err(|e| format!("{e:#}"))));
        });
    }

    pub(super) fn on_exported(&mut self, result: Result<ExportDone, String>) {
        match result {
            Ok(ExportDone::Written(msg)) => self.toast(msg),
            Ok(ExportDone::Copied { text, count }) => {
                self.ctx.copy_text(text);
                self.toast(format!("Copied {count} open comment(s) as a prompt"));
            }
            Err(e) => self.error = Some(format!("Export failed: {e}")),
        }
    }

    pub(super) fn save_report(&mut self) {
        let Some(rs) = self.rs.as_ref() else { return };
        let Some(sc) = rs.commit.as_ref() else { return };
        let title = match (&rs.range, sc.base) {
            (Some(r), Some(_)) => format!("Range {}", r.label),
            _ => format!("Commit {}: {}", sc.info.short_id(), sc.info.summary),
        };
        let files = sc
            .files()
            .iter()
            .map(|f| ReportFile { path: f.path().to_string(), status: f.status.letter(), reviewed: sc.reviewed.contains(f.path()) })
            .collect();
        let id = sc.info.id.to_string();
        let threads = rs.threads.iter().filter(|t| t.commit_id == id).cloned().collect();
        let ctx = ExportContext { repo_name: repo_name(rs.repo.root()), head: sc.info.short_id(), generated_at: review_core::settings::now() };
        let text = export::report_markdown(&ctx, &Report { title, files, threads });
        let default_name = format!("review-{}.md", sc.info.short_id());
        let Some(path) = rfd::FileDialog::new().set_title("Save review report").set_file_name(default_name).add_filter("Markdown", &["md"]).save_file() else {
            return;
        };
        match std::fs::write(&path, text) {
            Ok(()) => self.toast(format!("Saved review report to {}", path.display())),
            Err(e) => self.error = Some(format!("Saving the report failed: {e}")),
        }
    }

    // ---- ranges ----

    pub(super) fn open_compare(&mut self) {
        let Some(rs) = self.rs.as_ref() else { return };
        let base_branch = range::base_branch(&rs.repo, &rs.settings.base_branch);
        let current_branch = rs.repo.current_branch();
        let spec = rs.range.as_ref().map(|r| r.label.clone()).unwrap_or_default();
        self.compare = Some(CompareDialog { spec, refs: rs.repo.ref_names().unwrap_or_default(), base_branch, current_branch, error: None });
    }

    pub(super) fn compare_range(&mut self, spec: &str) {
        let Some(rs) = self.rs.as_ref() else { return };
        match range::resolve(&rs.repo, spec) {
            Ok(r) => {
                self.compare = None;
                self.enter_range(r);
            }
            Err(e) => {
                if let Some(d) = self.compare.as_mut() {
                    d.error = Some(format!("{e:#}"));
                }
            }
        }
    }

    pub(super) fn enter_range(&mut self, r: CommitRange) {
        let Some(rs) = self.rs.as_mut() else { return };
        let pager = CommitPager::start_range(rs.repo.clone(), r.tip, vec![r.base], self.notify.clone());
        pager.request(FIRST_PAGE);
        rs.pager = Some(pager);
        rs.pager_waiting = true;
        rs.pager_done = false;
        rs.commits.clear();
        rs.refilter();
        rs.range = Some(r);
        self.select_whole_range();
    }

    pub(super) fn exit_range(&mut self) {
        let Some(rs) = self.rs.as_mut() else { return };
        rs.range = None;
        rs.commit = None;
        self.restart_commit_list();
    }

    pub(super) fn select_whole_range(&mut self) {
        let Some(rs) = self.rs.as_ref() else { return };
        let Some(r) = rs.range.clone() else { return };
        match rs.repo.commit(r.tip) {
            Ok(info) => self.select_commit_with(info, 0, Some(r.base), None, true),
            Err(e) => self.error = Some(format!("{e:#}")),
        }
    }

    // ---- live refresh ----

    /// Reload the commit list from HEAD, keeping the selection.
    pub(super) fn restart_commit_list(&mut self) {
        let Some(rs) = self.rs.as_mut() else { return };
        let head = rs.repo.head_id().ok().flatten();
        rs.head = head;
        rs.commits.clear();
        rs.refilter();
        rs.pager = head.map(|tip| {
            let p = CommitPager::start(rs.repo.clone(), tip, self.notify.clone());
            p.request(FIRST_PAGE);
            p
        });
        rs.pager_waiting = rs.pager.is_some();
        rs.pager_done = rs.pager.is_none();
    }

    pub(super) fn poll_watcher(&mut self) {
        let Some(rs) = self.rs.as_mut() else { return };
        if !rs.watcher.as_ref().is_some_and(RepoWatcher::take_changed) {
            return;
        }
        if let Some(r) = rs.range.clone() {
            if let Ok(updated) = range::resolve(&rs.repo, &r.label)
                && updated != r
            {
                self.toast(format!("{} changed; showing the new commits", r.label));
                self.enter_range(updated);
            }
            return;
        }
        let head = rs.repo.head_id().ok().flatten();
        if head != rs.head {
            let ignore = head.map(|h| review_core::blame::ignore_revs(&rs.repo, h)).unwrap_or_default();
            self.ignore_revs = Arc::new(ignore);
            self.restart_commit_list();
            self.toast("New commits");
        }
    }

    pub(super) fn start_watcher(&mut self) {
        let Some(rs) = self.rs.as_mut() else { return };
        rs.watcher = None;
        if !rs.settings.live_refresh {
            return;
        }
        match RepoWatcher::start(&rs.repo.git_dir(), self.notify.clone()) {
            Ok(w) => rs.watcher = Some(w),
            Err(e) => self.error = Some(format!("Live refresh unavailable: {e:#}")),
        }
    }

    // ---- settings ----

    pub(super) fn open_settings(&mut self) {
        let Some(rs) = self.rs.as_ref() else { return };
        let s = &rs.settings;
        self.settings_dialog = Some(SettingsDialog {
            base_branch: s.base_branch.clone(),
            ignored_paths: s.ignored_paths.join("\n"),
            feedback_path: s.feedback_path.clone(),
            live_refresh: s.live_refresh,
        });
    }

    pub(super) fn save_settings(&mut self) {
        let Some(d) = self.settings_dialog.take() else { return };
        let Some(rs) = self.rs.as_mut() else { return };
        let settings = RepoSettings {
            base_branch: d.base_branch.trim().to_string(),
            ignored_paths: d.ignored_paths.lines().map(str::trim).filter(|l| !l.is_empty()).map(String::from).collect(),
            feedback_path: if d.feedback_path.trim().is_empty() { "REVIEW_FEEDBACK.md".into() } else { d.feedback_path.trim().to_string() },
            live_refresh: d.live_refresh,
        };
        if let Some(store) = &self.store
            && let Err(e) = store.save_repo_settings(rs.repo.root(), &settings)
        {
            self.error = Some(format!("{e:#}"));
        }
        let restart_watch = rs.settings.live_refresh != settings.live_refresh;
        rs.settings = settings;
        if restart_watch {
            self.start_watcher();
        }
        self.toast("Settings saved");
    }
}

pub(super) fn repo_name(root: &Path) -> String {
    root.file_name().map_or_else(|| root.display().to_string(), |n| n.to_string_lossy().into_owned())
}
