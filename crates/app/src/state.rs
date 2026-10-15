//! Application state that outlives a single frame.

use std::collections::HashSet;
use std::ops::Range;
use std::sync::Arc;

use review_core::ObjectId;
use review_core::diff::{DiffOptions, Row, hunk_ranges};
use review_core::file_view::FileView;
use review_core::blame::Blame;
use review_core::git::{ChangeStatus, CommitInfo, FileChange, FileSide, Repo, TreeFile};
use review_core::highlight::{Highlighted, ThemeKind};
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
    pub info: CommitInfo,
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
}

impl SelectedCommit {
    pub fn parent(&self) -> Option<ObjectId> {
        self.info.parents.get(self.parent_idx).copied()
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
}

impl Default for Peek {
    fn default() -> Self {
        Peek { open: false, tab: PeekTab::Definition, definition: None, references: None }
    }
}
