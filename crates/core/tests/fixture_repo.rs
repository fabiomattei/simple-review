//! Tests against the generated fixture repository.

use std::ops::ControlFlow;
use std::sync::Arc;

use review_core::diff::{DiffOptions, LineKind};
use review_core::file_view::{self, FileView, Placeholder};
use review_core::fixture::{self, Fixture};
use review_core::git::{ChangeStatus, CommitInfo, Repo, parse_id};
use review_core::tasks::{CommitPager, PagerEvent};

fn setup() -> (tempfile::TempDir, Fixture, Repo) {
    let dir = tempfile::tempdir().unwrap();
    let fixture = fixture::build(&dir.path().join("repo")).unwrap();
    let repo = Repo::open(&fixture.path).unwrap();
    (dir, fixture, repo)
}

fn commit(repo: &Repo, fx: &Fixture, name: &str) -> CommitInfo {
    repo.commit(parse_id(fx.commit(name)).unwrap()).unwrap()
}

fn changes(repo: &Repo, fx: &Fixture, name: &str) -> Vec<review_core::git::FileChange> {
    let c = commit(repo, fx, name);
    repo.changed_files(c.parents.first().copied(), c.id).unwrap()
}

#[test]
fn lists_commits_newest_first() {
    let (_dir, fx, repo) = setup();
    let head = repo.head_id().unwrap().unwrap();
    assert_eq!(head.to_string(), fx.commit("crlf"));
    let mut all = Vec::new();
    repo.walk_commits(head, |c| {
        all.push(c);
        ControlFlow::Continue(())
    })
    .unwrap();
    assert_eq!(all.len(), fx.main_commit_count());
    assert_eq!(all[0].summary, "Convert routes to CRLF");
    assert_eq!(all.last().unwrap().summary, "Initial Laravel skeleton");
    assert!(all.windows(2).all(|w| w[0].committer.time >= w[1].committer.time));
}

#[test]
fn commit_metadata() {
    let (_dir, fx, repo) = setup();
    let ai = commit(&repo, &fx, "ai");
    assert!(ai.ai_assisted);
    assert_eq!(ai.co_authors, vec!["Claude Opus 5.5 <noreply@anthropic.com>"]);
    assert!(!commit(&repo, &fx, "move").ai_assisted);

    let rebased = commit(&repo, &fx, "rebased");
    assert_eq!(rebased.author.name, "Carol Coder");
    assert_eq!(rebased.committer.name, "Alice Admin");

    let old_laptop = commit(&repo, &fx, "old_laptop");
    assert_eq!((old_laptop.author.name.as_str(), old_laptop.author.email.as_str()), ("Bob Backend", "bob@example.com"));

    let merge = commit(&repo, &fx, "merge");
    assert!(merge.is_merge());
    assert_eq!(merge.parents[1].to_string(), fx.commit("feature_tz"));
}

#[test]
fn pager_delivers_pages_on_request() {
    let (_dir, fx, repo) = setup();
    let head = repo.head_id().unwrap().unwrap();
    let pager = CommitPager::start(repo, head, Arc::new(|| {}));
    pager.request(5);
    let Some(PagerEvent::Batch(first)) = pager.recv() else { panic!("expected a batch") };
    assert_eq!(first.len(), 5);
    pager.request(100);
    let Some(PagerEvent::Batch(rest)) = pager.recv() else { panic!("expected a batch") };
    assert_eq!(first.len() + rest.len(), fx.main_commit_count());
    assert!(matches!(pager.recv(), Some(PagerEvent::Done)));
}

#[test]
fn detects_rename_add_delete() {
    let (_dir, fx, repo) = setup();
    let renamed = changes(&repo, &fx, "rename");
    assert_eq!(renamed.len(), 1);
    assert_eq!(renamed[0].status, ChangeStatus::Renamed);
    assert_eq!(renamed[0].old.as_ref().unwrap().path, "app/Http/Controllers/UserController.php");
    assert_eq!(renamed[0].path(), "app/Http/Controllers/AccountController.php");

    let deleted = changes(&repo, &fx, "delete");
    assert_eq!(deleted.len(), 1);
    assert_eq!(deleted[0].status, ChangeStatus::Deleted);
    assert!(deleted[0].new.is_none());

    let initial = changes(&repo, &fx, "initial");
    assert!(initial.iter().all(|c| c.status == ChangeStatus::Added));
    assert_eq!(initial.len(), 7);
}

#[test]
fn diffs_renamed_file_against_its_source() {
    let (_dir, fx, repo) = setup();
    let change = &changes(&repo, &fx, "rename")[0];
    let FileView::Text { diff, .. } = file_view::load(&repo, change, DiffOptions::default(), false).unwrap() else {
        panic!("expected text")
    };
    assert_eq!((diff.additions, diff.deletions), (1, 1));
}

#[test]
fn binary_and_image_placeholders() {
    let (_dir, fx, repo) = setup();
    let files = changes(&repo, &fx, "invoice");
    let logo = files.iter().find(|c| c.path() == "public/logo.png").unwrap();
    let view = file_view::load(&repo, logo, DiffOptions::default(), false).unwrap();
    assert!(matches!(view, FileView::Placeholder(Placeholder::Image { old_size: None, new_size: Some(16) })));
    let data = files.iter().find(|c| c.path() == "storage/app/data.bin").unwrap();
    let view = file_view::load(&repo, data, DiffOptions::default(), false).unwrap();
    assert!(matches!(view, FileView::Placeholder(Placeholder::Binary { .. })));
}

#[test]
fn moved_block_and_whitespace_only_commit() {
    let (_dir, fx, repo) = setup();
    let change = &changes(&repo, &fx, "move")[0];
    let FileView::Text { diff, .. } = file_view::load(&repo, change, DiffOptions::default(), false).unwrap() else {
        panic!()
    };
    assert!(diff.rows.iter().any(|r| r.left.as_ref().is_some_and(|c| c.kind == LineKind::Moved)));

    let files = changes(&repo, &fx, "pint");
    let opts = DiffOptions { ignore_whitespace: true, intra_line: true };
    for change in &files {
        let FileView::Text { diff, .. } = file_view::load(&repo, change, opts, false).unwrap() else { panic!() };
        // Only blank-line insertions and line splits remain once whitespace is ignored.
        assert!(diff.hunks.len() <= 2, "{}: {:?}", change.path(), diff.hunks);
    }
}

#[test]
fn merge_commit_against_either_parent() {
    let (_dir, fx, repo) = setup();
    let merge = commit(&repo, &fx, "merge");
    let first = repo.changed_files(Some(merge.parents[0]), merge.id).unwrap();
    let paths: Vec<_> = first.iter().map(|c| c.path()).collect();
    assert_eq!(paths, vec!["app/Models/User.php", "config/app.php"]);
    let second = repo.changed_files(Some(merge.parents[1]), merge.id).unwrap();
    let paths: Vec<_> = second.iter().map(|c| c.path()).collect();
    assert!(paths.contains(&"config/app.php"));
    assert!(!paths.contains(&"app/Models/User.php"));
}

#[test]
fn line_ending_change_is_reported_not_diffed() {
    let (_dir, fx, repo) = setup();
    let change = &changes(&repo, &fx, "crlf")[0];
    let FileView::Text { diff, .. } = file_view::load(&repo, change, DiffOptions::default(), false).unwrap() else {
        panic!()
    };
    assert!(diff.hunks.is_empty());
    assert!(diff.line_endings_changed());
}

mod phase2 {
    use super::*;
    use review_core::blame::{self, blame_file};
    use review_core::symbols::{ParseCache, SymbolIndex, SymbolKind};

    fn index(repo: &Repo, commit: review_core::ObjectId) -> SymbolIndex {
        SymbolIndex::build(repo, commit, &|_| true, &ParseCache::default(), &|_| {}, &|| false).unwrap().unwrap()
    }

    fn line_col(text: &str, needle: &str) -> (u32, u32) {
        let (i, line) = text.lines().enumerate().find(|(_, l)| l.contains(needle)).unwrap();
        (i as u32, line.find(needle).unwrap() as u32 + 1)
    }

    fn file_text(repo: &Repo, commit: review_core::ObjectId, path: &str) -> String {
        let f = repo.find_file(commit, path).unwrap().unwrap();
        String::from_utf8(repo.read_blob(f.id).unwrap()).unwrap()
    }

    #[test]
    fn lists_full_tree() {
        let (_dir, fx, repo) = setup();
        let files = repo.list_files(parse_id(fx.commit("move")).unwrap()).unwrap();
        let paths: Vec<_> = files.iter().map(|f| f.path.as_str()).collect();
        assert!(paths.contains(&"app/Services/InvoiceService.php"));
        assert!(paths.contains(&"app/Helpers/legacy.php"));
        assert!(paths.windows(2).all(|w| w[0] < w[1]));
    }

    #[test]
    fn definition_resolves_in_the_version_clicked() {
        let (_dir, fx, repo) = setup();
        let c = commit(&repo, &fx, "move");
        let (old, new) = (index(&repo, c.parents[0]), index(&repo, c.id));
        let path = "app/Services/InvoiceService.php";
        let old_text = file_text(&repo, c.parents[0], path);
        let new_text = file_text(&repo, c.id, path);

        let (l, col) = line_col(&old_text, "round($sum)");
        let left = old.definition_at(path, l, col).unwrap();
        let (l, col) = line_col(&new_text, "round($sum)");
        let right = new.definition_at(path, l, col).unwrap();
        assert_eq!(left.candidates.len(), 1);
        assert_eq!(right.candidates.len(), 1);
        let (ls, rs) = (&left.candidates[0].symbol, &right.candidates[0].symbol);
        assert_eq!(ls.fqn, "App\\Services\\InvoiceService::round");
        assert_eq!(rs.fqn, ls.fqn);
        assert!(old_text.lines().nth(ls.span.line as usize).unwrap().contains("function round"));
        assert!(new_text.lines().nth(rs.span.line as usize).unwrap().contains("function round"));
        assert!(rs.span.line > ls.span.line, "round() moved down in the commit");

        // A method that only exists after the commit.
        let ai = commit(&repo, &fx, "ai");
        let ai_text = file_text(&repo, ai.id, path);
        let (l, col) = line_col(&ai_text, "discount($sum)");
        assert_eq!(index(&repo, ai.id).definition_at(path, l, col).unwrap().candidates.len(), 1);
        assert!(index(&repo, ai.parents[0]).file(path).unwrap().symbols.iter().all(|s| s.name != "discount"));
    }

    #[test]
    fn finds_cross_file_references() {
        let (_dir, fx, repo) = setup();
        let idx = index(&repo, parse_id(fx.commit("ai")).unwrap());
        let path = "app/Services/InvoiceService.php";
        let text = file_text(&repo, idx.commit, path);
        let (l, col) = line_col(&text, "class InvoiceService");
        let class = idx.definition_at(path, l, col + 6).unwrap().candidates.remove(0);
        assert_eq!(class.symbol.kind, SymbolKind::Class);
        let hits = idx.references(&class);
        let paths: Vec<_> = hits.iter().map(|h| h.path.as_str()).collect();
        assert_eq!(paths, vec!["app/Http/Controllers/AccountController.php"; 2], "use + type hint");

        let (l, col) = line_col(&text, "function forUser");
        let for_user = idx.definition_at(path, l, col + 9).unwrap().candidates.remove(0);
        let hits = idx.references(&for_user);
        assert_eq!(hits.len(), 1);
        assert!(hits[0].reference.context.contains("$service->forUser($user)"));
    }

    #[test]
    fn blame_honours_ignore_revs_moves_and_identities() {
        let (_dir, fx, repo) = setup();
        let head = repo.head_id().unwrap().unwrap();
        let ignore = blame::ignore_revs(&repo, head);
        assert_eq!(ignore, vec![parse_id(fx.commit("pint")).unwrap()]);
        let path = "app/Services/InvoiceService.php";
        let pint = parse_id(fx.commit("pint")).unwrap();
        let text = file_text(&repo, pint, path);
        let (sum_line, _) = line_col(&text, "$sum    =  0;");

        let with = blame_file(&repo, pint, path, &ignore).unwrap();
        assert_eq!(with.lines.len(), text.lines().count());
        assert_ne!(with.commit_for_line(sum_line as usize).unwrap().id, pint, "formatting commit is ignored");
        let without = blame_file(&repo, pint, path, &[]).unwrap();
        assert_eq!(without.commit_for_line(sum_line as usize).unwrap().id, pint);

        // round() moved in `move`, but keeps the commit that wrote it.
        let moved = parse_id(fx.commit("move")).unwrap();
        let text = file_text(&repo, moved, path);
        let (round_line, _) = line_col(&text, "$rounded = round(");
        let b = blame_file(&repo, moved, path, &[]).unwrap();
        assert_eq!(b.commit_for_line(round_line as usize).unwrap().id.to_string(), fx.commit("invoice"));

        // Author vs committer, AI flag.
        let ai = parse_id(fx.commit("ai")).unwrap();
        let text = file_text(&repo, ai, path);
        let b = blame_file(&repo, ai, path, &ignore).unwrap();
        let (l, _) = line_col(&text, "discount(float");
        assert!(b.commit_for_line(l as usize).unwrap().ai_assisted);
        let routes = blame_file(&repo, parse_id(fx.commit("rebased")).unwrap(), "routes/web.php", &[]).unwrap();
        let rebased = routes.commits.iter().find(|c| c.id == parse_id(fx.commit("rebased")).unwrap()).unwrap();
        assert_eq!(rebased.author.name, "Carol Coder");
        assert_eq!(rebased.committer.name, "Alice Admin");
        assert!(rebased.committer_differs());

        // .mailmap folds bob@old-laptop.local into Bob Backend.
        let b = blame_file(&repo, parse_id(fx.commit("old_laptop")).unwrap(), "composer.json", &[]).unwrap();
        let c = b.commits.iter().find(|c| c.id == parse_id(fx.commit("old_laptop")).unwrap()).unwrap();
        assert_eq!((c.author.name.as_str(), c.author.email.as_str()), ("Bob Backend", "bob@example.com"));
    }
}

mod phase3 {
    use super::*;
    use review_core::range;
    use review_core::review::export::{HeadLocation, locate_at};
    use review_core::review::{Anchor, Placement, Thread, ThreadSide, ThreadStatus};
    use review_core::watch::RepoWatcher;

    #[test]
    fn resolves_ranges() {
        let (_dir, fx, repo) = setup();
        let r = range::resolve(&repo, "main...topic").unwrap();
        assert_eq!(r.base.to_string(), fx.commit("crlf"));
        assert_eq!(r.tip.to_string(), fx.commit("topic_test"));
        let files = repo.changed_files(Some(r.base), r.tip).unwrap();
        let paths: Vec<_> = files.iter().map(|f| f.path()).collect();
        assert_eq!(paths, vec!["app/Services/InvoiceService.php", "tests/Unit/InvoiceServiceTest.php"]);
        let mut commits = Vec::new();
        repo.walk_range(r.tip, &[r.base], |c| {
            commits.push(c.summary);
            ControlFlow::Continue(())
        })
        .unwrap();
        assert_eq!(commits, vec!["Test invoice totals", "Raise discount threshold"]);

        let two_dot = range::resolve(&repo, &format!("{}..{}", fx.commit("rename"), fx.commit("move"))).unwrap();
        assert_eq!(two_dot.base.to_string(), fx.commit("rename"));
        let single = range::resolve(&repo, "topic").unwrap();
        assert_eq!(single.base.to_string(), fx.commit("topic_discount"));
        assert!(range::resolve(&repo, "nope...topic").is_err());
        assert_eq!(range::base_branch(&repo, "develop").as_deref(), Some("main"));
        assert_eq!(repo.current_branch().as_deref(), Some("main"));
        let names = repo.ref_names().unwrap();
        assert!(names.contains(&"topic".to_string()) && names.contains(&"feature".to_string()));
    }

    #[test]
    fn thread_follows_code_to_head() {
        let (_dir, fx, repo) = setup();
        let path = "app/Services/InvoiceService.php";
        let at_move = parse_id(fx.commit("move")).unwrap();
        let lines: Vec<String> = String::from_utf8(repo.read_blob(repo.find_file(at_move, path).unwrap().unwrap().id).unwrap())
            .unwrap()
            .lines()
            .map(String::from)
            .collect();
        let line = lines.iter().position(|l| l.contains("$sum = 0;")).unwrap() as u32;
        let mut thread = Thread {
            id: 1,
            commit_id: fx.commit("move").into(),
            version_id: fx.commit("move").into(),
            version_time: 0,
            path: path.into(),
            side: ThreadSide::New,
            start_line: line,
            end_line: line,
            anchor: Anchor::capture(&lines, line, line),
            status: ThreadStatus::Open,
            created_at: 0,
            updated_at: 0,
            comments: vec![],
        };
        let head = repo.head_id().unwrap().unwrap();
        // Pint reformatted the line, but `ai` rewrote the file in the original style.
        assert_eq!(locate_at(&repo, head, &thread), HeadLocation::At(Placement::Exact { start: line, end: line }));
        // At `pint` itself the line only matches when whitespace is ignored.
        let pint = parse_id(fx.commit("pint")).unwrap();
        assert_eq!(locate_at(&repo, pint, &thread), HeadLocation::At(Placement::Moved { start: line, end: line }));

        let round = lines.iter().position(|l| l.contains("$rounded = round(")).unwrap() as u32;
        thread.anchor = Anchor::capture(&lines, round, round);
        thread.start_line = round;
        // The discount method was inserted above it in `ai`.
        let HeadLocation::At(Placement::Moved { start, .. }) = locate_at(&repo, head, &thread) else { panic!() };
        assert!(start > round);

        thread.path = "app/Helpers/legacy.php".into();
        assert_eq!(locate_at(&repo, head, &thread), HeadLocation::FileMissing);
    }

    #[test]
    fn watcher_sees_new_commits() {
        let (_dir, fx, repo) = setup();
        let watcher = RepoWatcher::start(&repo.git_dir(), Arc::new(|| {})).unwrap();
        assert!(!watcher.take_changed());
        let status = std::process::Command::new("git")
            .args(["-c", "user.name=T", "-c", "user.email=t@x", "commit", "-q", "--allow-empty", "-m", "live"])
            .current_dir(&fx.path)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .status()
            .unwrap();
        assert!(status.success());
        assert!(watcher.wait(std::time::Duration::from_secs(10)), "no change event");
    }
}

mod phase4 {
    use super::*;
    use review_core::analysis::{self, ToolCommands, complexity, secrets, snapshot, tools::Tool};
    use review_core::diff::DiffOptions;

    fn text_diff(repo: &Repo, change: &review_core::git::FileChange) -> review_core::diff::FileDiff {
        let FileView::Text { diff, .. } = file_view::load(repo, change, DiffOptions::default(), false).unwrap() else { panic!() };
        diff
    }

    #[test]
    fn finds_committed_secrets() {
        let (_dir, fx, repo) = setup();
        let files = changes(&repo, &fx, "leak_secret");
        let mut kinds: Vec<(String, &str)> = Vec::new();
        for f in &files {
            for s in secrets::scan_diff(&text_diff(&repo, f)) {
                kinds.push((f.path().to_string(), s.kind));
            }
        }
        kinds.sort();
        assert_eq!(
            kinds,
            vec![(".env.example".into(), "AWS access key"), ("config/services.php".into(), "Stripe live key")]
        );
        // Nothing in an ordinary commit.
        for f in &changes(&repo, &fx, "ai") {
            assert!(secrets::scan_diff(&text_diff(&repo, f)).is_empty());
        }
    }

    #[test]
    fn complexity_of_a_commit() {
        let (_dir, fx, repo) = setup();
        let change = &changes(&repo, &fx, "ai")[0];
        let diff = text_diff(&repo, change);
        let changed = complexity::compare(&diff.old.lines.join("\n"), &diff.new.lines.join("\n"));
        let names: Vec<_> = changed.iter().map(|c| c.name.as_str()).collect();
        // forUser lost the blank line Pint had added.
        assert_eq!(names, vec!["InvoiceService::discount", "InvoiceService::total", "InvoiceService::forUser"]);
        assert_eq!(changed[0].new.unwrap().branches, 1);
        assert!(changed[0].old.is_none());
        assert_eq!((changed[1].branch_delta(), changed[1].line_delta()), (0, 1));
        assert_eq!((changed[2].branch_delta(), changed[2].line_delta()), (0, -1));
    }

    #[test]
    fn snapshots_are_written_from_git_and_link_vendor() {
        let (dir, fx, repo) = setup();
        std::fs::create_dir_all(fx.path.join("vendor/bin")).unwrap();
        std::fs::write(fx.path.join("vendor/autoload.php"), "<?php\n").unwrap();
        let cache = dir.path().join("cache");
        let id = parse_id(fx.commit("move")).unwrap();
        let snap = snapshot::ensure(&repo, id, &cache, &|| false).unwrap();
        assert!(snap.vendor_linked && !snap.vendor_mismatch);
        let text = std::fs::read_to_string(snap.dir.join("app/Services/InvoiceService.php")).unwrap();
        assert!(text.contains("forUser"));
        assert!(snap.dir.join("app/Helpers/legacy.php").exists(), "deleted only later, in `delete`");
        // Reused, and the repository itself is untouched.
        let again = snapshot::ensure(&repo, id, &cache, &|| false).unwrap();
        assert_eq!(again.dir, snap.dir);
        let status = std::process::Command::new("git").args(["status", "--porcelain"]).current_dir(&fx.path).output().unwrap();
        assert_eq!(String::from_utf8_lossy(&status.stdout).trim(), "?? vendor/");
        snapshot::ensure(&repo, parse_id(fx.commit("ai")).unwrap(), &cache, &|| false).unwrap();
        assert_eq!(snapshot::prune(&cache, &repo, 1).unwrap(), 1);
    }

    /// A stand-in for PHPStan: flags every line calling round() or discount().
    const FAKE_PHPSTAN: &str = r#"#!/bin/sh
first=1
printf '{"totals":{},"files":{'
for f in "$@"; do
  case "$f" in -*|analyse) continue;; esac
  msgs=$(grep -n -e 'round(' -e 'discount(' "$f" | awk -F: '{ printf "%s{\"message\":\"Suspicious call\",\"line\":%d}", (n++ ? "," : ""), $1 }')
  [ -z "$msgs" ] && continue
  [ $first = 1 ] || printf ','
  first=0
  printf '"%s/%s":{"errors":1,"messages":[%s]}' "$PWD" "$f" "$msgs"
done
printf '},"errors":[]}'
"#;

    #[test]
    fn reports_only_warnings_the_commit_introduced() {
        let (dir, fx, repo) = setup();
        let bin = dir.path().join("fake-phpstan");
        std::fs::write(&bin, FAKE_PHPSTAN).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        let ai = commit(&repo, &fx, "ai");
        let files = repo.changed_files(Some(ai.parents[0]), ai.id).unwrap();
        let commands = ToolCommands { phpstan: bin.display().to_string(), pint: "true".into() };
        let result = analysis::analyse_change(&repo, &dir.path().join("cache"), Some(ai.parents[0]), ai.id, &files, &commands, &|_| {}, &|| false).unwrap();
        assert_eq!(result.ran, vec![Tool::PhpStan, Tool::Pint]);
        let lines: Vec<_> = result.introduced.iter().map(|w| (w.path.as_str(), w.line.unwrap())).collect();
        let text = String::from_utf8(repo.read_blob(files[0].new.as_ref().unwrap().id).unwrap()).unwrap();
        let expected: Vec<_> = text
            .lines()
            .enumerate()
            .filter(|(_, l)| l.contains("discount("))
            .map(|(i, _)| ("app/Services/InvoiceService.php", i as u32))
            .collect();
        assert_eq!(lines, expected);
        assert!(result.new_total > result.introduced.len(), "round() warnings exist in both versions");
        assert!(result.notes.iter().any(|n| n.contains("vendor/")), "{:?}", result.notes);
    }

    /// Needs Phpactor: `PHPACTOR="php /path/phpactor.phar" cargo test -- --ignored`.
    #[test]
    #[ignore]
    fn phpactor_goes_to_definition() {
        let Ok(cmd) = std::env::var("PHPACTOR") else { return };
        let (dir, fx, repo) = setup();
        let id = parse_id(fx.commit("move")).unwrap();
        let snap = snapshot::ensure(&repo, id, &dir.path().join("cache"), &|| false).unwrap();
        let mut cmd: Vec<String> = cmd.split_whitespace().map(String::from).collect();
        cmd.push("language-server".into());
        let mut client = review_core::lsp::LspClient::start(&cmd, &snap.dir).unwrap();
        let path = "app/Services/InvoiceService.php";
        let text = std::fs::read_to_string(snap.dir.join(path)).unwrap();
        let (line, l) = text.lines().enumerate().find(|(_, l)| l.contains("$this->round(")).unwrap();
        let col = l.find("round").unwrap() as u32 + 1;
        let defs = client.definition(path, line as u32, col).unwrap();
        let def_line = text.lines().position(|l| l.contains("private function round")).unwrap() as u32;
        assert!(defs.iter().any(|d| d.path.ends_with(path) && d.line == def_line), "{defs:?}");
        client.shutdown();
    }
}
