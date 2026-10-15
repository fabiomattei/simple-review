//! Time the operations the app performs when opening and browsing a repository:
//!
//!     cargo run --release -p review-core --example bench -- <repo>

use std::sync::Arc;
use std::time::Instant;

use anyhow::{Context, Result};
use review_core::diff::DiffOptions;
use review_core::file_view::{self, FileView};
use review_core::git::Repo;
use review_core::highlight::{Highlighter, ThemeKind};
use review_core::symbols::{ParseCache, SymbolIndex};
use review_core::tasks::{CommitPager, PagerEvent};

fn time<T>(label: &str, f: impl FnOnce() -> Result<T>) -> Result<T> {
    let t = Instant::now();
    let r = f()?;
    println!("{label:<48} {:>9.1} ms", t.elapsed().as_secs_f64() * 1000.0);
    Ok(r)
}

fn main() -> Result<()> {
    let path = std::env::args().nth(1).context("usage: bench <repo>")?;
    let _ = Highlighter::global(); // loaded at app start, in the background
    let repo = time("open repository", || Repo::open(&path))?;
    let head = repo.head_id()?.context("no commits")?;

    let pager = CommitPager::start(repo.clone(), head, Arc::new(|| {}));
    let first = time("first page of 300 commits", || {
        pager.request(300);
        match pager.recv() {
            Some(PagerEvent::Batch(b)) => Ok(b),
            other => anyhow::bail!("{other:?}"),
        }
    })?;
    let total = time("walk the rest of the history", || {
        let mut n = first.len();
        loop {
            pager.request(5000);
            match pager.recv() {
                Some(PagerEvent::Batch(b)) => n += b.len(),
                Some(PagerEvent::Done) | None => break,
                Some(PagerEvent::Error(e)) => anyhow::bail!(e),
            }
        }
        Ok(n)
    })?;
    println!("  ({total} commits)");

    let c = &first[0];
    let files = time("changed files of the newest commit", || repo.changed_files(c.parents.first().copied(), c.id))?;
    let change = files.first().context("no changes")?.clone();
    let view = time("first diff (read blobs + diff)", || file_view::load(&repo, &change, DiffOptions { ignore_whitespace: false, intra_line: true }, false))?;
    if let FileView::Text { diff, .. } = &view {
        time("syntax highlighting, new side", || {
            Highlighter::global().highlight(change.path(), &diff.new.lines, ThemeKind::Dark, &|| false).context("cancelled")
        })?;
    }

    let old = &first[150];
    time("changed files + diffs of a commit 150 back", || {
        for f in repo.changed_files(old.parents.first().copied(), old.id)? {
            file_view::load(&repo, &f, DiffOptions::default(), false)?;
        }
        Ok(())
    })?;

    let cache = ParseCache::default();
    let index = time("symbol index at HEAD (cold)", || {
        SymbolIndex::build(&repo, head, &|_| true, &cache, &|_| {}, &|| false)?.context("cancelled")
    })?;
    println!("  ({} PHP files)", index.file_count());
    time("symbol index at HEAD^ (parse cache warm)", || {
        SymbolIndex::build(&repo, c.parents[0], &|_| true, &cache, &|_| {}, &|| false)?.context("cancelled")
    })?;

    time("blame of the changed file", || review_core::blame::blame_file(&repo, head, change.path(), &[]))?;
    time("line history of one line (git log -L)", || review_core::history::line_history(&repo, head, change.path(), 12, 12))?;
    time("pickaxe over the whole history (git log -S)", || review_core::history::pickaxe(&repo, head, "return $x * 3 + 17;", false, None, 200))?;
    time("risk flags for 50 commits", || {
        for c in first.iter().take(50) {
            let files = repo.changed_files(c.parents.first().copied(), c.id)?;
            let _ = review_core::laravel::risk::risks(files.iter().map(|f| f.path()));
        }
        Ok(())
    })?;
    Ok(())
}
