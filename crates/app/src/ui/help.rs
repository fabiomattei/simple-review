//! Keyboard shortcut reference.

pub const SHORTCUTS: &[(&str, &str)] = &[
    ("n  /  F7", "Next change (continues into the next file)"),
    ("p  /  Shift+F7  /  Shift+N", "Previous change"),
    ("]  /  Alt+↓", "Next file"),
    ("[  /  Alt+↑", "Previous file"),
    ("j  /  k", "Next / previous commit"),
    ("Alt+←  /  Alt+→", "Back / forward in navigation history"),
    ("↑ ↓  PgUp PgDn  Home End", "Scroll the diff"),
    ("← →", "Scroll the diff horizontally"),
    ("u", "Toggle split / unified view"),
    ("w", "Toggle ignore whitespace"),
    ("i", "Toggle word (intra-line) highlighting"),
    ("r", "Toggle reviewed for the current file"),
    ("m", "Cycle merge parent"),
    ("a", "Show only AI-assisted commits"),
    ("Enter", "Show a collapsed or large file"),
    ("Cmd/Ctrl+O", "Open repository"),
    ("F5", "Reload commits"),
    ("?  /  F1", "Show this help"),
];

pub fn show(ctx: &egui::Context, open: &mut bool) {
    egui::Window::new("Keyboard shortcuts")
        .open(open)
        .collapsible(false)
        .resizable(false)
        .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
        .show(ctx, |ui| {
            egui::Grid::new("shortcuts").striped(true).num_columns(2).show(ui, |ui| {
                for (keys, what) in SHORTCUTS {
                    ui.monospace(*keys);
                    ui.label(*what);
                    ui.end_row();
                }
            });
        });
}
