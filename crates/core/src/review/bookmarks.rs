//! Named places in the code: a file and line at some version, with a note.

use std::path::Path;

use anyhow::Result;
use rusqlite::params;

use super::model::ThreadSide;
use crate::settings::{Store, now};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Bookmark {
    pub id: i64,
    /// The commit (or range tip) that was selected.
    pub commit_id: String,
    /// The commit whose tree contains the line.
    pub version_id: String,
    pub path: String,
    pub side: ThreadSide,
    /// 0-based.
    pub line: u32,
    pub note: String,
    pub created_at: i64,
}

#[derive(Debug, Clone)]
pub struct NewBookmark {
    pub commit_id: String,
    pub version_id: String,
    pub path: String,
    pub side: ThreadSide,
    pub line: u32,
    pub note: String,
}

impl Store {
    pub fn add_bookmark(&self, repo: &Path, b: &NewBookmark) -> Result<i64> {
        self.conn.execute(
            "INSERT INTO bookmarks (repo, commit_id, version_id, path, side, line, note, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![repo.to_string_lossy(), b.commit_id, b.version_id, b.path, b.side.as_str(), b.line, b.note, now()],
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    /// Bookmarks of a repository, newest first.
    pub fn bookmarks(&self, repo: &Path) -> Result<Vec<Bookmark>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, commit_id, version_id, path, side, line, note, created_at
             FROM bookmarks WHERE repo = ?1 ORDER BY created_at DESC, id DESC",
        )?;
        let rows = stmt.query_map([repo.to_string_lossy()], |r| {
            Ok(Bookmark {
                id: r.get(0)?,
                commit_id: r.get(1)?,
                version_id: r.get(2)?,
                path: r.get(3)?,
                side: ThreadSide::parse(&r.get::<_, String>(4)?),
                line: r.get(5)?,
                note: r.get(6)?,
                created_at: r.get(7)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    pub fn rename_bookmark(&self, id: i64, note: &str) -> Result<()> {
        self.conn.execute("UPDATE bookmarks SET note = ?1 WHERE id = ?2", params![note, id])?;
        Ok(())
    }

    pub fn delete_bookmark(&self, id: i64) -> Result<()> {
        self.conn.execute("DELETE FROM bookmarks WHERE id = ?1", [id])?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn add_list_rename_delete() -> Result<()> {
        let store = Store::in_memory()?;
        let repo = Path::new("/r");
        let new = |line| NewBookmark {
            commit_id: "c".into(),
            version_id: "v".into(),
            path: "app/A.php".into(),
            side: ThreadSide::New,
            line,
            note: format!("line {line}"),
        };
        let a = store.add_bookmark(repo, &new(1))?;
        let b = store.add_bookmark(repo, &new(2))?;
        let list = store.bookmarks(repo)?;
        assert_eq!(list.iter().map(|b| b.id).collect::<Vec<_>>(), vec![b, a]);
        store.rename_bookmark(a, "Entry point")?;
        store.delete_bookmark(b)?;
        let list = store.bookmarks(repo)?;
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].note, "Entry point");
        assert!(store.bookmarks(Path::new("/other"))?.is_empty());
        Ok(())
    }
}
