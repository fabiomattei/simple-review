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
    assert_eq!(all.len(), fx.commits.len());
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
    assert_eq!(first.len() + rest.len(), fx.commits.len());
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
