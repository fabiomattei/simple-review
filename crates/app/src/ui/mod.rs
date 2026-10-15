//! Widgets. They read state and report what the user did as [`Action`]s; the app applies them.

pub mod comments;
pub mod bookmarks;
pub mod commits;
pub mod dialogs;
pub mod diff;
pub mod files;
pub mod header;
pub mod help;
pub mod history;
pub mod laravel;
pub mod markdown;
pub mod peek;
pub mod quality;
pub mod theme;

use std::path::PathBuf;

use egui::Color32;
use review_core::ObjectId;
use review_core::git::ChangeStatus;

use review_core::review::ThreadStatus;

use crate::state::{CommentScope, Cursor, PeekTab, StatusFilter};

#[derive(Debug, Clone, PartialEq)]
pub enum Action {
    ShareThreads,
    ImportThreads,
    CheckUpdates,
    InstallUpdate,
    AddBookmark,
    OpenBookmark(i64),
    RenameBookmark(i64, String),
    DeleteBookmark(i64),
    ShowBookmarks,
    OpenPrefs,
    SavePrefs,
    SwitchTab(usize),
    CloseTab(usize),
    NewTab,
    ComputeRisks(Vec<ObjectId>),
    PickaxeSearch,
    LineHistory,
    OpenPickaxeHit { commit: ObjectId, path: String },
    OpenHistoryStep(usize),
    BisectMark(BisectMark),
    BisectReset,
    BisectGo,
    LaravelTab(crate::state::LaravelTab),
    CheckAdvisories,
    ArtisanRoutes,
    OpenRoute(usize),
    OpenTestsFor(String),
    GoToLine { path: String, line: u32 },
    RunAnalysis,
    ToggleCoverage,
    ShowQuality,
    SetCursor(Cursor),
    /// Extend the line selection from the cursor to here (drag or Shift+click).
    SelectTo(Cursor),
    StartComment,
    SaveComment,
    ReplyTo(i64),
    EditComment(i64),
    DeleteComment(i64),
    SetThreadStatus(i64, ThreadStatus),
    GoToThread(i64),
    NextThread,
    PrevThread,
    CommentScope(CommentScope),
    CommentStatusFilter(StatusFilter),
    WriteFeedback,
    CopyFeedback,
    SaveReport,
    OpenCompare,
    CompareRange(String),
    ExitRange,
    SelectWholeRange,
    OpenSettings,
    SaveSettings,
    CloseDialogs,
    GoToDefinition,
    FindReferences,
    OpenCommit { id: ObjectId, path: Option<String> },
    ToggleBlame,
    ToggleBlameMode,
    ToggleAllFiles,
    OpenTreeFile(String),
    PeekTab(PeekTab),
    PeekSelect(usize),
    OpenDefinition,
    OpenReference(usize),
    ClosePeek,
    OpenDialog,
    OpenRepo(PathBuf),
    Reload,
    SelectCommit(usize),
    NextCommit,
    PrevCommit,
    SelectFile(usize),
    NextFile,
    PrevFile,
    SetReviewed(usize, bool),
    ToggleReviewed,
    SetParent(usize),
    CycleParent,
    NextChange,
    PrevChange,
    ClickRow(usize),
    Back,
    Forward,
    ToggleWhitespace,
    ToggleIntraLine,
    ToggleUnified,
    ToggleAiOnly,
    ExpandFile,
    ToggleHelp,
    LoadMoreCommits,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BisectMark {
    Good,
    Bad,
    Skip,
}

/// Colours that depend on the light/dark theme.
pub struct Palette {
    pub removed_bg: Color32,
    pub removed_inline: Color32,
    pub added_bg: Color32,
    pub added_inline: Color32,
    pub moved_bg: Color32,
    pub filler: Color32,
    pub text: Color32,
    pub weak: Color32,
    pub accent: Color32,
    pub ai: Color32,
    pub warn: Color32,
    pub cursor_line: Color32,
    pub selection: Color32,
    pub dark: bool,
}

impl Palette {
    pub fn new(dark: bool) -> Self {
        if dark {
            Palette {
                removed_bg: Color32::from_rgb(0x4b, 0x1f, 0x24),
                removed_inline: Color32::from_rgb(0x8c, 0x2f, 0x39),
                added_bg: Color32::from_rgb(0x1d, 0x3d, 0x2a),
                added_inline: Color32::from_rgb(0x2b, 0x6e, 0x45),
                moved_bg: Color32::from_rgb(0x2a, 0x2f, 0x52),
                filler: Color32::from_rgb(0x24, 0x26, 0x2b),
                text: Color32::from_rgb(0xd4, 0xd7, 0xdd),
                weak: Color32::from_rgb(0x7d, 0x84, 0x90),
                accent: Color32::from_rgb(0x0a, 0x84, 0xff),
                ai: Color32::from_rgb(0xb3, 0x88, 0xff),
                warn: Color32::from_rgb(0xe5, 0xa5, 0x3c),
                cursor_line: Color32::from_rgba_unmultiplied(0x0a, 0x84, 0xff, 28),
                selection: Color32::from_rgba_unmultiplied(0x0a, 0x84, 0xff, 60),
                dark,
            }
        } else {
            Palette {
                removed_bg: Color32::from_rgb(0xff, 0xeb, 0xe9),
                removed_inline: Color32::from_rgb(0xff, 0xc0, 0xbb),
                added_bg: Color32::from_rgb(0xe6, 0xff, 0xec),
                added_inline: Color32::from_rgb(0xab, 0xf2, 0xbc),
                moved_bg: Color32::from_rgb(0xe8, 0xec, 0xff),
                filler: Color32::from_rgb(0xf3, 0xf4, 0xf6),
                text: Color32::from_rgb(0x1f, 0x23, 0x28),
                weak: Color32::from_rgb(0x6e, 0x77, 0x81),
                accent: Color32::from_rgb(0x00, 0x7a, 0xff),
                ai: Color32::from_rgb(0x82, 0x50, 0xdf),
                warn: Color32::from_rgb(0x9a, 0x67, 0x00),
                cursor_line: Color32::from_rgba_unmultiplied(0x00, 0x7a, 0xff, 22),
                selection: Color32::from_rgba_unmultiplied(0x00, 0x7a, 0xff, 50),
                dark,
            }
        }
    }

    pub fn status(&self, status: ChangeStatus) -> Color32 {
        match status {
            ChangeStatus::Added => Color32::from_rgb(0x2d, 0xa4, 0x4e),
            ChangeStatus::Deleted => Color32::from_rgb(0xd1, 0x24, 0x2f),
            ChangeStatus::Modified => self.warn,
            ChangeStatus::Renamed | ChangeStatus::Copied => self.accent,
        }
    }
}

pub fn palette(ui: &egui::Ui) -> Palette {
    Palette::new(ui.visuals().dark_mode)
}

pub fn human_size(bytes: u64) -> String {
    const UNITS: [&str; 4] = ["B", "KB", "MB", "GB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 { format!("{bytes} B") } else { format!("{value:.1} {}", UNITS[unit]) }
}

/// A Unix timestamp in local-agnostic form (UTC), for comment headers.
pub fn time_label(seconds: i64) -> String {
    let sig = review_core::git::Signature { name: String::new(), email: String::new(), time: seconds, offset: 0 };
    format!("{} UTC", review_core::git::format_time(&sig))
}
