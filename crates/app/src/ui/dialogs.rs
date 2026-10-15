//! Modal-ish windows: the comment editor, range comparison and repository settings.

use egui::{Key, Modifiers, RichText};

use super::{Action, markdown};
use crate::state::{CommentEditor, CompareDialog, PrefsDialog, SettingsDialog};

pub fn comment_editor(ctx: &egui::Context, editor: &mut CommentEditor, actions: &mut Vec<Action>) {
    let mut open = true;
    egui::Window::new("Comment")
        .id(egui::Id::new("comment_editor"))
        .open(&mut open)
        .collapsible(false)
        .default_width(520.0)
        .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
        .show(ctx, |ui| {
            ui.weak(&editor.context);
            ui.horizontal(|ui| {
                ui.selectable_value(&mut editor.preview, false, "Write");
                ui.selectable_value(&mut editor.preview, true, "Preview");
                ui.weak("Markdown");
            });
            if editor.preview {
                egui::Frame::group(ui.style()).show(ui, |ui| {
                    ui.set_min_size(egui::vec2(480.0, 140.0));
                    markdown::show(ui, &editor.text);
                });
            } else {
                let edit = ui.add(
                    egui::TextEdit::multiline(&mut editor.text)
                        .desired_rows(8)
                        .desired_width(f32::INFINITY)
                        .hint_text("What should change? Markdown is supported."),
                );
                if !edit.has_focus() && editor.text.is_empty() {
                    edit.request_focus();
                }
            }
            if let Some(e) = &editor.error {
                ui.colored_label(ui.visuals().error_fg_color, e);
            }
            ui.horizontal(|ui| {
                let save = ui.add_enabled(!editor.text.trim().is_empty(), egui::Button::new("Save (Cmd/Ctrl+Enter)"));
                if save.clicked() {
                    actions.push(Action::SaveComment);
                }
                if ui.button("Cancel (Esc)").clicked() {
                    actions.push(Action::CloseDialogs);
                }
            });
        });
    let (submit, cancel) = ctx.input_mut(|i| (i.consume_key(Modifiers::COMMAND, Key::Enter), i.consume_key(Modifiers::NONE, Key::Escape)));
    if submit && !editor.text.trim().is_empty() {
        actions.push(Action::SaveComment);
    }
    if cancel || !open {
        actions.push(Action::CloseDialogs);
    }
}

pub fn compare(ctx: &egui::Context, d: &mut CompareDialog, actions: &mut Vec<Action>) {
    let mut open = true;
    egui::Window::new("Compare")
        .open(&mut open)
        .collapsible(false)
        .default_width(460.0)
        .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
        .show(ctx, |ui| {
            ui.label("Review everything between two points in history.");
            ui.weak("base...tip compares from their merge base (like a pull request); base..tip compares the two directly.");
            let edit = ui.add(egui::TextEdit::singleline(&mut d.spec).hint_text("main...feature").desired_width(f32::INFINITY));
            if edit.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter)) {
                actions.push(Action::CompareRange(d.spec.clone()));
            }
            if let (Some(base), Some(current)) = (&d.base_branch, &d.current_branch)
                && base != current
            {
                let spec = format!("{base}...{current}");
                if ui.button(format!("Current branch vs {base}  ({spec})")).clicked() {
                    actions.push(Action::CompareRange(spec));
                }
            }
            if let Some(base) = &d.base_branch {
                let others: Vec<&String> = d.refs.iter().filter(|r| *r != base).take(12).collect();
                if !others.is_empty() {
                    ui.label(RichText::new(format!("Branches and tags vs {base}:")).small());
                    ui.horizontal_wrapped(|ui| {
                        for r in others {
                            if ui.small_button(r).clicked() {
                                d.spec = format!("{base}...{r}");
                            }
                        }
                    });
                }
            }
            if let Some(e) = &d.error {
                ui.colored_label(ui.visuals().error_fg_color, e);
            }
            ui.horizontal(|ui| {
                if ui.add_enabled(!d.spec.trim().is_empty(), egui::Button::new("Compare")).clicked() {
                    actions.push(Action::CompareRange(d.spec.clone()));
                }
                if ui.button("Cancel").clicked() {
                    actions.push(Action::CloseDialogs);
                }
            });
        });
    if !open || ctx.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Escape)) {
        actions.push(Action::CloseDialogs);
    }
}

pub fn settings(ctx: &egui::Context, d: &mut SettingsDialog, actions: &mut Vec<Action>) {
    let mut open = true;
    egui::Window::new("Repository settings")
        .open(&mut open)
        .collapsible(false)
        .default_width(460.0)
        .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
        .show(ctx, |ui| {
            egui::Grid::new("settings_grid").num_columns(2).spacing([12.0, 8.0]).show(ui, |ui| {
                ui.label("Base branch");
                ui.text_edit_singleline(&mut d.base_branch);
                ui.end_row();
                ui.label("Feedback file");
                ui.add(egui::TextEdit::singleline(&mut d.feedback_path).hint_text("REVIEW_FEEDBACK.md"))
                    .on_hover_text("Relative to the repository root");
                ui.end_row();
                ui.label("Live refresh");
                ui.checkbox(&mut d.live_refresh, "Show new commits as they appear");
                ui.end_row();
            });
            ui.separator();
            ui.strong("Project tools");
            ui.checkbox(&mut d.trusted, "Trust this repository: allow running its tools");
            ui.label(
                RichText::new(
                    "PHPStan, Pint, the language server and ide-helper execute code from the repository \
                     (configuration, extensions, bootstrap files). Only enable this for code you trust.",
                )
                .small()
                .color(ui.visuals().warn_fg_color),
            );
            ui.add_enabled_ui(d.trusted, |ui| {
                egui::Grid::new("tools_grid").num_columns(2).spacing([12.0, 6.0]).show(ui, |ui| {
                    ui.label("PHPStan");
                    ui.add(egui::TextEdit::singleline(&mut d.phpstan_cmd).hint_text("vendor/bin/phpstan"));
                    ui.end_row();
                    ui.label("Pint");
                    ui.add(egui::TextEdit::singleline(&mut d.pint_cmd).hint_text("vendor/bin/pint"));
                    ui.end_row();
                    ui.label("Language server");
                    ui.add(egui::TextEdit::singleline(&mut d.lsp_cmd).hint_text("phpactor language-server"));
                    ui.end_row();
                    ui.label("");
                    ui.checkbox(&mut d.use_lsp, "Use it for go to definition and references");
                    ui.end_row();
                });
            });
            egui::Grid::new("coverage_grid").num_columns(2).spacing([12.0, 6.0]).show(ui, |ui| {
                ui.label("Coverage (Clover XML)");
                ui.add(egui::TextEdit::singleline(&mut d.coverage_path).hint_text("coverage.xml"))
                    .on_hover_text("Relative to the repository root, or absolute. Reading it runs nothing.");
                ui.end_row();
            });
            ui.separator();
            ui.label("Ignored paths (collapsed and not indexed), one per line:");
            ui.weak("dir/  matches a directory · *.ext  matches a suffix · anything else a file name or path");
            ui.add(egui::TextEdit::multiline(&mut d.ignored_paths).desired_rows(6).desired_width(f32::INFINITY).code_editor());
            ui.horizontal(|ui| {
                if ui.button("Save").clicked() {
                    actions.push(Action::SaveSettings);
                }
                if ui.button("Cancel").clicked() {
                    actions.push(Action::CloseDialogs);
                }
            });
        });
    if !open || ctx.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Escape)) {
        actions.push(Action::CloseDialogs);
    }
}

pub fn prefs(ctx: &egui::Context, d: &mut PrefsDialog, actions: &mut Vec<Action>) {
    let mut open = true;
    egui::Window::new("Preferences")
        .open(&mut open)
        .collapsible(false)
        .default_width(380.0)
        .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
        .show(ctx, |ui| {
            egui::Grid::new("prefs_grid").num_columns(2).spacing([12.0, 8.0]).show(ui, |ui| {
                ui.label("Theme");
                ui.horizontal(|ui| {
                    ui.selectable_value(&mut d.prefs.theme, egui::ThemePreference::System, "System");
                    ui.selectable_value(&mut d.prefs.theme, egui::ThemePreference::Light, "Light");
                    ui.selectable_value(&mut d.prefs.theme, egui::ThemePreference::Dark, "Dark");
                });
                ui.end_row();
                ui.label("Interface zoom");
                ui.add(egui::Slider::new(&mut d.prefs.zoom, 0.6..=2.0).step_by(0.05).suffix("×"))
                    .on_hover_text("Also Cmd/Ctrl + and −");
                ui.end_row();
                ui.label("Code font size");
                ui.add(egui::Slider::new(&mut d.prefs.code_font, 9.0..=24.0).step_by(0.5).suffix(" pt"));
                ui.end_row();
                ui.label("Diff layout");
                ui.horizontal(|ui| {
                    ui.selectable_value(&mut d.unified, false, "Split");
                    ui.selectable_value(&mut d.unified, true, "Unified");
                });
                ui.end_row();
                ui.label("Blame gutter");
                ui.checkbox(&mut d.blame_on, "Show by default");
                ui.end_row();
                ui.label("Coverage overlay");
                ui.checkbox(&mut d.coverage_on, "Show when a report is available");
                ui.end_row();
                ui.label("Updates");
                ui.vertical(|ui| {
                    let available = crate::updater::available();
                    ui.add_enabled(available, egui::Checkbox::new(&mut d.check_updates, "Check for updates at startup"));
                    if available {
                        if ui.small_button("Check now").clicked() {
                            actions.push(Action::CheckUpdates);
                        }
                    } else {
                        ui.weak("This build has no update channel.");
                    }
                });
                ui.end_row();
            });
            ui.horizontal(|ui| {
                if ui.button("Save").clicked() {
                    actions.push(Action::SavePrefs);
                }
                if ui.button("Cancel").clicked() {
                    actions.push(Action::CloseDialogs);
                }
            });
        });
    if !open || ctx.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Escape)) {
        actions.push(Action::CloseDialogs);
    }
}
