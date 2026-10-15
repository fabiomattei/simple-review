//! Review comments: threads anchored to lines of a file at some version.

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ThreadStatus {
    Open,
    Resolved,
    WontFix,
}

impl ThreadStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            ThreadStatus::Open => "open",
            ThreadStatus::Resolved => "resolved",
            ThreadStatus::WontFix => "wont_fix",
        }
    }

    pub fn parse(s: &str) -> Self {
        match s {
            "resolved" => ThreadStatus::Resolved,
            "wont_fix" => ThreadStatus::WontFix,
            _ => ThreadStatus::Open,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            ThreadStatus::Open => "Open",
            ThreadStatus::Resolved => "Resolved",
            ThreadStatus::WontFix => "Won't fix",
        }
    }
}

/// Which side of a diff the commented lines are on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ThreadSide {
    /// The parent (or range base) version.
    Old,
    /// The commit (or range tip) version.
    New,
}

impl ThreadSide {
    pub fn as_str(self) -> &'static str {
        match self {
            ThreadSide::Old => "old",
            ThreadSide::New => "new",
        }
    }

    pub fn parse(s: &str) -> Self {
        if s == "old" { ThreadSide::Old } else { ThreadSide::New }
    }
}

/// Lines of context kept on each side of the commented lines.
pub const ANCHOR_CONTEXT: usize = 3;

/// The commented lines and a little context, used to find them again in other versions.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Anchor {
    pub before: Vec<String>,
    pub lines: Vec<String>,
    pub after: Vec<String>,
}

impl Anchor {
    /// Capture lines `start..=end` (0-based) of `file`.
    pub fn capture(file: &[String], start: u32, end: u32) -> Self {
        let (start, end) = (start as usize, (end as usize).min(file.len().saturating_sub(1)));
        let start = start.min(end);
        Anchor {
            before: file[start.saturating_sub(ANCHOR_CONTEXT)..start].to_vec(),
            lines: file.get(start..=end).map(<[String]>::to_vec).unwrap_or_default(),
            after: file[(end + 1).min(file.len())..(end + 1 + ANCHOR_CONTEXT).min(file.len())].to_vec(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Comment {
    pub id: i64,
    pub thread_id: i64,
    pub author_name: String,
    pub author_email: String,
    /// Markdown.
    pub body: String,
    pub created_at: i64,
    pub updated_at: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Thread {
    pub id: i64,
    /// Stable across machines: the file name when shared in `.reviews/`.
    pub uid: String,
    /// The commit (or range tip) being reviewed when the thread was created.
    pub commit_id: String,
    /// The commit whose tree contains the commented lines (the parent for `Old`).
    pub version_id: String,
    /// Committer time of `version_id`, to tell older versions from newer ones.
    pub version_time: i64,
    pub path: String,
    pub side: ThreadSide,
    /// 0-based, inclusive.
    pub start_line: u32,
    pub end_line: u32,
    pub anchor: Anchor,
    pub status: ThreadStatus,
    pub created_at: i64,
    pub updated_at: i64,
    pub comments: Vec<Comment>,
}

impl Thread {
    /// "12" or "12–14", 1-based.
    pub fn line_label(&self) -> String {
        line_label(self.start_line, self.end_line)
    }
}

pub fn line_label(start: u32, end: u32) -> String {
    if start == end { format!("{}", start + 1) } else { format!("{}–{}", start + 1, end + 1) }
}

/// Everything needed to create a thread with its first comment.
#[derive(Debug, Clone)]
pub struct NewThread {
    pub commit_id: String,
    pub version_id: String,
    pub version_time: i64,
    pub path: String,
    pub side: ThreadSide,
    pub start_line: u32,
    pub end_line: u32,
    pub anchor: Anchor,
    pub author_name: String,
    pub author_email: String,
    pub body: String,
}

#[derive(Debug, Clone, Default)]
pub struct ThreadFilter {
    pub path: Option<String>,
    pub commit_id: Option<String>,
    pub status: Option<ThreadStatus>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn captures_context() {
        let file: Vec<String> = (1..=10).map(|i| format!("l{i}")).collect();
        let a = Anchor::capture(&file, 4, 5);
        assert_eq!(a.before, vec!["l2", "l3", "l4"]);
        assert_eq!(a.lines, vec!["l5", "l6"]);
        assert_eq!(a.after, vec!["l7", "l8", "l9"]);
        let a = Anchor::capture(&file, 0, 0);
        assert!(a.before.is_empty());
        let a = Anchor::capture(&file, 9, 9);
        assert!(a.after.is_empty());
        assert_eq!(a.lines, vec!["l10"]);
    }
}
