//! Application state that outlives a single frame.

use std::collections::HashSet;
use std::ops::Range;
use std::sync::Arc;

use review_core::ObjectId;
use review_core::diff::{DiffOptions, Row, hunk_ranges};
use review_core::file_view::FileView;
use review_core::analysis::AnalysisResult;
use review_core::analysis::complexity::ComplexityChange;
use review_core::analysis::secrets::SecretFinding;
use review_core::blame::Blame;
use review_core::git::{ChangeStatus, CommitInfo, FileChange, FileSide, Repo, TreeFile};
use review_core::highlight::{Highlighted, ThemeKind};
use review_core::range::CommitRange;
use review_core::review::{Placement, Thread, ThreadStatus};
use review_core::watch::RepoWatcher;
use review_core::settings::RepoSettings;
use review_core::symbols::{Located, Lookup, RefHit, SymbolIndex};
use review_core::tasks::{CancelToken, CommitPager};

use crate::nav::History;

/// Identifies a computed file view; equal keys give equal views, so results can be cached.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ViewKey {
    pub old: Option<ObjectId>,
    pub new: Option<ObjectId>,
    pub path: String,
    pub opts: DiffOptions,
    pub force: bool,
}

impl ViewKey {
    pub fn for_change(change: &FileChange, opts: DiffOptions, force: bool) -> Self {
        ViewKey {
            old: change.old.as_ref().map(|s| s.id),
            new: change.new.as_ref().map(|s| s.id),
            path: change.path().to_string(),
            opts,
            force,
        }
    }
}

pub type HighlightKey = (ObjectId, ThemeKind);

/// A file at a commit: what blame and the symbol index are computed for.
pub type BlameKey = (ObjectId, String);

/// Which pane: the parent (old) or the commit (new).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Side {
    Old,
    New,
}

/// A text position the user clicked: 0-based line, byte column.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Cursor {
    pub side: Side,
    pub line: u32,
    pub col: u32,
}

pub enum JobResult {
    Files {
        commit: ObjectId,
        parent: Option<ObjectId>,
        result: Result<Vec<FileChange>, String>,
    },
    View {
        key: ViewKey,
        result: Result<Arc<FileView>, String>,
    },
    Highlight {
        key: HighlightKey,
        result: Option<Arc<Highlighted>>,
    },
    Index {
        commit: ObjectId,
        result: Result<Option<Arc<SymbolIndex>>, String>,
    },
    Blame {
        key: BlameKey,
        result: Arc<Result<Blame, String>>,
    },
    Tree {
        commit: ObjectId,
        result: Result<Vec<TreeFile>, String>,
    },
    /// An export finished: a message for the user, or an error.
    Exported(Result<ExportDone, String>),
    Analysis {
        key: AnalysisKey,
        result: Result<Arc<AnalysisResult>, String>,
    },
    Secrets {
        key: AnalysisKey,
        findings: Vec<(String, SecretFinding)>,
    },
    /// Language server answers, tagged with the request they belong to.
    LspDefinition {
        request: u64,
        result: Result<Vec<LspTarget>, String>,
    },
    LspReferences {
        request: u64,
        result: Result<Vec<LspTarget>, String>,
    },
    Risks(Vec<(ObjectId, Vec<review_core::laravel::risk::Risk>)>),
    UpdateCheck {
        /// The user asked, so "up to date" is worth saying.
        manual: bool,
        result: Result<Option<crate::updater::UpdateInfo>, String>,
    },
    UpdateFailed(String),
    Pickaxe {
        query: String,
        result: Result<Vec<review_core::history::PickaxeHit>, String>,
    },
    LineHistory {
        label: String,
        result: Result<LineHistoryResult, String>,
    },
    Laravel {
        key: AnalysisKey,
        result: Result<LaravelData, String>,
    },
    Advisories {
        key: AnalysisKey,
        result: Result<Vec<review_core::laravel::composer::Advisory>, String>,
    },
    ArtisanRoutes {
        key: AnalysisKey,
        result: Result<Vec<review_core::laravel::routes::RouteChange>, String>,
    },
}

/// Laravel views for one diff (old → new).
#[derive(Debug, Clone, Default)]
pub struct LaravelData {
    pub schema_changes: Vec<review_core::laravel::schema::TableChange>,
    pub schema: review_core::laravel::schema::Schema,
    /// `None` when composer.lock didn't change.
    pub packages: Option<Vec<review_core::laravel::composer::PackageChange>>,
    pub route_changes: Vec<review_core::laravel::routes::RouteChange>,
    pub routes: Vec<review_core::laravel::routes::RouteDef>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum LaravelTab {
    #[default]
    Schema,
    Packages,
    Routes,
}

/// Commits that changed a line range, and who first wrote its first line.
#[derive(Debug, Clone)]
pub struct LineHistoryResult {
    pub steps: Vec<(review_core::history::LineStep, CommitInfo)>,
    pub origin: Option<CommitInfo>,
}

/// A background result: `Err(None)` while running, `Err(Some(message))` on failure.
pub type Pending<T> = Result<T, Option<String>>;

/// State of the History tab.
#[derive(Default)]
pub struct HistoryPanel {
    pub query: String,
    pub regex: bool,
    pub this_file: bool,
    /// The query that produced `pickaxe`, and the results; `Err(None)` while running.
    pub pickaxe: Option<(String, Pending<Vec<review_core::history::PickaxeHit>>)>,
    pub lines: Option<(String, Pending<LineHistoryResult>)>,
}

/// State of the Laravel tab for the selected diff.
#[derive(Default)]
pub struct LaravelPanel {
    pub tab: LaravelTab,
    pub data: std::collections::HashMap<AnalysisKey, Result<LaravelData, Option<String>>>,
    pub advisories: std::collections::HashMap<AnalysisKey, Result<Vec<review_core::laravel::composer::Advisory>, Option<String>>>,
    pub artisan: std::collections::HashMap<AnalysisKey, Result<Vec<review_core::laravel::routes::RouteChange>, Option<String>>>,
    pub route_filter: String,
}

/// Old and new commit of a diff: what analysis results are cached under.
pub type AnalysisKey = (Option<ObjectId>, ObjectId);

pub enum AnalysisState {
    Running,
    Done(Arc<AnalysisResult>),
    Failed(String),
}

/// A location from the language server, mapped back to the repository when possible.
#[derive(Debug, Clone)]
pub struct LspTarget {
    /// Repository-relative path when the file is part of the commit's tree.
    pub rel: Option<String>,
    /// Where the file is on disk (snapshot, or the working copy's vendor/).
    pub abs: std::path::PathBuf,
    pub line: u32,
    pub context: String,
}

impl LspTarget {
    pub fn label(&self) -> String {
        let path = self.rel.clone().unwrap_or_else(|| self.abs.display().to_string());
        format!("{path}:{}", self.line + 1)
    }
}

/// Signals computed for the open file.
#[derive(Debug, Clone, Default)]
pub struct FileQuality {
    pub complexity: Vec<ComplexityChange>,
    pub secrets: Vec<SecretFinding>,
    /// Added lines (new side) not covered by tests; `None` without coverage for this file.
    pub uncovered: Option<Vec<u32>>,
}

pub enum ExportDone {
    Written(String),
    Copied { text: String, count: usize },
}

pub struct RepoState {
    pub repo: Repo,
    pub settings: RepoSettings,
    pub commits: Vec<CommitInfo>,
    pub pager: Option<CommitPager>,
    pub pager_waiting: bool,
    pub pager_done: bool,
    pub ai_only: bool,
    /// Indices into `commits` that pass the current filter.
    pub visible: Vec<usize>,
    pub commit: Option<SelectedCommit>,
    pub history: History,
    /// Scroll the commit list so the selected commit is visible.
    pub reveal_commit: bool,
    /// The tip the commit list was loaded from, to notice new commits.
    pub head: Option<ObjectId>,
    /// When set, the list shows only `range.base..range.tip` and the whole range can be reviewed.
    pub range: Option<CommitRange>,
    pub watcher: Option<RepoWatcher>,
    /// `user.name` / `user.email` for new comments.
    pub identity: (String, String),
    /// All comment threads of the repository.
    pub threads: Vec<Thread>,
    /// Bumped whenever `threads` is reloaded, so cached placements can be refreshed.
    pub threads_generation: u64,
    pub bookmarks: Vec<review_core::review::bookmarks::Bookmark>,
}

impl RepoState {
    pub fn refilter(&mut self) {
        self.visible = (0..self.commits.len())
            .filter(|&i| !self.ai_only || self.commits[i].ai_assisted)
            .collect();
    }

    pub fn selected_visible_index(&self) -> Option<usize> {
        let id = self.commit.as_ref()?.info.id;
        self.visible.iter().position(|&i| self.commits[i].id == id)
    }
}

pub enum FilesState {
    Loading,
    Ready(Vec<FileChange>),
    Error(String),
}

/// Where to put the cursor once a file's diff is ready.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HunkTarget {
    First,
    Last,
}

pub struct SelectedCommit {
    /// The commit, or the range tip when reviewing a whole range.
    pub info: CommitInfo,
    /// Range base: when set, the diff is `base..info` instead of against a parent.
    pub base: Option<ObjectId>,
    /// Committer time of the old side's commit, for ordering comment versions.
    pub base_time: i64,
    pub parent_idx: usize,
    pub files: FilesState,
    pub reviewed: HashSet<String>,
    pub file: Option<SelectedFile>,
    /// File to select once the file list arrives (history navigation).
    pub pending_path: Option<String>,
    pub pending_target: HunkTarget,
    /// Scroll the file list so the selected file is visible.
    pub reveal_file: bool,
    /// Every file at the commit, for "All files" mode; loaded on demand.
    pub tree: Option<Result<Vec<TreeFile>, String>>,
    /// Whole file to open once the file list arrives (history navigation).
    pub pending_browse: Option<(ObjectId, String)>,
    /// Line to reveal in the file opened from `pending_path`.
    pub pending_line: Option<(Side, u32)>,
}

impl SelectedCommit {
    pub fn parent(&self) -> Option<ObjectId> {
        self.base.or_else(|| self.info.parents.get(self.parent_idx).copied())
    }

    /// Key under which per-file review marks are stored: the commit, or the range.
    pub fn review_key(&self) -> String {
        match self.base {
            Some(base) => format!("{base}..{}", self.info.id),
            None => self.info.id.to_string(),
        }
    }

    pub fn files(&self) -> &[FileChange] {
        match &self.files {
            FilesState::Ready(files) => files,
            _ => &[],
        }
    }
}

pub enum ViewState {
    /// Not loaded until the user asks (ignored or generated path).
    Collapsed(&'static str),
    Loading,
    Ready(Arc<FileView>),
    Error(String),
}

/// Rows for the current layout (split or unified) and their change hunks.
pub struct Layout {
    pub rows: Arc<Vec<Row>>,
    pub hunks: Vec<Range<usize>>,
}

/// A whole file opened at some version (not a diff).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Browse {
    pub commit: ObjectId,
    /// Short description of the version, e.g. "parent 1a2b3c4d".
    pub label: String,
}

pub struct SelectedFile {
    /// Position in the commit's changed files; `None` when browsing an unchanged file.
    pub index: Option<usize>,
    pub browse: Option<Browse>,
    pub change: FileChange,
    pub force: bool,
    pub view: ViewState,
    pub key: Option<ViewKey>,
    pub token: Option<CancelToken>,
    pub split: Option<Layout>,
    pub unified: Option<Layout>,
    pub max_line_chars: usize,
    pub current_hunk: Option<usize>,
    pub scroll_to_row: Option<usize>,
    pub h_scroll: f32,
    pub target: HunkTarget,
    pub cursor: Option<Cursor>,
    /// Lines selected for a comment: side and inclusive line range.
    pub selection: Option<(Side, u32, u32)>,
    /// Comment threads placed on this view; recomputed when threads or the view change.
    pub placed: Option<(u64, Vec<PlacedThread>)>,
    pub quality: Option<FileQuality>,
    /// Line to reveal once the view is ready.
    pub pending_line: Option<(Side, u32)>,
}

impl SelectedFile {
    pub fn new(index: usize, change: FileChange, target: HunkTarget) -> Self {
        Self::with(Some(index), None, change, target)
    }

    /// Show `file` as it is at `browse.commit`, without a diff.
    pub fn browse(file: TreeFile, browse: Browse) -> Self {
        let side = FileSide { path: file.path, id: file.id, entry: file.entry };
        let change = FileChange { status: ChangeStatus::Modified, old: Some(side.clone()), new: Some(side) };
        Self::with(None, Some(browse), change, HunkTarget::First)
    }

    fn with(index: Option<usize>, browse: Option<Browse>, change: FileChange, target: HunkTarget) -> Self {
        SelectedFile {
            index,
            browse,
            change,
            force: false,
            view: ViewState::Loading,
            key: None,
            token: None,
            split: None,
            unified: None,
            max_line_chars: 0,
            current_hunk: None,
            scroll_to_row: None,
            h_scroll: 0.0,
            target,
            cursor: None,
            selection: None,
            placed: None,
            quality: None,
            pending_line: None,
        }
    }

    /// Install a freshly computed view and position the cursor.
    pub fn set_view(&mut self, view: Arc<FileView>) {
        if let FileView::Text { diff, .. } = view.as_ref() {
            self.max_line_chars = diff
                .old
                .lines
                .iter()
                .chain(&diff.new.lines)
                .map(|l| l.chars().count() + l.matches('\t').count() * 3)
                .max()
                .unwrap_or(0);
            self.split = Some(Layout { rows: Arc::new(diff.rows.clone()), hunks: diff.hunks.clone() });
            let unified = diff.unified_rows();
            let hunks = hunk_ranges(&unified);
            self.unified = Some(Layout { rows: Arc::new(unified), hunks });
            let count = diff.hunks.len();
            let keep = self.current_hunk.filter(|&h| h < count);
            self.current_hunk = keep.or(match (count, self.target) {
                (0, _) => None,
                (_, HunkTarget::First) => Some(0),
                (n, HunkTarget::Last) => Some(n - 1),
            });
        }
        self.view = ViewState::Ready(view);
    }

    /// Browsed files are shown as a single pane.
    pub fn single_pane(&self) -> bool {
        self.browse.is_some()
    }

    /// Row showing `line` of `side` in the given layout.
    pub fn row_for_line(&self, unified: bool, side: Side, line: u32) -> Option<usize> {
        let layout = self.layout(unified)?;
        layout.rows.iter().position(|r| {
            let cell = match side {
                Side::Old => &r.left,
                Side::New => &r.right,
            };
            cell.as_ref().is_some_and(|c| c.line == line)
        })
    }

    /// Scroll to a pending line, once rows exist. Returns true if applied.
    pub fn apply_pending_line(&mut self, unified: bool) -> bool {
        let Some((side, line)) = self.pending_line else { return false };
        let Some(row) = self.row_for_line(unified, side, line) else { return false };
        self.scroll_to_row = Some(row);
        self.cursor = Some(Cursor { side, line, col: 0 });
        self.pending_line = None;
        true
    }

    pub fn layout(&self, unified: bool) -> Option<&Layout> {
        if unified { self.unified.as_ref() } else { self.split.as_ref() }
    }

    pub fn hunk_count(&self) -> usize {
        self.split.as_ref().map_or(0, |l| l.hunks.len())
    }

    pub fn reveal_current_hunk(&mut self, unified: bool) {
        let row = self
            .current_hunk
            .and_then(|h| self.layout(unified).and_then(|l| l.hunks.get(h)))
            .map(|r| r.start);
        if let Some(row) = row {
            self.scroll_to_row = Some(row);
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PeekTab {
    Definition,
    References,
    Blame,
    Comments,
    Quality,
    History,
    Laravel,
    Bookmarks,
}

/// A version of the code: the commit whose tree a lookup runs against.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Version {
    pub commit: ObjectId,
    pub label: String,
}

pub enum DefinitionState {
    /// Waiting for the symbol index of `version`.
    Pending { path: String, line: u32, col: u32 },
    Ready { lookup: Lookup, selected: usize },
    /// Waiting for the language server.
    LspPending { request: u64, path: String, line: u32, col: u32 },
    Lsp { name: String, targets: Vec<LspTarget>, selected: usize },
    NotFound(String),
}

pub struct DefinitionPeek {
    pub version: Version,
    pub state: DefinitionState,
    /// Source lines of the selected candidate, with its starting line.
    pub snippet: Option<Snippet>,
}

pub struct Snippet {
    pub key: (String, u32),
    pub first_line: u32,
    pub lines: Vec<String>,
    pub highlighted: Option<Highlighted>,
}

pub enum ReferencesState {
    Pending,
    Ready { target: Located, hits: Vec<RefHit> },
    LspPending { request: u64 },
    Lsp { name: String, targets: Vec<LspTarget> },
    NotFound(String),
}

pub struct ReferencesPeek {
    pub version: Version,
    /// Position the search started from, re-resolved once the index is ready.
    pub origin: (String, u32, u32),
    pub state: ReferencesState,
}

pub struct Peek {
    pub open: bool,
    pub tab: PeekTab,
    pub definition: Option<DefinitionPeek>,
    pub references: Option<ReferencesPeek>,
    pub comment_scope: CommentScope,
    pub status_filter: StatusFilter,
    /// Thread to highlight and scroll to in the Comments tab.
    pub focused_thread: Option<i64>,
    /// Scroll the Comments tab to `focused_thread` on the next frame.
    pub reveal_thread: bool,
}

impl Default for Peek {
    fn default() -> Self {
        Peek {
            open: false,
            tab: PeekTab::Definition,
            definition: None,
            references: None,
            comment_scope: CommentScope::File,
            status_filter: StatusFilter::Open,
            focused_thread: None,
            reveal_thread: false,
        }
    }
}

/// A thread shown on the current file view.
#[derive(Debug, Clone)]
pub struct PlacedThread {
    pub thread_id: i64,
    pub side: Side,
    pub placement: Placement,
}

impl PlacedThread {
    pub fn lines(&self) -> Option<(u32, u32)> {
        self.placement.lines()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommentScope {
    File,
    Commit,
    All,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StatusFilter {
    Open,
    Closed,
    All,
}

impl StatusFilter {
    pub fn matches(self, status: ThreadStatus) -> bool {
        match self {
            StatusFilter::Open => status == ThreadStatus::Open,
            StatusFilter::Closed => status != ThreadStatus::Open,
            StatusFilter::All => true,
        }
    }
}

/// What the comment editor will do on save.
#[derive(Debug, Clone, PartialEq)]
pub enum EditorTarget {
    NewThread { side: Side, start: u32, end: u32 },
    Reply(i64),
    Edit(i64),
}

pub struct CommentEditor {
    pub target: EditorTarget,
    /// Where the comment goes, for the editor's title.
    pub context: String,
    pub text: String,
    pub preview: bool,
    pub error: Option<String>,
}

pub struct CompareDialog {
    pub spec: String,
    pub refs: Vec<String>,
    pub base_branch: Option<String>,
    pub current_branch: Option<String>,
    pub error: Option<String>,
}

pub struct SettingsDialog {
    pub base_branch: String,
    pub ignored_paths: String,
    pub feedback_path: String,
    pub live_refresh: bool,
    pub trusted: bool,
    pub coverage_path: String,
    pub phpstan_cmd: String,
    pub pint_cmd: String,
    pub lsp_cmd: String,
    pub use_lsp: bool,
}

/// Global appearance preferences.
#[derive(Debug, Clone, PartialEq)]
pub struct Prefs {
    pub theme: egui::ThemePreference,
    /// UI zoom (Cmd/Ctrl +/−).
    pub zoom: f32,
    /// Code font size in points (before zoom).
    pub code_font: f32,
    /// Height of the peek panel, in points.
    pub peek_height: f32,
}

pub struct PrefsDialog {
    pub prefs: Prefs,
    pub check_updates: bool,
    pub unified: bool,
    pub blame_on: bool,
    pub coverage_on: bool,
}
