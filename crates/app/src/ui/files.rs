//! Files changed in the selected commit, or every file at it ("All files").

use egui::{Align2, FontId, Sense, TextStyle, Ui, pos2, vec2};
use review_core::diff::content::is_generated;
use review_core::git::ChangeStatus;
use review_core::settings::RepoSettings;

use super::{Action, palette};
use crate::state::{FilesState, SelectedCommit};

const ROW_H: f32 = 24.0;

pub fn show(
    ui: &mut Ui,
    sc: &mut SelectedCommit,
    settings: &RepoSettings,
    show_all: bool,
    filter: &mut String,
    actions: &mut Vec<Action>,
) {
    ui.horizontal(|ui| {
        if ui.selectable_label(!show_all, "Changed").on_hover_text("Files changed in this commit (t)").clicked() && show_all {
            actions.push(Action::ToggleAllFiles);
        }
        if ui.selectable_label(show_all, "All files").on_hover_text("Every file at this commit (t)").clicked() && !show_all {
            actions.push(Action::ToggleAllFiles);
        }
    });
    if show_all {
        all_files(ui, sc, filter, actions);
    } else {
        changed_files(ui, sc, settings, actions);
    }
}

fn all_files(ui: &mut Ui, sc: &SelectedCommit, filter: &mut String, actions: &mut Vec<Action>) {
    let files = match &sc.tree {
        None => {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label("Listing files…");
            });
            return;
        }
        Some(Err(e)) => {
            ui.colored_label(ui.visuals().error_fg_color, e);
            return;
        }
        Some(Ok(files)) => files,
    };
    ui.add(egui::TextEdit::singleline(filter).hint_text("Filter paths…").desired_width(f32::INFINITY));
    let needle = filter.to_lowercase();
    let shown: Vec<&str> = files
        .iter()
        .map(|f| f.path.as_str())
        .filter(|p| needle.is_empty() || p.to_lowercase().contains(&needle))
        .collect();
    ui.weak(format!("{} of {} files at {}", shown.len(), files.len(), sc.info.short_id()));
    ui.separator();

    let pal = palette(ui);
    let body = TextStyle::Body.resolve(ui.style());
    let open = sc.file.as_ref().map(|f| f.change.path());
    let changed: std::collections::HashSet<&str> = sc.files().iter().map(|f| f.path()).collect();
    egui::ScrollArea::vertical().id_salt("tree_list").auto_shrink(false).show_rows(ui, ROW_H, shown.len(), |ui, range| {
        for path in &shown[range] {
            let (rect, response) = ui.allocate_exact_size(vec2(ui.available_width(), ROW_H), Sense::click());
            let selected = open == Some(*path);
            if selected {
                ui.painter().rect_filled(rect, 3.0, ui.visuals().selection.bg_fill);
            } else if response.hovered() {
                ui.painter().rect_filled(rect, 3.0, ui.visuals().widgets.hovered.weak_bg_fill);
            }
            let painter = ui.painter_at(rect);
            let y = rect.center().y;
            if changed.contains(path) {
                painter.text(pos2(rect.min.x + 4.0, y), Align2::LEFT_CENTER, "M", FontId::monospace(body.size - 1.0), pal.warn);
            }
            let colour = if selected { ui.visuals().selection.stroke.color } else { pal.text };
            painter.text(pos2(rect.min.x + 22.0, y), Align2::LEFT_CENTER, *path, body.clone(), colour);
            if response.clicked() {
                actions.push(Action::OpenTreeFile(path.to_string()));
            }
        }
    });
}

fn changed_files(ui: &mut Ui, sc: &mut SelectedCommit, settings: &RepoSettings, actions: &mut Vec<Action>) {
    let files = match &sc.files {
        FilesState::Loading => {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label("Loading changed files…");
            });
            return;
        }
        FilesState::Error(e) => {
            ui.colored_label(ui.visuals().error_fg_color, e);
            return;
        }
        FilesState::Ready(files) => files,
    };

    ui.horizontal(|ui| {
        ui.strong("Files");
        let reviewed = files.iter().filter(|f| sc.reviewed.contains(f.path())).count();
        ui.weak(format!("{} changed · {reviewed}/{} reviewed", files.len(), files.len()));
    });
    ui.separator();
    if files.is_empty() {
        ui.weak("No changes against this parent.");
        return;
    }

    let pal = palette(ui);
    let body = TextStyle::Body.resolve(ui.style());
    let mono = FontId::monospace(body.size - 1.0);
    let selected = sc.file.as_ref().and_then(|f| f.index);
    let mut area = egui::ScrollArea::vertical().id_salt("file_list").auto_shrink(false);
    if std::mem::take(&mut sc.reveal_file)
        && let Some(sel) = selected
    {
        let top = sel as f32 * (ROW_H + ui.spacing().item_spacing.y);
        area = area.vertical_scroll_offset((top - ui.available_height() / 2.0).max(0.0));
    }

    area.show_rows(ui, ROW_H, files.len(), |ui, range| {
        for i in range {
            let f = &files[i];
            let path = f.path();
            ui.horizontal(|ui| {
                let mut reviewed = sc.reviewed.contains(path);
                if ui.checkbox(&mut reviewed, "").on_hover_text("Reviewed (r)").changed() {
                    actions.push(Action::SetReviewed(i, reviewed));
                }
                let (rect, response) = ui.allocate_exact_size(vec2(ui.available_width(), ROW_H), Sense::click());
                if selected == Some(i) {
                    ui.painter().rect_filled(rect, 3.0, ui.visuals().selection.bg_fill);
                } else if response.hovered() {
                    ui.painter().rect_filled(rect, 3.0, ui.visuals().widgets.hovered.weak_bg_fill);
                }
                let painter = ui.painter_at(rect);
                let colour = pal.status(f.status);
                let y = rect.center().y;
                painter.text(pos2(rect.min.x + 4.0, y), Align2::LEFT_CENTER, f.status.letter(), mono.clone(), colour);

                let collapsed = settings.is_ignored(path) || is_generated(path);
                let (dir, name) = match path.rsplit_once('/') {
                    Some((d, n)) => (format!("{d}/"), n),
                    None => (String::new(), path),
                };
                let name_colour = if collapsed { pal.weak } else if selected == Some(i) { ui.visuals().selection.stroke.color } else { pal.text };
                let mut x = rect.min.x + 22.0;
                x += painter.text(pos2(x, y), Align2::LEFT_CENTER, name, body.clone(), name_colour).width() + 6.0;
                painter.text(pos2(x, y), Align2::LEFT_CENTER, &dir, FontId::proportional(body.size - 2.0), pal.weak);

                if response.clicked() {
                    actions.push(Action::SelectFile(i));
                }
                response.on_hover_ui(|ui| {
                    ui.monospace(path);
                    if matches!(f.status, ChangeStatus::Renamed | ChangeStatus::Copied)
                        && let Some(old) = &f.old
                    {
                        ui.label(format!("from {}", old.path));
                    }
                    if collapsed {
                        ui.weak("Collapsed by default (ignored or generated path)");
                    }
                });
            });
        }
    });
}
