//! Share threads with teammates as Markdown files in `.reviews/` (one file per thread), and
//! import them back. SQLite stays the source of truth; files are written only on request.
//!
//! A file looks like:
//!
//! ```text
//! ---
//! uid: 3f2a9c1b7e5d4a60
//! status: open
//! path: app/Services/InvoiceService.php
//! side: new
//! lines: 25-26
//! commit: <reviewed commit>
//! version: <commit containing the lines>
//! version_time: 1704358800
//! created: 1704358900
//! updated: 1704359000
//! anchor: {"before":[...],"lines":[...],"after":[...]}
//! ---
//!
//! (the commented code in a fenced block)
//!
//! ### Ann <ann@example.com> · 2026-10-15 08:16 UTC
//! <!-- comment created=1704358900 updated=1704358900 -->
//!
//! Comment body in Markdown.
//! ```

use std::path::Path;

use anyhow::{Context, Result, bail};
use serde_json::{Value, json};

use super::model::{Anchor, Comment, Thread, ThreadSide, ThreadStatus};
use crate::git::{Signature, format_time};
use crate::settings::Store;

pub const DIR: &str = ".reviews";
const COMMENT_MARK: &str = "<!-- comment ";

fn utc(seconds: i64) -> String {
    let sig = Signature { name: String::new(), email: String::new(), time: seconds, offset: 0 };
    format!("{} UTC", format_time(&sig))
}

fn fence_language(path: &str) -> &str {
    path.rsplit_once('.').map_or("", |(_, e)| e)
}

/// Render a thread as a shareable Markdown file.
pub fn render(t: &Thread) -> String {
    let anchor = json!({ "before": t.anchor.before, "lines": t.anchor.lines, "after": t.anchor.after });
    let mut out = format!(
        "---\nuid: {}\nstatus: {}\npath: {}\nside: {}\nlines: {}-{}\ncommit: {}\nversion: {}\nversion_time: {}\ncreated: {}\nupdated: {}\nanchor: {}\n---\n\n",
        t.uid,
        t.status.as_str(),
        t.path,
        t.side.as_str(),
        t.start_line + 1,
        t.end_line + 1,
        t.commit_id,
        t.version_id,
        t.version_time,
        t.created_at,
        t.updated_at,
        anchor
    );
    let longest = t.anchor.lines.iter().map(|l| l.matches('`').count()).max().unwrap_or(0);
    let fence = "`".repeat((longest + 1).max(3));
    out.push_str(&format!("{fence}{}\n", fence_language(&t.path)));
    for l in &t.anchor.lines {
        out.push_str(l);
        out.push('\n');
    }
    out.push_str(&format!("{fence}\n"));
    for c in &t.comments {
        out.push_str(&format!(
            "\n### {} <{}> · {}\n{COMMENT_MARK}created={} updated={} -->\n\n{}\n",
            c.author_name,
            c.author_email,
            utc(c.created_at),
            c.created_at,
            c.updated_at,
            c.body.trim_end()
        ));
    }
    out
}

/// Parse a file written by [`render`].
pub fn parse(text: &str) -> Result<Thread> {
    let rest = text.strip_prefix("---\n").context("missing front matter")?;
    let (front, body) = rest.split_once("\n---\n").context("unterminated front matter")?;
    let field = |k: &str| -> Option<&str> { front.lines().find_map(|l| l.strip_prefix(k)?.strip_prefix(": ")) };
    let need = |k: &str| field(k).with_context(|| format!("missing `{k}`"));
    let num = |k: &str| -> Result<i64> { need(k)?.trim().parse().with_context(|| format!("bad `{k}`")) };
    let (start, end) = need("lines")?.split_once('-').context("bad `lines`")?;
    let anchor: Value = serde_json::from_str(need("anchor")?).context("bad `anchor`")?;
    let list = |k: &str| -> Vec<String> {
        anchor.get(k).and_then(Value::as_array).into_iter().flatten().filter_map(Value::as_str).map(String::from).collect()
    };
    let uid = need("uid")?.trim().to_string();
    if uid.is_empty() || !uid.chars().all(|c| c.is_ascii_alphanumeric() || c == '-') {
        bail!("bad `uid`");
    }

    // Comments: each starts with a heading line followed by the marker line.
    let mut comments = Vec::new();
    let lines: Vec<&str> = body.lines().collect();
    let mut i = 0;
    while i < lines.len() {
        if let (Some(head), Some(mark)) = (lines[i].strip_prefix("### "), lines.get(i + 1).and_then(|l| l.strip_prefix(COMMENT_MARK)))
        {
            let who = head.rsplit_once(" · ").map_or(head, |(w, _)| w);
            let (name, email) = match who.rsplit_once(" <") {
                Some((n, e)) => (n.to_string(), e.trim_end_matches('>').to_string()),
                None => (who.to_string(), String::new()),
            };
            let stamp = |k: &str| -> i64 {
                mark.split_whitespace().find_map(|p| p.strip_prefix(k)?.parse().ok()).unwrap_or(0)
            };
            let (created, updated) = (stamp("created="), stamp("updated="));
            let mut j = i + 2;
            let mut text = Vec::new();
            while j < lines.len()
                && !(lines[j].starts_with("### ") && lines.get(j + 1).is_some_and(|l| l.starts_with(COMMENT_MARK)))
            {
                text.push(lines[j]);
                j += 1;
            }
            let body = text.join("\n").trim().to_string();
            comments.push(Comment { id: 0, thread_id: 0, author_name: name, author_email: email, body, created_at: created, updated_at: updated });
            i = j;
        } else {
            i += 1;
        }
    }
    let line = |s: &str| -> Result<u32> { Ok(s.trim().parse::<u32>().context("bad `lines`")?.saturating_sub(1)) };
    Ok(Thread {
        id: 0,
        uid,
        commit_id: need("commit")?.trim().to_string(),
        version_id: need("version")?.trim().to_string(),
        version_time: num("version_time")?,
        path: need("path")?.trim().to_string(),
        side: ThreadSide::parse(need("side")?.trim()),
        start_line: line(start)?,
        end_line: line(end)?,
        anchor: Anchor { before: list("before"), lines: list("lines"), after: list("after") },
        status: ThreadStatus::parse(need("status")?.trim()),
        created_at: num("created")?,
        updated_at: num("updated")?,
        comments,
    })
}

/// Write each thread to `<repo>/.reviews/<uid>.md`. Returns how many files were written.
pub fn export(repo_root: &Path, threads: &[Thread]) -> Result<usize> {
    let dir = repo_root.join(DIR);
    std::fs::create_dir_all(&dir)?;
    for t in threads {
        std::fs::write(dir.join(format!("{}.md", t.uid)), render(t))?;
    }
    Ok(threads.len())
}

#[derive(Debug, Default, PartialEq, Eq)]
pub struct ImportReport {
    pub threads_added: usize,
    pub comments_added: usize,
    pub files_skipped: Vec<String>,
}

/// Merge every `.reviews/*.md` file into the store.
pub fn import(store: &Store, repo_root: &Path) -> Result<ImportReport> {
    let mut report = ImportReport::default();
    let Ok(entries) = std::fs::read_dir(repo_root.join(DIR)) else { return Ok(report) };
    let mut paths: Vec<_> = entries.flatten().map(|e| e.path()).filter(|p| p.extension().is_some_and(|e| e == "md")).collect();
    paths.sort();
    for path in paths {
        let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        match std::fs::read_to_string(&path).map_err(anyhow::Error::from).and_then(|t| parse(&t)) {
            Ok(thread) => {
                let (created, added) = store.merge_shared_thread(repo_root, &thread)?;
                report.threads_added += usize::from(created);
                report.comments_added += if created { 0 } else { added };
            }
            Err(e) => report.files_skipped.push(format!("{name}: {e:#}")),
        }
    }
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::review::model::{NewThread, ThreadFilter};

    fn sample_store(repo: &Path) -> (Store, std::path::PathBuf) {
        let store = Store::in_memory().unwrap();
        let repo = repo.to_path_buf();
        let id = store
            .create_thread(
                &repo,
                &NewThread {
                    commit_id: "c0ffee".into(),
                    version_id: "c0ffee".into(),
                    version_time: 1_700_000_000,
                    path: "app/User.php".into(),
                    side: ThreadSide::New,
                    start_line: 4,
                    end_line: 5,
                    anchor: Anchor { before: vec!["{".into()], lines: vec!["    $a = `x`;".into(), "".into()], after: vec![] },
                    author_name: "Ann".into(),
                    author_email: "ann@x".into(),
                    body: "Use a constant.\n\n### Not a heading of ours\n\n- and a list".into(),
                },
            )
            .unwrap();
        store.add_comment(id, "Bob", "bob@x", "Agreed.").unwrap();
        (store, repo)
    }

    #[test]
    fn round_trips() {
        let (store, repo) = sample_store(Path::new("/repo"));
        let t = store.threads(&repo, &ThreadFilter::default()).unwrap().remove(0);
        let text = render(&t);
        assert!(text.contains("```php\n    $a = `x`;\n\n```"));
        let back = parse(&text).unwrap();
        assert_eq!(back.uid, t.uid);
        assert_eq!((back.start_line, back.end_line, back.side), (4, 5, ThreadSide::New));
        assert_eq!(back.anchor, t.anchor);
        assert_eq!(back.comments.len(), 2);
        assert_eq!(back.comments[0].body, t.comments[0].body);
        assert_eq!((back.comments[1].author_name.as_str(), back.comments[1].author_email.as_str()), ("Bob", "bob@x"));
        assert_eq!(back.comments[1].created_at, t.comments[1].created_at);
    }

    #[test]
    fn imports_and_merges() {
        let dir = tempfile::tempdir().unwrap();
        let (mine, my_repo) = sample_store(dir.path());
        let threads = mine.threads(&my_repo, &ThreadFilter::default()).unwrap();
        assert_eq!(export(dir.path(), &threads).unwrap(), 1);

        // A teammate imports into an empty store…
        let theirs = Store::in_memory().unwrap();
        let report = import(&theirs, dir.path()).unwrap();
        assert_eq!(report, ImportReport { threads_added: 1, comments_added: 0, files_skipped: vec![] });
        let mut t = theirs.threads(dir.path(), &ThreadFilter::default()).unwrap().remove(0);
        assert_eq!(t.comments.len(), 2);
        // …replies and resolves, and shares back.
        theirs.add_comment(t.id, "Cy", "cy@x", "Done in abc123.").unwrap();
        std::thread::sleep(std::time::Duration::from_millis(1100));
        theirs.set_thread_status(t.id, ThreadStatus::Resolved).unwrap();
        t = theirs.threads(dir.path(), &ThreadFilter::default()).unwrap().remove(0);
        export(dir.path(), &[t]).unwrap();
        std::fs::write(dir.path().join(DIR).join("junk.md"), "not a thread").unwrap();

        // Importing again merges: one new comment, newer status wins, nothing duplicated.
        let report = import(&mine, dir.path()).unwrap();
        assert_eq!(report.threads_added, 0);
        assert_eq!(report.comments_added, 1);
        assert_eq!(report.files_skipped.len(), 1);
        let merged = mine.threads(dir.path(), &ThreadFilter::default()).unwrap();
        assert_eq!(merged.len(), 1);
        assert_eq!(merged[0].status, ThreadStatus::Resolved);
        assert_eq!(merged[0].comments.len(), 3);
        assert_eq!(import(&mine, dir.path()).unwrap().comments_added, 0, "idempotent");
    }
}
