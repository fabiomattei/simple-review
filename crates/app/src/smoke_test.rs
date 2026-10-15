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
    let Some(dir) = std::env::var_os("SNAPSHOT_DIR") else { return };
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
    assert_eq!(harness.state().rs.as_ref().unwrap().commits.len(), fx.commits.len());

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
