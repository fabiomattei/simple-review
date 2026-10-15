//! Simple Review: a desktop app for reviewing code changes commit by commit.

mod app;
mod nav;
mod state;
mod ui;

use std::path::PathBuf;

fn main() -> eframe::Result {
    let initial = std::env::args_os().nth(1).map(PathBuf::from);
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("Simple Review")
            .with_inner_size([1440.0, 900.0])
            .with_min_inner_size([800.0, 500.0]),
        ..Default::default()
    };
    eframe::run_native(
        "Simple Review",
        options,
        Box::new(move |cc| Ok(Box::new(app::ReviewApp::new(cc, initial)))),
    )
}
