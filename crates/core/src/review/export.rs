//! Markdown exports: feedback for Claude Code, and a review report.

use std::fmt::Write;

use gix::ObjectId;

use super::anchor::{Placement, relocate};
use super::model::{Thread, ThreadStatus, line_label};
use crate::diff::TextFile;
use crate::git::{Repo, Signature, format_time};

/// Where a thread's code is at the tip the feedback is written for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HeadLocation {
    At(Placement),
    FileMissing,
}

/// Find a thread's lines in `path` at `head`.
pub fn locate_at(repo: &Repo, head: ObjectId, thread: &Thread) -> HeadLocation {
    let Ok(Some(file)) = repo.find_file(head, &thread.path) else { return HeadLocation::FileMissing };
    let Ok(bytes) = repo.read_blob(file.id) else { return HeadLocation::FileMissing };
    let text = TextFile::parse(&String::from_utf8_lossy(&bytes));
    HeadLocation::At(relocate(&thread.anchor, thread.start_line, &text.lines))
}

pub struct FeedbackItem {
    pub thread: Thread,
    pub commit_summary: Option<String>,
    pub at_head: HeadLocation,
}

pub struct ExportContext {
    pub repo_name: String,
    /// Short id of the commit locations refer to.
    pub head: String,
    /// Unix seconds.
    pub generated_at: i64,
}

fn utc(seconds: i64) -> String {
    let sig = Signature { name: String::new(), email: String::new(), time: seconds, offset: 0 };
    format!("{} UTC", format_time(&sig))
}

fn fence_language(path: &str) -> &'static str {
    let name = path.rsplit('/').next().unwrap_or(path);
    if name.ends_with(".blade.php") {
        return "blade";
    }
    match name.rsplit_once('.').map(|(_, e)| e) {
        Some("php") => "php",
        Some("js" | "mjs" | "cjs") => "javascript",
        Some("ts") => "typescript",
        Some("vue") => "vue",
        Some("json") => "json",
        Some("yml" | "yaml") => "yaml",
        Some("css") => "css",
        Some("scss") => "scss",
        Some("html") => "html",
        Some("md") => "markdown",
        Some("sh") => "bash",
        Some("sql") => "sql",
        Some("xml") => "xml",
        _ => "",
    }
}

/// A fence long enough not to be closed by backticks inside the code.
fn fence_for(lines: &[String]) -> String {
    let longest = lines
        .iter()
        .map(|l| {
            let mut run = 0;
            let mut best = 0;
            for c in l.chars() {
                run = if c == '`' { run + 1 } else { 0 };
                best = best.max(run);
            }
            best
        })
        .max()
        .unwrap_or(0);
    "`".repeat((longest + 1).max(3))
}

fn code_block(out: &mut String, path: &str, lines: &[String]) {
    let fence = fence_for(lines);
    let _ = writeln!(out, "{fence}{}", fence_language(path));
    for l in lines {
        let _ = writeln!(out, "{l}");
    }
    let _ = writeln!(out, "{fence}");
}

fn quote(out: &mut String, text: &str) {
    for line in text.trim().lines() {
        if line.is_empty() {
            out.push_str(">\n");
        } else {
            let _ = writeln!(out, "> {line}");
        }
    }
}

fn location_text(thread: &Thread, at: HeadLocation) -> String {
    match at {
        HeadLocation::FileMissing => format!("`{}` no longer exists; the code below is from when the comment was made", thread.path),
        HeadLocation::At(Placement::Exact { start, end }) => format!("`{}` lines {}", thread.path, line_label(start, end)),
        HeadLocation::At(Placement::Moved { start, end }) => {
            format!("`{}` lines {} (moved from {})", thread.path, line_label(start, end), thread.line_label())
        }
        HeadLocation::At(Placement::Fuzzy { start, end }) => format!(
            "`{}` around lines {} (the code has changed since the comment)",
            thread.path,
            line_label(start, end)
        ),
        HeadLocation::At(Placement::Outdated) => format!(
            "`{}`: the commented code was not found any more (it was at lines {}); check whether this still applies",
            thread.path,
            thread.line_label()
        ),
    }
}

/// `REVIEW_FEEDBACK.md`: open threads phrased as instructions for Claude Code.
pub fn feedback_markdown(ctx: &ExportContext, items: &[FeedbackItem]) -> String {
    let mut out = String::new();
    let _ = writeln!(out, "# Review feedback\n");
    let _ = writeln!(
        out,
        "Code review of `{}` at `{}`, exported {}. {} open comment{}.\n",
        ctx.repo_name,
        ctx.head,
        utc(ctx.generated_at),
        items.len(),
        if items.len() == 1 { "" } else { "s" }
    );
    out.push_str(
        "## Instructions\n\n\
         Address each item below in the current code. Each item gives the file and lines, the code \
         the reviewer commented on, and what to change. Line numbers refer to the commit above. \
         If an item no longer applies, say so instead of changing code. Keep changes minimal and \
         run the tests afterwards.\n\n",
    );
    if items.is_empty() {
        out.push_str("There are no open review comments.\n");
        return out;
    }
    for (n, item) in items.iter().enumerate() {
        let t = &item.thread;
        let _ = writeln!(out, "## {}. {}\n", n + 1, location_text(t, item.at_head));
        let commit = &t.commit_id[..t.commit_id.len().min(8)];
        let side = match t.side {
            super::model::ThreadSide::Old => " (on the old side of the diff: code the commit removed or replaced)",
            super::model::ThreadSide::New => "",
        };
        match &item.commit_summary {
            Some(summary) => {
                let _ = writeln!(out, "Reviewed in commit `{commit}` \u{201c}{summary}\u{201d}{side}.\n");
            }
            None => {
                let _ = writeln!(out, "Reviewed in `{commit}`{side}.\n");
            }
        }
        code_block(&mut out, &t.path, &t.anchor.lines);
        out.push('\n');
        let mut comments = t.comments.iter();
        if let Some(first) = comments.next() {
            let _ = writeln!(out, "**Requested change** ({}):\n", first.author_name);
            quote(&mut out, &first.body);
            out.push('\n');
        }
        for c in comments {
            let _ = writeln!(out, "**Follow-up** ({}):\n", c.author_name);
            quote(&mut out, &c.body);
            out.push('\n');
        }
    }
    out
}

pub struct ReportFile {
    pub path: String,
    pub status: char,
    pub reviewed: bool,
}

pub struct Report {
    /// e.g. "Commit 1a2b3c4d: Add discount" or "Range main...feature".
    pub title: String,
    pub files: Vec<ReportFile>,
    pub threads: Vec<Thread>,
}

/// A human-readable summary of a review: files, progress and every thread.
pub fn report_markdown(ctx: &ExportContext, r: &Report) -> String {
    let mut out = String::new();
    let _ = writeln!(out, "# Review: {}\n", r.title);
    let reviewed = r.files.iter().filter(|f| f.reviewed).count();
    let open = r.threads.iter().filter(|t| t.status == ThreadStatus::Open).count();
    let _ = writeln!(
        out,
        "Repository `{}` · exported {} · {reviewed}/{} files reviewed · {} comment thread{} ({open} open)\n",
        ctx.repo_name,
        utc(ctx.generated_at),
        r.files.len(),
        r.threads.len(),
        if r.threads.len() == 1 { "" } else { "s" },
    );
    out.push_str("## Files\n\n");
    for f in &r.files {
        let _ = writeln!(out, "- [{}] `{}` {}", if f.reviewed { "x" } else { " " }, f.status, f.path);
    }
    out.push('\n');
    if r.threads.is_empty() {
        return out;
    }
    out.push_str("## Comments\n");
    let mut current_path = None;
    for t in &r.threads {
        if current_path != Some(&t.path) {
            let _ = writeln!(out, "\n### `{}`\n", t.path);
            current_path = Some(&t.path);
        }
        let _ = writeln!(out, "#### Lines {} ({} side) — {}\n", t.line_label(), t.side.as_str(), t.status.label());
        code_block(&mut out, &t.path, &t.anchor.lines);
        out.push('\n');
        for c in &t.comments {
            let _ = writeln!(out, "**{}** · {}\n", c.author_name, utc(c.created_at));
            quote(&mut out, &c.body);
            out.push('\n');
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::review::model::{Anchor, Comment, ThreadSide};

    fn thread() -> Thread {
        Thread {
            id: 1,
            uid: "0123456789abcdef".into(),
            commit_id: "1234567890abcdef".into(),
            version_id: "1234567890abcdef".into(),
            version_time: 0,
            path: "app/User.php".into(),
            side: ThreadSide::New,
            start_line: 9,
            end_line: 10,
            anchor: Anchor { before: vec![], lines: vec!["$a = 1;".into(), "$b = `x`;".into()], after: vec![] },
            status: ThreadStatus::Open,
            created_at: 1_700_000_000,
            updated_at: 1_700_000_000,
            comments: vec![
                Comment {
                    id: 1,
                    thread_id: 1,
                    author_name: "Ann".into(),
                    author_email: "ann@x".into(),
                    body: "Use a constant.\n\nAnd name it well.".into(),
                    created_at: 1_700_000_000,
                    updated_at: 1_700_000_000,
                },
                Comment {
                    id: 2,
                    thread_id: 1,
                    author_name: "Bob".into(),
                    author_email: "bob@x".into(),
                    body: "Also add a test.".into(),
                    created_at: 1_700_000_100,
                    updated_at: 1_700_000_100,
                },
            ],
        }
    }

    fn ctx() -> ExportContext {
        ExportContext { repo_name: "shop".into(), head: "abcdef12".into(), generated_at: 1_700_000_000 }
    }

    #[test]
    fn feedback_document() {
        let item = FeedbackItem {
            thread: thread(),
            commit_summary: Some("Add totals".into()),
            at_head: HeadLocation::At(Placement::Moved { start: 11, end: 12 }),
        };
        let md = feedback_markdown(&ctx(), &[item]);
        let expected = "# Review feedback

Code review of `shop` at `abcdef12`, exported 2023-11-14 22:13 UTC. 1 open comment.

## Instructions

Address each item below in the current code. Each item gives the file and lines, the code the reviewer commented on, and what to change. Line numbers refer to the commit above. If an item no longer applies, say so instead of changing code. Keep changes minimal and run the tests afterwards.

## 1. `app/User.php` lines 12–13 (moved from 10–11)

Reviewed in commit `12345678` \u{201c}Add totals\u{201d}.

```php
$a = 1;
$b = `x`;
```

**Requested change** (Ann):

> Use a constant.
>
> And name it well.

**Follow-up** (Bob):

> Also add a test.

";
        assert_eq!(md, expected);
    }

    #[test]
    fn outdated_and_missing_items_say_so() {
        let mut t = thread();
        t.side = ThreadSide::Old;
        let md = feedback_markdown(
            &ctx(),
            &[
                FeedbackItem { thread: t.clone(), commit_summary: None, at_head: HeadLocation::At(Placement::Outdated) },
                FeedbackItem { thread: t, commit_summary: None, at_head: HeadLocation::FileMissing },
            ],
        );
        assert!(md.contains("the commented code was not found any more (it was at lines 10–11)"));
        assert!(md.contains("`app/User.php` no longer exists"));
        assert!(md.contains("on the old side of the diff"));
        assert!(md.contains("2 open comments"));
    }

    #[test]
    fn fences_survive_backticks() {
        assert_eq!(fence_for(&["a ``` b".into()]), "````");
        assert_eq!(fence_for(&["plain".into()]), "```");
    }

    #[test]
    fn report_lists_files_and_threads() {
        let report = Report {
            title: "Commit 12345678: Add totals".into(),
            files: vec![
                ReportFile { path: "app/User.php".into(), status: 'M', reviewed: true },
                ReportFile { path: "routes/web.php".into(), status: 'A', reviewed: false },
            ],
            threads: vec![thread()],
        };
        let md = report_markdown(&ctx(), &report);
        assert!(md.starts_with("# Review: Commit 12345678: Add totals\n"));
        assert!(md.contains("1/2 files reviewed · 1 comment thread (1 open)"));
        assert!(md.contains("- [x] `M` app/User.php\n- [ ] `A` routes/web.php"));
        assert!(md.contains("#### Lines 10–11 (new side) — Open"));
        assert!(md.contains("**Bob** · 2023-11-14 22:15 UTC"));
    }
}
