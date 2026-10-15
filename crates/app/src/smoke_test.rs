//! End-to-end smoke test: drive the real UI against the fixture repository.
//!
//! Set `SNAPSHOT_DIR=<dir>` to also save rendered screenshots there.

use std::time::{Duration, Instant};

use egui::Key;
use egui_kittest::Harness;
use review_core::diff::LineKind;
use review_core::file_view::FileView;
use review_core::fixture;
use review_core::settings::Store;

use super::ReviewApp;
use review_core::git::parse_id;

use crate::state::{Cursor, DefinitionState, FilesState, PeekTab, ReferencesState, Side, ViewState};
use crate::ui::Action;

/// Step the UI until `done` holds, giving background threads time to finish.
fn wait_until(harness: &mut Harness<'_, ReviewApp>, what: &str, done: impl Fn(&ReviewApp) -> bool) {
    let deadline = Instant::now() + Duration::from_secs(20);
    while !done(harness.state()) {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        harness.step();
        std::thread::sleep(Duration::from_millis(5));
    }
    harness.run_steps(2);
}

fn view_ready(app: &ReviewApp) -> bool {
    app.rs
        .as_ref()
        .and_then(|rs| rs.commit.as_ref())
        .and_then(|c| c.file.as_ref())
        .is_some_and(|f| matches!(f.view, ViewState::Ready(_)))
}

fn current_path(app: &ReviewApp) -> Option<String> {
    let f = app.rs.as_ref()?.commit.as_ref()?.file.as_ref()?;
    Some(f.change.path().to_string())
}

fn current_summary(app: &ReviewApp) -> Option<String> {
    Some(app.rs.as_ref()?.commit.as_ref()?.info.summary.clone())
}

fn snapshot(harness: &mut Harness<'_, ReviewApp>, name: &str) {
    let Some(dir) = std::env::var_os("SNAPSHOT_DIR").filter(|d| !d.is_empty()) else { return };
    let image = harness.render().expect("render");
    image.save(std::path::Path::new(&dir).join(format!("{name}.png"))).expect("save snapshot");
}

#[test]
fn browse_commits_and_navigate_diffs() {
    let tmp = tempfile::tempdir().unwrap();
    let fx = fixture::build(&tmp.path().join("repo")).unwrap();
    let path = fx.path.clone();
    let mut harness = Harness::builder()
        .with_size(egui::vec2(1400.0, 860.0))
        .wgpu()
        .build_eframe(move |cc| {
            cc.egui_ctx.set_visuals(egui::Visuals::dark());
            ReviewApp::with_store(cc, Store::in_memory(), Some(path))
        });

    // Opening selects the newest commit and its first file.
    wait_until(&mut harness, "first diff", view_ready);
    assert_eq!(current_summary(harness.state()).as_deref(), Some("Convert routes to CRLF"));
    assert_eq!(current_path(harness.state()).as_deref(), Some("routes/web.php"));
    assert_eq!(harness.state().rs.as_ref().unwrap().commits.len(), fx.main_commit_count());

    // Jump to the commit that moved a block, and check the moved rows reach the view.
    let idx = harness.state().rs.as_ref().unwrap().commits.iter().position(|c| c.summary.starts_with("Move round()")).unwrap();
    harness.state_mut().apply(Action::SelectCommit(idx));
    wait_until(&mut harness, "move diff", view_ready);
    {
        let sf = harness.state().rs.as_ref().unwrap().commit.as_ref().unwrap().file.as_ref().unwrap();
        let ViewState::Ready(view) = &sf.view else { unreachable!() };
        let FileView::Text { diff, .. } = view.as_ref() else { panic!("expected text") };
        assert!(diff.rows.iter().any(|r| r.right.as_ref().is_some_and(|c| c.kind == LineKind::Moved)));
        assert_eq!(sf.current_hunk, Some(0));
    }
    // Let highlighting land before the screenshot.
    wait_until(&mut harness, "highlighting", |app| app.hl_pending.is_empty());
    snapshot(&mut harness, "split_moved_block");

    // Keyboard: next change, unified toggle.
    let hunks = harness.state().rs.as_ref().unwrap().commit.as_ref().unwrap().file.as_ref().unwrap().hunk_count();
    harness.key_press(Key::N);
    harness.run_steps(2);
    let cur = harness.state().rs.as_ref().unwrap().commit.as_ref().unwrap().file.as_ref().unwrap().current_hunk;
    assert_eq!(cur, Some(1.min(hunks - 1)));
    harness.key_press(Key::U);
    harness.run_steps(2);
    assert!(harness.state().unified);
    snapshot(&mut harness, "unified_moved_block");
    harness.key_press(Key::U);

    // j moves to the next (older) commit; Alt+← goes back.
    harness.key_press(Key::J);
    wait_until(&mut harness, "older commit", view_ready);
    assert_eq!(current_summary(harness.state()).as_deref(), Some("Rename UserController to AccountController"));
    harness.key_press_modifiers(egui::Modifiers::ALT, Key::ArrowLeft);
    wait_until(&mut harness, "back", |app| view_ready(app) && current_summary(app).as_deref() == Some("Move round() below forUser()"));

    // Commit with several files: ] moves to the next file, r marks it reviewed.
    let idx = harness.state().rs.as_ref().unwrap().commits.iter().position(|c| c.summary.starts_with("Add invoice service")).unwrap();
    harness.state_mut().apply(Action::SelectCommit(idx));
    wait_until(&mut harness, "invoice files", |app| {
        app.rs.as_ref().unwrap().commit.as_ref().is_some_and(|c| matches!(c.files, FilesState::Ready(_)))
            && view_ready(app)
    });
    assert_eq!(current_path(harness.state()).as_deref(), Some("app/Http/Controllers/UserController.php"));
    harness.key_press(Key::R);
    harness.key_press(Key::CloseBracket);
    wait_until(&mut harness, "invoice service", view_ready);
    assert_eq!(current_path(harness.state()).as_deref(), Some("app/Services/InvoiceService.php"));
    harness.key_press(Key::CloseBracket);
    wait_until(&mut harness, "logo placeholder", view_ready);
    assert_eq!(current_path(harness.state()).as_deref(), Some("public/logo.png"));
    let sc = harness.state().rs.as_ref().unwrap().commit.as_ref().unwrap();
    assert!(sc.reviewed.contains("app/Http/Controllers/UserController.php"));
    snapshot(&mut harness, "image_placeholder");

    // storage/ is an ignored path: collapsed until Enter.
    harness.key_press(Key::CloseBracket);
    harness.run_steps(2);
    assert_eq!(current_path(harness.state()).as_deref(), Some("storage/app/data.bin"));
    assert!(matches!(
        harness.state().rs.as_ref().unwrap().commit.as_ref().unwrap().file.as_ref().unwrap().view,
        ViewState::Collapsed(_)
    ));
    harness.key_press(Key::Enter);
    wait_until(&mut harness, "expanded binary", view_ready);

    // Merge commit: compare against the second parent with m.
    let idx = harness.state().rs.as_ref().unwrap().commits.iter().position(|c| c.is_merge()).unwrap();
    harness.state_mut().apply(Action::SelectCommit(idx));
    wait_until(&mut harness, "merge", view_ready);
    harness.key_press(Key::M);
    wait_until(&mut harness, "second parent", |app| {
        view_ready(app) && app.rs.as_ref().unwrap().commit.as_ref().unwrap().parent_idx == 1
    });
    assert_eq!(current_path(harness.state()).as_deref(), Some("config/app.php"));
    snapshot(&mut harness, "merge_second_parent");
}

fn line_of(app: &ReviewApp, side: Side, needle: &str) -> (u32, u32) {
    let sf = app.rs.as_ref().unwrap().commit.as_ref().unwrap().file.as_ref().unwrap();
    let ViewState::Ready(view) = &sf.view else { panic!("view not ready") };
    let FileView::Text { diff, .. } = view.as_ref() else { panic!("expected text") };
    let lines = if side == Side::Old { &diff.old.lines } else { &diff.new.lines };
    let (i, l) = lines.iter().enumerate().find(|(_, l)| l.contains(needle)).expect("needle");
    (i as u32, l.find(needle).unwrap() as u32 + 1)
}

fn definition_line(app: &ReviewApp) -> Option<(String, u32)> {
    let def = app.peek.definition.as_ref()?;
    let DefinitionState::Ready { lookup, selected } = &def.state else { return None };
    let c = &lookup.candidates[*selected];
    Some((c.path.clone(), c.symbol.span.line))
}

#[test]
fn definitions_references_blame_and_tree() {
    let tmp = tempfile::tempdir().unwrap();
    let fx = fixture::build(&tmp.path().join("repo")).unwrap();
    let path = fx.path.clone();
    let mut harness = Harness::builder()
        .with_size(egui::vec2(1400.0, 900.0))
        .wgpu()
        .build_eframe(move |cc| {
            cc.egui_ctx.set_visuals(egui::Visuals::dark());
            ReviewApp::with_store(cc, Store::in_memory(), Some(path))
        });
    wait_until(&mut harness, "first diff", view_ready);
    let idx = harness.state().rs.as_ref().unwrap().commits.iter().position(|c| c.summary.starts_with("Move round()")).unwrap();
    harness.state_mut().apply(Action::SelectCommit(idx));
    wait_until(&mut harness, "move diff", view_ready);

    // Go to definition of `round` from each pane resolves in that pane's version.
    let mut lines = Vec::new();
    for side in [Side::Old, Side::New] {
        let (line, col) = line_of(harness.state(), side, "round($sum)");
        harness.state_mut().apply(Action::SetCursor(Cursor { side, line, col }));
        harness.state_mut().apply(Action::GoToDefinition);
        wait_until(&mut harness, "definition", |app| definition_line(app).is_some());
        let (def_path, def_line) = definition_line(harness.state()).unwrap();
        assert_eq!(def_path, "app/Services/InvoiceService.php");
        let (expected, _) = line_of(harness.state(), side, "private function round");
        assert_eq!(def_line, expected, "{side:?}");
        lines.push(def_line);
    }
    assert!(lines[1] > lines[0]);
    snapshot(&mut harness, "peek_definition");

    // `o` jumps to the definition: the cursor lands on that line of the new side.
    harness.key_press(Key::O);
    harness.run_steps(2);
    let cursor = harness.state().rs.as_ref().unwrap().commit.as_ref().unwrap().file.as_ref().unwrap().cursor.unwrap();
    assert_eq!((cursor.side, cursor.line), (Side::New, lines[1]));

    // References to forUser() cross into the controller.
    let (line, col) = line_of(harness.state(), Side::New, "function forUser");
    harness.state_mut().apply(Action::SetCursor(Cursor { side: Side::New, line, col: col + 9 }));
    harness.key_press_modifiers(egui::Modifiers::SHIFT, Key::F12);
    wait_until(&mut harness, "references", |app| {
        matches!(app.peek.references.as_ref().map(|r| &r.state), Some(ReferencesState::Ready { .. }))
    });
    let Some(ReferencesState::Ready { hits, .. }) = harness.state().peek.references.as_ref().map(|r| &r.state) else { unreachable!() };
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].path, "app/Http/Controllers/AccountController.php");
    let ref_line = hits[0].reference.span.line;

    // Opening the reference shows the unchanged controller as a whole file at this commit.
    harness.state_mut().apply(Action::OpenReference(0));
    wait_until(&mut harness, "browse view", view_ready);
    {
        let sf = harness.state().rs.as_ref().unwrap().commit.as_ref().unwrap().file.as_ref().unwrap();
        assert!(sf.browse.is_some());
        assert_eq!(sf.change.path(), "app/Http/Controllers/AccountController.php");
        assert!(sf.cursor.is_some_and(|c| c.line == ref_line));
    }
    // Back returns to the diff.
    harness.key_press_modifiers(egui::Modifiers::ALT, Key::ArrowLeft);
    wait_until(&mut harness, "back to diff", |app| {
        view_ready(app) && app.rs.as_ref().unwrap().commit.as_ref().unwrap().file.as_ref().unwrap().browse.is_none()
    });

    // Blame gutter: both sides get blamed; moved lines keep their original commit.
    harness.key_press(Key::B);
    wait_until(&mut harness, "blame", |app| app.blame_pending.is_empty() && app.blames.len() >= 2);
    let (line, _) = line_of(harness.state(), Side::New, "$rounded = round(");
    let key = (parse_id(fx.commit("move")).unwrap(), "app/Services/InvoiceService.php".to_string());
    let blame = harness.state_mut().blames.get(&key).unwrap();
    let blame = blame.as_ref().as_ref().unwrap();
    assert_eq!(blame.commit_for_line(line as usize).unwrap().id.to_string(), fx.commit("invoice"));
    harness.state_mut().apply(Action::SetCursor(Cursor { side: Side::New, line, col: 0 }));
    harness.state_mut().apply(Action::PeekTab(PeekTab::Blame));
    wait_until(&mut harness, "highlighting", |app| app.hl_pending.is_empty());
    snapshot(&mut harness, "blame_gutter");

    // All files: open an unchanged file from the tree.
    harness.key_press(Key::T);
    wait_until(&mut harness, "tree", |app| app.rs.as_ref().unwrap().commit.as_ref().unwrap().tree.is_some());
    harness.state_mut().apply(Action::OpenTreeFile("app/Models/User.php".into()));
    wait_until(&mut harness, "browse", view_ready);
    assert!(harness.state().rs.as_ref().unwrap().commit.as_ref().unwrap().file.as_ref().unwrap().browse.is_some());
    snapshot(&mut harness, "all_files_browse");
}

fn file_ready(app: &ReviewApp, path: &str) -> bool {
    view_ready(app) && current_path(app).as_deref() == Some(path)
}

fn select_summary(harness: &mut Harness<'_, ReviewApp>, prefix: &str) {
    let idx = harness.state().rs.as_ref().unwrap().commits.iter().position(|c| c.summary.starts_with(prefix)).unwrap();
    harness.state_mut().apply(Action::SelectCommit(idx));
}

#[test]
fn comments_export_ranges_and_live_refresh() {
    use review_core::review::{Placement, ThreadStatus};

    let tmp = tempfile::tempdir().unwrap();
    let fx = fixture::build(&tmp.path().join("repo")).unwrap();
    let path = fx.path.clone();
    let mut harness = Harness::builder()
        .with_size(egui::vec2(1400.0, 900.0))
        .wgpu()
        .build_eframe(move |cc| {
            cc.egui_ctx.set_visuals(egui::Visuals::dark());
            ReviewApp::with_store(cc, Store::in_memory(), Some(path))
        });
    let invoice = "app/Services/InvoiceService.php";
    wait_until(&mut harness, "first diff", view_ready);
    select_summary(&mut harness, "Move round()");
    wait_until(&mut harness, "move diff", |app| file_ready(app, invoice));

    // Select two lines on the new side and comment on them.
    let (line, _) = line_of(harness.state(), Side::New, "$rounded = round(");
    harness.state_mut().apply(Action::SetCursor(Cursor { side: Side::New, line, col: 0 }));
    harness.state_mut().apply(Action::SelectTo(Cursor { side: Side::New, line: line + 1, col: 0 }));
    harness.key_press(Key::C);
    harness.run_steps(2);
    let editor = harness.state_mut().comment_editor.as_mut().expect("editor opened");
    assert!(editor.context.contains("lines"));
    editor.text = "Extract the **rounding** into a helper.".into();
    harness.state_mut().apply(Action::SaveComment);
    harness.run_steps(3);
    let thread_id = {
        let app = harness.state();
        let rs = app.rs.as_ref().unwrap();
        assert_eq!(rs.threads.len(), 1);
        let t = &rs.threads[0];
        assert_eq!((t.start_line, t.end_line, t.path.as_str()), (line, line + 1, invoice));
        assert_eq!(app.thread_marks().len(), 1);
        t.id
    };
    harness.state_mut().apply(Action::ReplyTo(thread_id));
    harness.state_mut().comment_editor.as_mut().unwrap().text = "And cover it with a test.".into();
    harness.state_mut().apply(Action::SaveComment);
    harness.run_steps(3);
    assert_eq!(harness.state().rs.as_ref().unwrap().threads[0].comments.len(), 2);
    wait_until(&mut harness, "highlighting", |app| app.hl_pending.is_empty());
    snapshot(&mut harness, "comment_thread");

    // At a later commit the thread follows the code (a method was inserted above it).
    select_summary(&mut harness, "Add volume discount");
    wait_until(&mut harness, "ai diff", |app| file_ready(app, invoice));
    harness.run_steps(2);
    {
        let sf = harness.state().rs.as_ref().unwrap().commit.as_ref().unwrap().file.as_ref().unwrap();
        let (_, placed) = sf.placed.as_ref().expect("placed");
        assert_eq!(placed.len(), 1);
        let Placement::Moved { start, .. } = placed[0].placement else { panic!("{:?}", placed[0].placement) };
        let (now, _) = line_of(harness.state(), Side::New, "$rounded = round(");
        assert_eq!(start, now);
    }

    // Export for Claude Code.
    harness.state_mut().apply(Action::WriteFeedback);
    let feedback = fx.path.join("REVIEW_FEEDBACK.md");
    wait_until(&mut harness, "feedback file", |_| feedback.exists());
    let text = std::fs::read_to_string(&feedback).unwrap();
    assert!(text.contains("1 open comment"), "{text}");
    assert!(text.contains(&format!("`{invoice}` lines")), "{text}");
    assert!(text.contains("> Extract the **rounding** into a helper."));
    assert!(text.contains("**Follow-up**"));
    std::fs::remove_file(&feedback).unwrap();

    // Resolve: no longer exported, still listed under "Closed".
    harness.state_mut().apply(Action::SetThreadStatus(thread_id, ThreadStatus::Resolved));
    assert_eq!(harness.state().rs.as_ref().unwrap().threads[0].status, ThreadStatus::Resolved);
    assert!(harness.state().comments_view().rows.is_empty());
    harness.state_mut().apply(Action::CommentStatusFilter(crate::state::StatusFilter::Closed));
    assert_eq!(harness.state().comments_view().rows.len(), 1);

    // Range review: main...topic.
    harness.state_mut().apply(Action::CompareRange("main...topic".into()));
    wait_until(&mut harness, "range diff", |app| {
        let rs = app.rs.as_ref().unwrap();
        rs.range.is_some()
            && rs.commits.len() == 2
            && rs.commit.as_ref().is_some_and(|c| c.base.is_some() && matches!(c.files, FilesState::Ready(ref f) if f.len() == 2))
            && view_ready(app)
    });
    harness.key_press(Key::R);
    harness.run_steps(1);
    {
        let sc = harness.state().rs.as_ref().unwrap().commit.as_ref().unwrap();
        assert_eq!(sc.review_key(), format!("{}..{}", fx.commit("crlf"), fx.commit("topic_test")));
        assert_eq!(sc.reviewed.len(), 1);
    }
    snapshot(&mut harness, "range_review");
    harness.state_mut().apply(Action::ExitRange);
    wait_until(&mut harness, "full history", |app| app.rs.as_ref().unwrap().commits.len() == fx.main_commit_count());

    // Live refresh: a commit made outside the app shows up.
    let status = std::process::Command::new("git")
        .args(["-c", "user.name=Claude", "-c", "user.email=c@x", "commit", "-q", "--allow-empty", "-m", "Made by Claude Code"])
        .current_dir(&fx.path)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .status()
        .unwrap();
    assert!(status.success());
    wait_until(&mut harness, "live refresh", |app| {
        app.rs.as_ref().unwrap().commits.first().is_some_and(|c| c.summary == "Made by Claude Code")
    });
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
fn quality_signals() {
    let tmp = tempfile::tempdir().unwrap();
    let fx = fixture::build(&tmp.path().join("repo")).unwrap();
    let invoice = "app/Services/InvoiceService.php";

    // Coverage report (in the working copy, untracked): the discount call is never run.
    let repo = review_core::git::Repo::open(&fx.path).unwrap();
    let ai = parse_id(fx.commit("ai")).unwrap();
    let text = String::from_utf8(repo.read_blob(repo.find_file(ai, invoice).unwrap().unwrap().id).unwrap()).unwrap();
    let line_no = |needle: &str| text.lines().position(|l| l.contains(needle)).unwrap() as u32;
    let (discount_call, round_call) = (line_no("$sum -= $this->discount("), line_no("return $this->round("));
    std::fs::write(
        fx.path.join("coverage.xml"),
        format!(
            r#"<?xml version="1.0"?><coverage><project><file name="/ci/shop/{invoice}"><line num="{}" type="stmt" count="0"/><line num="{}" type="stmt" count="4"/></file></project></coverage>"#,
            discount_call + 1,
            round_call + 1
        ),
    )
    .unwrap();
    let bin = tmp.path().join("fake-phpstan");
    std::fs::write(&bin, FAKE_PHPSTAN).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    // SAFETY: no other test reads this variable; snapshots go to the temp dir, not the user's cache.
    unsafe { std::env::set_var("SIMPLE_REVIEW_CACHE", tmp.path().join("cache")) };

    let path = fx.path.clone();
    let mut harness = Harness::builder()
        .with_size(egui::vec2(1400.0, 900.0))
        .wgpu()
        .build_eframe(move |cc| {
            cc.egui_ctx.set_visuals(egui::Visuals::dark());
            ReviewApp::with_store(cc, Store::in_memory(), Some(path))
        });
    wait_until(&mut harness, "first diff", view_ready);

    // Secrets on the unmerged `leak` branch.
    harness.state_mut().apply(Action::OpenCommit { id: parse_id(fx.commit("leak_secret")).unwrap(), path: None });
    wait_until(&mut harness, "secret scan", |app| app.secret_scans.values().any(|f| f.len() == 2));

    // Complexity and coverage for the discount commit.
    select_summary(&mut harness, "Add volume discount");
    wait_until(&mut harness, "file quality", |app| {
        file_ready(app, invoice)
            && app.rs.as_ref().unwrap().commit.as_ref().unwrap().file.as_ref().unwrap().quality.is_some()
    });
    {
        let q = harness.state().rs.as_ref().unwrap().commit.as_ref().unwrap().file.as_ref().unwrap().quality.clone().unwrap();
        assert_eq!(q.complexity[0].name, "InvoiceService::discount");
        assert_eq!(q.uncovered, Some(vec![discount_call]));
        assert!(q.secrets.is_empty());
    }

    // Untrusted: nothing runs.
    harness.state_mut().apply(Action::RunAnalysis);
    assert!(harness.state().analyses.is_empty());

    // Trusted, with the stand-in PHPStan: only the discount() lines are new.
    {
        let settings = &mut harness.state_mut().rs.as_mut().unwrap().settings;
        settings.trusted = true;
        settings.phpstan_cmd = bin.display().to_string();
        settings.pint_cmd = "true".into();
    }
    harness.key_press(Key::Q);
    harness.state_mut().apply(Action::RunAnalysis);
    wait_until(&mut harness, "analysis", |app| {
        app.analyses.values().any(|s| matches!(s, crate::state::AnalysisState::Done(_) | crate::state::AnalysisState::Failed(_)))
    });
    let warnings = harness.state().file_warnings();
    let lines: Vec<u32> = warnings.iter().map(|(_, l, _)| *l).collect();
    assert_eq!(lines, vec![discount_call, line_no("private function discount(")], "{warnings:?}");
    wait_until(&mut harness, "highlighting", |app| app.hl_pending.is_empty());
    snapshot(&mut harness, "quality_tab");

    // Precise navigation through Phpactor, when available.
    if let Ok(phpactor) = std::env::var("PHPACTOR") {
        {
            let settings = &mut harness.state_mut().rs.as_mut().unwrap().settings;
            settings.use_lsp = true;
            settings.lsp_cmd = format!("{phpactor} language-server");
        }
        let (line, col) = line_of(harness.state(), Side::New, "round($sum)");
        harness.state_mut().apply(Action::SetCursor(Cursor { side: Side::New, line, col }));
        harness.state_mut().apply(Action::GoToDefinition);
        wait_until(&mut harness, "language server", |app| {
            matches!(app.peek.definition.as_ref().map(|d| &d.state), Some(DefinitionState::Lsp { .. } | DefinitionState::Ready { .. }))
        });
        let Some(DefinitionState::Lsp { targets, .. }) = harness.state().peek.definition.as_ref().map(|d| &d.state) else {
            panic!("fell back to the symbol index")
        };
        assert_eq!(targets[0].rel.as_deref(), Some(invoice));
        assert_eq!(targets[0].line, line_no("private function round("));
        snapshot(&mut harness, "lsp_definition");
    }
}

#[test]
fn history_bisect_and_laravel() {
    use review_core::bisect::Verdict;
    use review_core::laravel::risk::Risk;

    let tmp = tempfile::tempdir().unwrap();
    let fx = fixture::build(&tmp.path().join("repo")).unwrap();
    let id = |n: &str| parse_id(fx.commit(n)).unwrap();
    let path = fx.path.clone();
    let mut harness = Harness::builder()
        .with_size(egui::vec2(1400.0, 900.0))
        .wgpu()
        .build_eframe(move |cc| {
            cc.egui_ctx.set_visuals(egui::Visuals::dark());
            ReviewApp::with_store(cc, Store::in_memory(), Some(path))
        });
    wait_until(&mut harness, "first diff", view_ready);

    // Risk badges for the commits on screen.
    wait_until(&mut harness, "risk flags", |app| app.risks.contains_key(&id("main_tz")));
    assert_eq!(harness.state().risks[&id("main_tz")], vec![Risk::Config]);
    assert!(harness.state().risks[&id("move")].is_empty());

    // Pickaxe: when did `discount(` appear?
    select_summary(&mut harness, "Add volume discount");
    wait_until(&mut harness, "ai diff", |app| file_ready(app, "app/Services/InvoiceService.php"));
    harness.state_mut().history_panel.query = "discount(".into();
    harness.state_mut().apply(Action::PickaxeSearch);
    wait_until(&mut harness, "pickaxe", |app| matches!(app.history_panel.pickaxe, Some((_, Ok(_)))));
    {
        let Some((_, Ok(hits))) = &harness.state().history_panel.pickaxe else { unreachable!() };
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].commit.id, id("ai"));
    }

    // History of the function at the cursor: discount() was born in this commit.
    let (line, col) = line_of(harness.state(), Side::New, "return $sum > 100");
    harness.state_mut().apply(Action::SetCursor(Cursor { side: Side::New, line, col }));
    harness.key_press(Key::H);
    wait_until(&mut harness, "line history", |app| matches!(app.history_panel.lines, Some((_, Ok(_)))));
    {
        let Some((label, Ok(result))) = &harness.state().history_panel.lines else { unreachable!() };
        assert!(label.contains("app/Services/InvoiceService.php"), "{label}");
        let origin = result.origin.as_ref().unwrap();
        assert_eq!(origin.id, id("ai"));
        assert!(origin.ai_assisted);
        assert!(result.steps.iter().any(|(_, c)| c.id == id("ai")));
    }
    wait_until(&mut harness, "highlighting", |app| app.hl_pending.is_empty());
    snapshot(&mut harness, "history_tab");

    // Bisect between `initial` (good) and `crlf` (bad), with `ai` as the culprit.
    let repo = review_core::git::Repo::open(&fx.path).unwrap();
    let is_bad = |c: review_core::ObjectId| {
        let mut found = false;
        repo.walk_commits(c, |info| {
            found |= info.id == id("ai");
            if found { std::ops::ControlFlow::Break(()) } else { std::ops::ControlFlow::Continue(()) }
        })
        .unwrap();
        found
    };
    harness.state_mut().apply(Action::OpenCommit { id: id("crlf"), path: None });
    harness.state_mut().apply(Action::BisectMark(crate::ui::BisectMark::Bad));
    harness.state_mut().apply(Action::OpenCommit { id: id("initial"), path: None });
    harness.state_mut().apply(Action::BisectMark(crate::ui::BisectMark::Good));
    for _ in 0..8 {
        match harness.state().bisect_verdict.clone() {
            Some(Ok(Verdict::Next { commit, .. })) => {
                harness.state_mut().apply(Action::BisectGo);
                harness.run_steps(1);
                let mark = if is_bad(commit) { crate::ui::BisectMark::Bad } else { crate::ui::BisectMark::Good };
                harness.state_mut().apply(Action::BisectMark(mark));
            }
            Some(Ok(Verdict::Found(c))) => {
                assert_eq!(c, id("ai"));
                break;
            }
            other => panic!("{other:?}"),
        }
    }
    assert!(matches!(harness.state().bisect_verdict, Some(Ok(Verdict::Found(_)))));
    snapshot(&mut harness, "bisect");
    harness.state_mut().apply(Action::BisectReset);

    // Laravel views on the `laravel` branch.
    harness.state_mut().apply(Action::OpenCommit { id: id("laravel_change"), path: None });
    harness.state_mut().apply(Action::LaravelTab(crate::state::LaravelTab::Schema));
    let key = (Some(id("laravel_base")), id("laravel_change"));
    wait_until(&mut harness, "laravel data", |app| matches!(app.laravel.data.get(&key), Some(Ok(_))));
    {
        let Some(Ok(data)) = harness.state().laravel.data.get(&key) else { unreachable!() };
        assert_eq!(data.schema_changes.len(), 1);
        assert_eq!(data.route_changes.len(), 2);
        assert_eq!(data.packages.as_ref().map(Vec::len), Some(4));
    }
    snapshot(&mut harness, "laravel_schema");
    harness.state_mut().apply(Action::LaravelTab(crate::state::LaravelTab::Routes));
    // composer.lock is the first changed file and stays collapsed; wait for the index instead.
    wait_until(&mut harness, "symbol index", |app| app.index_jobs.is_empty());
    harness.run_steps(2);
    snapshot(&mut harness, "laravel_routes");
    // The first route (GET api/invoices) opens InvoiceController::index.
    harness.state_mut().apply(Action::OpenRoute(0));
    wait_until(&mut harness, "controller", |app| {
        view_ready(app) && current_path(app).as_deref() == Some("app/Http/Controllers/InvoiceController.php")
    });
    let cursor = harness.state().rs.as_ref().unwrap().commit.as_ref().unwrap().file.as_ref().unwrap().cursor.unwrap();
    let (index_line, _) = line_of(harness.state(), Side::New, "function index");
    assert_eq!(cursor.line, index_line);
}

#[test]
fn tabs_bookmarks_and_preferences() {
    let tmp = tempfile::tempdir().unwrap();
    let a = fixture::build(&tmp.path().join("shop")).unwrap();
    let b = fixture::build(&tmp.path().join("other")).unwrap();
    let db = tmp.path().join("settings.sqlite3");
    let (pa, pb, db1) = (a.path.clone(), b.path.clone(), db.clone());
    let mut harness = Harness::builder()
        .with_size(egui::vec2(1400.0, 900.0))
        .wgpu()
        .build_eframe(move |cc| ReviewApp::with_store(cc, Store::open(&db1), Some(pa)));
    wait_until(&mut harness, "first repo", view_ready);

    // A second repository opens in its own tab; the first keeps its state.
    select_summary(&mut harness, "Move round()");
    wait_until(&mut harness, "move diff", |app| file_ready(app, "app/Services/InvoiceService.php"));
    harness.state_mut().apply(Action::OpenRepo(pb.clone()));
    wait_until(&mut harness, "second repo", view_ready);
    assert_eq!(harness.state().tab_labels(), (vec!["shop".to_string(), "other".to_string()], 1));
    assert_eq!(current_summary(harness.state()).as_deref(), Some("Convert routes to CRLF"));
    // Select a commit in tab 2 and switch away before its files arrive: the result waits.
    select_summary(&mut harness, "Add volume discount");
    harness.state_mut().apply(Action::SwitchTab(0));
    assert_eq!(current_summary(harness.state()).as_deref(), Some("Move round() below forUser()"));
    harness.run_steps(5);
    harness.state_mut().apply(Action::SwitchTab(1));
    wait_until(&mut harness, "deferred result", |app| file_ready(app, "app/Services/InvoiceService.php"));
    assert_eq!(current_summary(harness.state()).as_deref(), Some("Add volume discount"));
    // Opening an already open repository switches to its tab.
    harness.state_mut().apply(Action::OpenRepo(a.path.clone()));
    assert_eq!(harness.state().tab_labels().1, 0);

    // Bookmark a line in an older commit, move elsewhere, and come back through the bookmark.
    let (line, _) = line_of(harness.state(), Side::New, "private function round");
    harness.state_mut().apply(Action::SetCursor(Cursor { side: Side::New, line, col: 0 }));
    harness.key_press_modifiers(egui::Modifiers::COMMAND, Key::D);
    harness.run_steps(1);
    let bookmark = harness.state().rs.as_ref().unwrap().bookmarks[0].clone();
    assert_eq!(bookmark.note, "private function round(float $value): float");
    select_summary(&mut harness, "Convert routes to CRLF");
    wait_until(&mut harness, "other commit", |app| file_ready(app, "routes/web.php"));
    harness.state_mut().apply(Action::OpenBookmark(bookmark.id));
    wait_until(&mut harness, "bookmark", |app| file_ready(app, "app/Services/InvoiceService.php"));
    harness.run_steps(2);
    let cursor = harness.state().rs.as_ref().unwrap().commit.as_ref().unwrap().file.as_ref().unwrap().cursor.unwrap();
    assert_eq!((cursor.side, cursor.line), (Side::New, line));
    harness.state_mut().apply(Action::RenameBookmark(bookmark.id, "round helper".into()));
    assert_eq!(harness.state().rs.as_ref().unwrap().bookmarks[0].note, "round helper");

    // Preferences: theme, zoom and code font apply now and survive a restart.
    harness.state_mut().apply(Action::OpenPrefs);
    {
        let d = harness.state_mut().prefs_dialog.as_mut().unwrap();
        d.prefs.theme = egui::ThemePreference::Light;
        d.prefs.code_font = 16.0;
        d.unified = true;
    }
    harness.state_mut().apply(Action::SavePrefs);
    harness.run_steps(2);
    let mono = harness.ctx.global_style().text_styles[&egui::TextStyle::Monospace].size;
    assert_eq!(mono, 16.0);
    assert!(!harness.ctx.global_style().visuals.dark_mode);
    harness.state_mut().apply(Action::ShowBookmarks);
    wait_until(&mut harness, "highlighting", |app| app.hl_pending.is_empty());
    snapshot(&mut harness, "light_unified_bookmarks");
    drop(harness);

    // Restart: both tabs, the active one, and the preferences come back.
    let mut harness = Harness::builder()
        .with_size(egui::vec2(1400.0, 900.0))
        .wgpu()
        .build_eframe(move |cc| ReviewApp::with_store(cc, Store::open(&db), None));
    wait_until(&mut harness, "restored tabs", |app| app.tabs.len() == 2 && view_ready(app));
    assert_eq!(harness.state().tab_labels(), (vec!["shop".to_string(), "other".to_string()], 0));
    assert!(harness.state().unified);
    assert_eq!(harness.state().prefs.code_font, 16.0);
    harness.state_mut().apply(Action::CloseTab(0));
    assert_eq!(harness.state().tab_labels(), (vec!["other".to_string()], 0));
    assert_eq!(harness.state().rs.as_ref().unwrap().bookmarks.len(), 0, "bookmarks are per repository");
}
