//! Settings, recent repositories and per-file review marks, stored in SQLite in the app data dir.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use rusqlite::{Connection, OptionalExtension, params};

pub const DEFAULT_IGNORED_PATHS: &[&str] = &[
    "vendor/",
    "node_modules/",
    "storage/",
    "public/build/",
    "*.lock",
    "package-lock.json",
    "pnpm-lock.yaml",
];

/// Scope for global (not repository-specific) settings.
pub const GLOBAL: &str = "";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepoSettings {
    pub base_branch: String,
    /// Paths whose diffs are collapsed by default. `dir/` matches a directory prefix,
    /// `*.ext` matches a suffix, anything else matches a file name or full path.
    pub ignored_paths: Vec<String>,
    /// Where "Write REVIEW_FEEDBACK.md" writes, relative to the repository root.
    pub feedback_path: String,
    /// Watch the repository and show new commits as they appear.
    pub live_refresh: bool,
    /// The user allows running this project's tools (PHPStan, Pint, the language server), which
    /// execute code from the repository.
    pub trusted: bool,
    /// Clover XML coverage report, relative to the repository root or absolute.
    pub coverage_path: String,
    /// Commands; empty means the project's `vendor/bin`.
    pub phpstan_cmd: String,
    pub pint_cmd: String,
    /// Language server command, run in a snapshot of the commit.
    pub lsp_cmd: String,
    /// Use the language server for go to definition / references (needs `trusted`).
    pub use_lsp: bool,
}

impl Default for RepoSettings {
    fn default() -> Self {
        RepoSettings {
            base_branch: "main".into(),
            ignored_paths: DEFAULT_IGNORED_PATHS.iter().map(|s| s.to_string()).collect(),
            feedback_path: "REVIEW_FEEDBACK.md".into(),
            live_refresh: true,
            trusted: false,
            coverage_path: "coverage.xml".into(),
            phpstan_cmd: String::new(),
            pint_cmd: String::new(),
            lsp_cmd: "phpactor language-server".into(),
            use_lsp: false,
        }
    }
}

impl RepoSettings {
    pub fn is_ignored(&self, path: &str) -> bool {
        self.ignored_paths.iter().any(|p| path_matches(path, p))
    }
}

pub fn path_matches(path: &str, pattern: &str) -> bool {
    let pattern = pattern.trim();
    if pattern.is_empty() {
        return false;
    }
    if let Some(dir) = pattern.strip_suffix('/') {
        return path.starts_with(pattern) || path.contains(&format!("/{dir}/"));
    }
    let name = path.rsplit('/').next().unwrap_or(path);
    if let Some(suffix) = pattern.strip_prefix('*') {
        return name.ends_with(suffix);
    }
    path == pattern || name == pattern
}

pub struct Store {
    pub(crate) conn: Connection,
}

/// Schema migrations, applied in order; `PRAGMA user_version` records how many ran.
const MIGRATIONS: &[&str] = &[
    "CREATE TABLE IF NOT EXISTS settings (
         scope TEXT NOT NULL, key TEXT NOT NULL, value TEXT NOT NULL,
         PRIMARY KEY (scope, key));
     CREATE TABLE IF NOT EXISTS recent_repos (
         path TEXT PRIMARY KEY, opened_at INTEGER NOT NULL);
     CREATE TABLE IF NOT EXISTS reviewed_files (
         repo TEXT NOT NULL, commit_id TEXT NOT NULL, path TEXT NOT NULL,
         PRIMARY KEY (repo, commit_id, path));",
    "CREATE TABLE threads (
         id INTEGER PRIMARY KEY,
         repo TEXT NOT NULL,
         commit_id TEXT NOT NULL,
         version_id TEXT NOT NULL,
         version_time INTEGER NOT NULL,
         path TEXT NOT NULL,
         side TEXT NOT NULL CHECK (side IN ('old', 'new')),
         start_line INTEGER NOT NULL,
         end_line INTEGER NOT NULL,
         anchor_before TEXT NOT NULL,
         anchor_lines TEXT NOT NULL,
         anchor_after TEXT NOT NULL,
         status TEXT NOT NULL CHECK (status IN ('open', 'resolved', 'wont_fix')),
         created_at INTEGER NOT NULL,
         updated_at INTEGER NOT NULL);
     CREATE INDEX threads_by_path ON threads (repo, path);
     CREATE TABLE comments (
         id INTEGER PRIMARY KEY,
         thread_id INTEGER NOT NULL REFERENCES threads (id) ON DELETE CASCADE,
         author_name TEXT NOT NULL,
         author_email TEXT NOT NULL,
         body TEXT NOT NULL,
         created_at INTEGER NOT NULL,
         updated_at INTEGER NOT NULL);
     CREATE INDEX comments_by_thread ON comments (thread_id);",
];

/// Current Unix time in seconds.
pub fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or_default()
}

impl Store {
    /// Open the store in the platform's application data directory.
    pub fn open_default() -> Result<Self> {
        let dirs = directories::ProjectDirs::from("dev", "simple-review", "Simple Review")
            .context("cannot determine the application data directory")?;
        std::fs::create_dir_all(dirs.data_dir())?;
        Self::open(&dirs.data_dir().join("simple-review.sqlite3"))
    }

    pub fn open(path: &Path) -> Result<Self> {
        Self::init(Connection::open(path)?)
    }

    pub fn in_memory() -> Result<Self> {
        Self::init(Connection::open_in_memory()?)
    }

    fn init(mut conn: Connection) -> Result<Self> {
        conn.execute_batch("PRAGMA journal_mode = WAL; PRAGMA foreign_keys = ON;")?;
        let version: usize = conn.query_row("PRAGMA user_version", [], |r| r.get::<_, i64>(0))? as usize;
        for (i, sql) in MIGRATIONS.iter().enumerate().skip(version) {
            let tx = conn.transaction()?;
            tx.execute_batch(sql).with_context(|| format!("database migration {}", i + 1))?;
            tx.pragma_update(None, "user_version", (i + 1) as i64)?;
            tx.commit()?;
        }
        Ok(Store { conn })
    }

    pub fn get(&self, scope: &str, key: &str) -> Result<Option<String>> {
        Ok(self
            .conn
            .query_row("SELECT value FROM settings WHERE scope = ?1 AND key = ?2", params![scope, key], |r| r.get(0))
            .optional()?)
    }

    pub fn set(&self, scope: &str, key: &str, value: &str) -> Result<()> {
        self.conn.execute(
            "INSERT INTO settings (scope, key, value) VALUES (?1, ?2, ?3)
             ON CONFLICT (scope, key) DO UPDATE SET value = excluded.value",
            params![scope, key, value],
        )?;
        Ok(())
    }

    pub fn get_bool(&self, scope: &str, key: &str, default: bool) -> bool {
        match self.get(scope, key) {
            Ok(Some(v)) => v == "1",
            _ => default,
        }
    }

    pub fn set_bool(&self, scope: &str, key: &str, value: bool) -> Result<()> {
        self.set(scope, key, if value { "1" } else { "0" })
    }

    pub fn repo_settings(&self, repo: &Path) -> Result<RepoSettings> {
        let scope = repo.to_string_lossy();
        let mut s = RepoSettings::default();
        if let Some(v) = self.get(&scope, "base_branch")? {
            s.base_branch = v;
        }
        if let Some(v) = self.get(&scope, "ignored_paths")? {
            s.ignored_paths = v.lines().map(str::to_string).filter(|l| !l.trim().is_empty()).collect();
        }
        if let Some(v) = self.get(&scope, "feedback_path")? {
            s.feedback_path = v;
        }
        s.live_refresh = self.get_bool(&scope, "live_refresh", s.live_refresh);
        s.trusted = self.get_bool(&scope, "trusted", s.trusted);
        s.use_lsp = self.get_bool(&scope, "use_lsp", s.use_lsp);
        for (key, field) in [
            ("coverage_path", &mut s.coverage_path),
            ("phpstan_cmd", &mut s.phpstan_cmd),
            ("pint_cmd", &mut s.pint_cmd),
            ("lsp_cmd", &mut s.lsp_cmd),
        ] {
            if let Some(v) = self.get(&scope, key)? {
                *field = v;
            }
        }
        Ok(s)
    }

    pub fn save_repo_settings(&self, repo: &Path, s: &RepoSettings) -> Result<()> {
        let scope = repo.to_string_lossy();
        self.set(&scope, "base_branch", &s.base_branch)?;
        self.set(&scope, "ignored_paths", &s.ignored_paths.join("\n"))?;
        self.set(&scope, "feedback_path", &s.feedback_path)?;
        self.set_bool(&scope, "live_refresh", s.live_refresh)?;
        self.set_bool(&scope, "trusted", s.trusted)?;
        self.set_bool(&scope, "use_lsp", s.use_lsp)?;
        self.set(&scope, "coverage_path", &s.coverage_path)?;
        self.set(&scope, "phpstan_cmd", &s.phpstan_cmd)?;
        self.set(&scope, "pint_cmd", &s.pint_cmd)?;
        self.set(&scope, "lsp_cmd", &s.lsp_cmd)
    }

    /// Commands for the analysis tools.
    pub fn tool_commands(s: &RepoSettings) -> crate::analysis::ToolCommands {
        crate::analysis::ToolCommands { phpstan: s.phpstan_cmd.clone(), pint: s.pint_cmd.clone() }
    }

    pub fn touch_recent(&self, repo: &Path) -> Result<()> {
        let now = now();
        self.conn.execute(
            "INSERT INTO recent_repos (path, opened_at) VALUES (?1, ?2)
             ON CONFLICT (path) DO UPDATE SET opened_at = excluded.opened_at",
            params![repo.to_string_lossy(), now],
        )?;
        Ok(())
    }

    pub fn recent_repos(&self, limit: usize) -> Result<Vec<PathBuf>> {
        let mut stmt = self.conn.prepare("SELECT path FROM recent_repos ORDER BY opened_at DESC, rowid DESC LIMIT ?1")?;
        let rows = stmt.query_map([limit as i64], |r| r.get::<_, String>(0))?;
        Ok(rows.filter_map(|r| r.ok()).map(PathBuf::from).collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ignored_path_patterns() {
        let s = RepoSettings::default();
        assert!(s.is_ignored("vendor/laravel/framework/src/Foo.php"));
        assert!(s.is_ignored("composer.lock"));
        assert!(s.is_ignored("packages/x/node_modules/a.js"));
        assert!(!s.is_ignored("app/Http/Controllers/VendorController.php"));
    }

    #[test]
    fn round_trips() -> Result<()> {
        let store = Store::in_memory()?;
        let repo = Path::new("/tmp/repo");
        let mut s = store.repo_settings(repo)?;
        assert_eq!(s, RepoSettings::default());
        s.base_branch = "develop".into();
        s.ignored_paths = vec!["dist/".into()];
        store.save_repo_settings(repo, &s)?;
        assert_eq!(store.repo_settings(repo)?, s);

        store.touch_recent(Path::new("/a"))?;
        store.touch_recent(Path::new("/b"))?;
        store.touch_recent(Path::new("/a"))?;
        assert_eq!(store.recent_repos(10)?.len(), 2);

        store.set_reviewed(repo, "abc", "x.php", true)?;
        assert!(store.reviewed_files(repo, "abc")?.contains("x.php"));
        store.set_reviewed(repo, "abc", "x.php", false)?;
        assert!(store.reviewed_files(repo, "abc")?.is_empty());

        assert!(store.get_bool(GLOBAL, "flag", true));
        store.set_bool(GLOBAL, "flag", false)?;
        assert!(!store.get_bool(GLOBAL, "flag", true));
        Ok(())
    }
}
