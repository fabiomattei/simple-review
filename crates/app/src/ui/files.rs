//! Files changed in the selected commit.

use egui::{Align2, FontId, Sense, TextStyle, Ui, pos2, vec2};
use review_core::diff::content::is_generated;
use review_core::git::ChangeStatus;
use review_core::settings::RepoSettings;

use super::{Action, palette};
use crate::state::{FilesState, SelectedCommit};

const ROW_H: f32 = 24.0;

pub fn show(ui: &mut Ui, sc: &mut SelectedCommit, settings: &RepoSettings, actions: &mut Vec<Action>) {
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
    let selected = sc.file.as_ref().map(|f| f.index);
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
