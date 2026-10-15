//! SQLite persistence for threads, comments and per-file review marks.

use std::collections::{HashMap, HashSet};
use std::path::Path;

use anyhow::{Result, bail};
use rusqlite::{OptionalExtension, Row, params};

use super::model::{Anchor, Comment, NewThread, Thread, ThreadFilter, ThreadSide, ThreadStatus};
use crate::settings::{Store, now};

/// Lists of lines are stored as their count, then the lines, newline-separated, so that
/// `[]` and `[""]` stay distinct.
fn encode_lines(lines: &[String]) -> String {
    let mut out = lines.len().to_string();
    for l in lines {
        out.push('\n');
        out.push_str(l);
    }
    out
}

fn decode_lines(s: &str) -> Vec<String> {
    let mut it = s.split('\n');
    let n: usize = it.next().and_then(|c| c.parse().ok()).unwrap_or(0);
    it.take(n).map(str::to_string).collect()
}

const THREAD_COLUMNS: &str = "id, commit_id, version_id, version_time, path, side, start_line, end_line,
     anchor_before, anchor_lines, anchor_after, status, created_at, updated_at";

fn thread_from_row(r: &Row<'_>) -> rusqlite::Result<Thread> {
    Ok(Thread {
        id: r.get(0)?,
        commit_id: r.get(1)?,
        version_id: r.get(2)?,
        version_time: r.get(3)?,
        path: r.get(4)?,
        side: ThreadSide::parse(&r.get::<_, String>(5)?),
        start_line: r.get(6)?,
        end_line: r.get(7)?,
        anchor: Anchor {
            before: decode_lines(&r.get::<_, String>(8)?),
            lines: decode_lines(&r.get::<_, String>(9)?),
            after: decode_lines(&r.get::<_, String>(10)?),
        },
        status: ThreadStatus::parse(&r.get::<_, String>(11)?),
        created_at: r.get(12)?,
        updated_at: r.get(13)?,
        comments: Vec::new(),
    })
}

impl Store {
    pub fn create_thread(&self, repo: &Path, t: &NewThread) -> Result<i64> {
        if t.body.trim().is_empty() {
            bail!("a comment needs some text");
        }
        let now = now();
        self.conn.execute(
            "INSERT INTO threads (repo, commit_id, version_id, version_time, path, side, start_line, end_line,
                                  anchor_before, anchor_lines, anchor_after, status, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, 'open', ?12, ?12)",
            params![
                repo.to_string_lossy(),
                t.commit_id,
                t.version_id,
                t.version_time,
                t.path,
                t.side.as_str(),
                t.start_line,
                t.end_line,
                encode_lines(&t.anchor.before),
                encode_lines(&t.anchor.lines),
                encode_lines(&t.anchor.after),
                now,
            ],
        )?;
        let id = self.conn.last_insert_rowid();
        self.add_comment(id, &t.author_name, &t.author_email, &t.body)?;
        Ok(id)
    }

    pub fn add_comment(&self, thread_id: i64, author_name: &str, author_email: &str, body: &str) -> Result<i64> {
        if body.trim().is_empty() {
            bail!("a comment needs some text");
        }
        let now = now();
        self.conn.execute(
            "INSERT INTO comments (thread_id, author_name, author_email, body, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?5)",
            params![thread_id, author_name, author_email, body, now],
        )?;
        let id = self.conn.last_insert_rowid();
        self.conn.execute("UPDATE threads SET updated_at = ?1 WHERE id = ?2", params![now, thread_id])?;
        Ok(id)
    }

    pub fn edit_comment(&self, comment_id: i64, body: &str) -> Result<()> {
        if body.trim().is_empty() {
            bail!("a comment needs some text");
        }
        self.conn.execute(
            "UPDATE comments SET body = ?1, updated_at = ?2 WHERE id = ?3",
            params![body, now(), comment_id],
        )?;
        Ok(())
    }

    /// Delete a comment; deleting the last comment of a thread deletes the thread.
    pub fn delete_comment(&self, comment_id: i64) -> Result<()> {
        let thread: Option<i64> = self
            .conn
            .query_row("SELECT thread_id FROM comments WHERE id = ?1", [comment_id], |r| r.get(0))
            .optional()?;
        self.conn.execute("DELETE FROM comments WHERE id = ?1", [comment_id])?;
        if let Some(thread) = thread {
            let left: i64 = self.conn.query_row("SELECT COUNT(*) FROM comments WHERE thread_id = ?1", [thread], |r| r.get(0))?;
            if left == 0 {
                self.delete_thread(thread)?;
            }
        }
        Ok(())
    }

    pub fn delete_thread(&self, thread_id: i64) -> Result<()> {
        self.conn.execute("DELETE FROM threads WHERE id = ?1", [thread_id])?;
        Ok(())
    }

    pub fn set_thread_status(&self, thread_id: i64, status: ThreadStatus) -> Result<()> {
        self.conn.execute(
            "UPDATE threads SET status = ?1, updated_at = ?2 WHERE id = ?3",
            params![status.as_str(), now(), thread_id],
        )?;
        Ok(())
    }

    /// Threads of `repo` matching `filter`, oldest first, with their comments.
    pub fn threads(&self, repo: &Path, filter: &ThreadFilter) -> Result<Vec<Thread>> {
        let mut sql = format!("SELECT {THREAD_COLUMNS} FROM threads WHERE repo = ?1");
        let mut args: Vec<String> = vec![repo.to_string_lossy().into_owned()];
        for (column, value) in [
            ("path", filter.path.clone()),
            ("commit_id", filter.commit_id.clone()),
            ("status", filter.status.map(|s| s.as_str().to_string())),
        ] {
            if let Some(v) = value {
                args.push(v);
                sql.push_str(&format!(" AND {column} = ?{}", args.len()));
            }
        }
        sql.push_str(" ORDER BY path, start_line, id");
        let mut stmt = self.conn.prepare(&sql)?;
        let mut threads: Vec<Thread> = stmt
            .query_map(rusqlite::params_from_iter(args.iter()), thread_from_row)?
            .collect::<rusqlite::Result<_>>()?;
        if threads.is_empty() {
            return Ok(threads);
        }

        let index: HashMap<i64, usize> = threads.iter().enumerate().map(|(i, t)| (t.id, i)).collect();
        let mut stmt = self.conn.prepare(
            "SELECT c.id, c.thread_id, c.author_name, c.author_email, c.body, c.created_at, c.updated_at
             FROM comments c JOIN threads t ON t.id = c.thread_id
             WHERE t.repo = ?1 ORDER BY c.created_at, c.id",
        )?;
        let comments = stmt.query_map([repo.to_string_lossy()], |r| {
            Ok(Comment {
                id: r.get(0)?,
                thread_id: r.get(1)?,
                author_name: r.get(2)?,
                author_email: r.get(3)?,
                body: r.get(4)?,
                created_at: r.get(5)?,
                updated_at: r.get(6)?,
            })
        })?;
        for c in comments {
            let c = c?;
            if let Some(&i) = index.get(&c.thread_id) {
                threads[i].comments.push(c);
            }
        }
        Ok(threads)
    }

    /// Files marked reviewed for a commit or range key.
    pub fn reviewed_files(&self, repo: &Path, key: &str) -> Result<HashSet<String>> {
        let mut stmt = self.conn.prepare("SELECT path FROM reviewed_files WHERE repo = ?1 AND commit_id = ?2")?;
        let rows = stmt.query_map(params![repo.to_string_lossy(), key], |r| r.get::<_, String>(0))?;
        Ok(rows.filter_map(|r| r.ok()).collect())
    }

    pub fn set_reviewed(&self, repo: &Path, key: &str, path: &str, reviewed: bool) -> Result<()> {
        let repo = repo.to_string_lossy();
        if reviewed {
            self.conn.execute(
                "INSERT OR IGNORE INTO reviewed_files (repo, commit_id, path) VALUES (?1, ?2, ?3)",
                params![repo, key, path],
            )?;
        } else {
            self.conn.execute(
                "DELETE FROM reviewed_files WHERE repo = ?1 AND commit_id = ?2 AND path = ?3",
                params![repo, key, path],
            )?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn new_thread(path: &str, body: &str) -> NewThread {
        NewThread {
            commit_id: "c1".into(),
            version_id: "c1".into(),
            version_time: 100,
            path: path.into(),
            side: ThreadSide::New,
            start_line: 2,
            end_line: 3,
            anchor: Anchor { before: vec!["".into()], lines: vec!["a".into(), "b".into()], after: vec![] },
            author_name: "Ann".into(),
            author_email: "ann@x".into(),
            body: body.into(),
        }
    }

    #[test]
    fn line_lists_round_trip() {
        for case in [vec![], vec![String::new()], vec!["a".to_string(), String::new(), "c".to_string()]] {
            assert_eq!(decode_lines(&encode_lines(&case)), case);
        }
    }

    #[test]
    fn threads_and_comments() -> Result<()> {
        let store = Store::in_memory()?;
        let repo = Path::new("/r");
        let t1 = store.create_thread(repo, &new_thread("a.php", "Rename this"))?;
        let t2 = store.create_thread(repo, &new_thread("b.php", "Missing test"))?;
        assert!(store.create_thread(repo, &new_thread("c.php", "  ")).is_err());
        store.add_comment(t1, "Bob", "bob@x", "Agreed")?;
        store.set_thread_status(t2, ThreadStatus::Resolved)?;

        let all = store.threads(repo, &ThreadFilter::default())?;
        assert_eq!(all.len(), 2);
        assert_eq!(all[0].comments.len(), 2);
        assert_eq!(all[0].anchor.lines, vec!["a", "b"]);
        assert_eq!(all[0].anchor.before, vec![""]);
        assert_eq!(all[0].side, ThreadSide::New);

        let open = store.threads(repo, &ThreadFilter { status: Some(ThreadStatus::Open), ..Default::default() })?;
        assert_eq!(open.iter().map(|t| t.id).collect::<Vec<_>>(), vec![t1]);
        let b = store.threads(repo, &ThreadFilter { path: Some("b.php".into()), ..Default::default() })?;
        assert_eq!(b[0].status, ThreadStatus::Resolved);
        assert!(store.threads(Path::new("/other"), &ThreadFilter::default())?.is_empty());

        // Deleting the only comment deletes the thread.
        store.delete_comment(b[0].comments[0].id)?;
        assert_eq!(store.threads(repo, &ThreadFilter::default())?.len(), 1);
        Ok(())
    }
}
