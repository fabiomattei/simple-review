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
use crate::state::{FilesState, ViewState};
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
    assert_eq!(current_path(harness.state()).as_deref(), Some("app/Services/InvoiceService.php"));
    harness.key_press(Key::R);
    harness.key_press(Key::CloseBracket);
    wait_until(&mut harness, "logo placeholder", view_ready);
    assert_eq!(current_path(harness.state()).as_deref(), Some("public/logo.png"));
    let sc = harness.state().rs.as_ref().unwrap().commit.as_ref().unwrap();
    assert!(sc.reviewed.contains("app/Services/InvoiceService.php"));
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
