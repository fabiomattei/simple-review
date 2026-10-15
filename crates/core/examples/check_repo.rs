//! Run every signal the app computes for one commit and print the results, for checking the
//! tools on a real project:
//!
//!     cargo run -p review-core --example check_repo -- <repo> [commit] [--tools] [--lsp "<cmd>"] [--audit]
//!
//! `--tools` runs PHPStan, Pint and `artisan route:list` (they execute project code), `--lsp`
//! asks a language server for a definition, `--audit` runs `composer audit` (network).

use std::time::Instant;

use anyhow::{Context, Result};
use review_core::analysis::{self, ToolCommands, complexity, secrets, snapshot};
use review_core::diff::DiffOptions;
use review_core::file_view::{self, FileView};
use review_core::git::Repo;
use review_core::laravel::{composer, routes, schema};

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let flag = |f: &str| args.iter().any(|a| a == f);
    let value = |f: &str| args.iter().position(|a| a == f).and_then(|i| args.get(i + 1)).cloned();
    let positional: Vec<&String> = args
        .iter()
        .enumerate()
        .filter(|(i, a)| !a.starts_with("--") && (*i == 0 || args[i - 1] != "--lsp"))
        .map(|(_, a)| a)
        .collect();
    let repo = Repo::open(positional.first().context("usage: check_repo <repo> [commit] [--tools] [--lsp cmd] [--audit]")?)?;
    let commit = match positional.get(1) {
        Some(spec) => repo.resolve(spec)?,
        None => repo.head_id()?.context("no commits")?,
    };
    let info = repo.commit(commit)?;
    let parent = info.parents.first().copied();
    println!("Commit {} {}  (AI-assisted: {})", info.short_id(), info.summary, info.ai_assisted);

    let t = Instant::now();
    let files = repo.changed_files(parent, commit)?;
    println!("\n{} changed files ({:?})", files.len(), t.elapsed());
    for f in &files {
        let FileView::Text { diff, .. } = file_view::load(&repo, f, DiffOptions::default(), false)? else {
            println!("  {} {} (not text)", f.status.letter(), f.path());
            continue;
        };
        println!("  {} {}  +{} -{}", f.status.letter(), f.path(), diff.additions, diff.deletions);
        for s in secrets::scan_diff(&diff) {
            println!("      SECRET line {}: {} {}", s.line + 1, s.kind, s.masked);
        }
        if f.path().ends_with(".php") {
            for c in complexity::compare(&diff.old.lines.join("\n"), &diff.new.lines.join("\n")) {
                let m = |m: Option<complexity::Metrics>| m.map_or("-".into(), |m| format!("{}l/{}b", m.lines, m.branches));
                println!("      complexity {}: {} -> {}", c.name, m(c.old), m(c.new));
            }
        }
    }

    let new_schema = schema::schema_at(&repo, commit)?;
    let old_schema = parent.map(|p| schema::schema_at(&repo, p)).transpose()?.unwrap_or_default();
    println!("\nSchema: {} tables from {} migrations", new_schema.tables.len(), new_schema.migrations.len());
    for change in schema::diff(&old_schema, &new_schema) {
        println!("  {change:?}");
    }

    let new_routes = routes::routes_at(&repo, commit)?;
    let old_routes = parent.map(|p| routes::routes_at(&repo, p)).transpose()?.unwrap_or_default();
    println!("\nRoutes (static): {}", new_routes.len());
    for c in routes::diff(&old_routes, &new_routes) {
        println!("  {c:?}");
    }

    let lock = |id| -> Option<String> {
        let f = repo.find_file(id, "composer.lock").ok()??;
        Some(String::from_utf8_lossy(&repo.read_blob(f.id).ok()?).into_owned())
    };
    if let (Some(old), Some(new)) = (parent.and_then(lock), lock(commit))
        && old != new
    {
        for c in composer::diff_lock(&composer::parse_lock(&old)?, &composer::parse_lock(&new)?) {
            println!("  package {c:?}");
        }
    }

    let cache = snapshot::default_cache_dir()?;
    if flag("--tools") {
        let t = Instant::now();
        let r = analysis::analyse_change(&repo, &cache, parent, commit, &files, &ToolCommands::default(), &|_| {}, &|| false)?;
        println!("\nAnalysis ({:?}): ran {:?}, {} before, {} after, {} new", t.elapsed(), r.ran, r.old_total, r.new_total, r.introduced.len());
        for w in &r.introduced {
            println!("  {} {}:{} {}", w.tool.label(), w.path, w.line.map_or(0, |l| l + 1), w.message);
        }
        for n in &r.notes {
            println!("  note: {n}");
        }
        let snap = snapshot::ensure(&repo, commit, &cache, &|| false)?;
        match routes::artisan_routes(&snap.dir, &|| false) {
            Ok(r) => println!("\nRoutes (artisan): {}", r.len()),
            Err(e) => println!("\nartisan route:list failed: {e:#}"),
        }
    }
    if flag("--audit") {
        let snap = snapshot::ensure(&repo, commit, &cache, &|| false)?;
        match composer::audit(&snap.dir, &|| false) {
            Ok(a) => println!("\ncomposer audit: {} advisories {:?}", a.len(), a.iter().map(|a| &a.package).collect::<Vec<_>>()),
            Err(e) => println!("\ncomposer audit failed: {e:#}"),
        }
    }
    if let Some(cmd) = value("--lsp") {
        let snap = snapshot::ensure(&repo, commit, &cache, &|| false)?;
        let _ = review_core::lsp::generate_ide_helper(&snap.dir);
        let t = Instant::now();
        let cmd: Vec<String> = cmd.split_whitespace().map(String::from).collect();
        let mut client = review_core::lsp::LspClient::start(&cmd, &snap.dir)?;
        println!("\nLanguage server started in {:?}", t.elapsed());
        for f in files.iter().filter(|f| f.path().ends_with(".php")) {
            let text = std::fs::read_to_string(snap.dir.join(f.path()))?;
            // Ask about the first `Foo::` or `->bar(` on a line that has one.
            for (i, line) in text.lines().enumerate() {
                if let Some(col) = line.find("::").map(|c| c.saturating_sub(2)).or_else(|| line.find("->").map(|c| c + 2)) {
                    let t = Instant::now();
                    let defs = client.definition(f.path(), i as u32, col as u32)?;
                    println!("  {}:{} `{}` -> {:?} ({:?})", f.path(), i + 1, line.trim(), defs.iter().map(|d| format!("{}:{}", d.path.display(), d.line + 1)).collect::<Vec<_>>(), t.elapsed());
                }
            }
        }
        client.shutdown();
    }
    Ok(())
}
