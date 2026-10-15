//! Files changed in the selected commit, or every file at it ("All files").

use egui::{Align2, FontId, RichText, Sense, TextStyle, Ui, pos2, vec2};
use review_core::diff::content::is_generated;
use review_core::git::ChangeStatus;
use review_core::settings::RepoSettings;

use super::{Action, palette, theme};
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
    let views = [(false, "Changed", "Files changed in this commit (t)"), (true, "All files", "Every file at this commit (t)")];
    if theme::segmented(ui, show_all, &views, true).is_some_and(|v| v != show_all) {
        actions.push(Action::ToggleAllFiles);
    }
    ui.add_space(2.0);
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
    theme::search_field(ui, filter, "Filter paths");
    let needle = filter.to_lowercase();
    let shown: Vec<&str> = files
        .iter()
        .map(|f| f.path.as_str())
        .filter(|p| needle.is_empty() || p.to_lowercase().contains(&needle))
        .collect();
    ui.label(RichText::new(format!("{} of {} files at {}", shown.len(), files.len(), sc.info.short_id())).small().color(theme::chrome(ui).muted));

    let pal = palette(ui);
    let body = TextStyle::Body.resolve(ui.style());
    let open = sc.file.as_ref().map(|f| f.change.path());
    let changed: std::collections::HashSet<&str> = sc.files().iter().map(|f| f.path()).collect();
    egui::ScrollArea::vertical().id_salt("tree_list").auto_shrink(false).show_rows(ui, ROW_H, shown.len(), |ui, range| {
        for path in &shown[range] {
            let (rect, response) = ui.allocate_exact_size(vec2(ui.available_width(), ROW_H), Sense::click());
            let selected = open == Some(*path);
            theme::row_highlight(ui, rect, selected, response.hovered());
            let painter = ui.painter_at(rect);
            let y = rect.center().y;
            if changed.contains(path) {
                painter.text(pos2(rect.min.x + 4.0, y), Align2::LEFT_CENTER, "M", FontId::monospace(body.size - 1.0), pal.warn);
            }
            painter.text(pos2(rect.min.x + 22.0, y), Align2::LEFT_CENTER, *path, body.clone(), pal.text);
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

    let reviewed = files.iter().filter(|f| sc.reviewed.contains(f.path())).count();
    ui.horizontal(|ui| {
        theme::section(ui, "Files");
        ui.label(RichText::new(format!("{} changed", files.len())).small().color(theme::chrome(ui).muted));
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.label(RichText::new(format!("{reviewed}/{}", files.len())).small().color(theme::chrome(ui).muted));
            let progress = if files.is_empty() { 0.0 } else { reviewed as f32 / files.len() as f32 };
            progress_bar(ui, progress);
        });
    });
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
                theme::row_highlight(ui, rect, selected == Some(i), response.hovered());
                let painter = ui.painter_at(rect);
                let colour = pal.status(f.status);
                let y = rect.center().y;
                painter.text(pos2(rect.min.x + 4.0, y), Align2::LEFT_CENTER, f.status.letter(), mono.clone(), colour);

                let collapsed = settings.is_ignored(path) || is_generated(path);
                let (dir, name) = match path.rsplit_once('/') {
                    Some((d, n)) => (format!("{d}/"), n),
                    None => (String::new(), path),
                };
                let name_colour = if collapsed { pal.weak } else { pal.text };
                let mut x = rect.min.x + 22.0;
                let name_font = if selected == Some(i) { super::theme::bold(body.size) } else { body.clone() };
                x += painter.text(pos2(x, y), Align2::LEFT_CENTER, name, name_font, name_colour).width() + 6.0;
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

/// A thin rounded bar, filled with the accent up to `progress` (0–1).
fn progress_bar(ui: &mut Ui, progress: f32) {
    let c = theme::chrome(ui);
    let (rect, _) = ui.allocate_exact_size(vec2(60.0, 5.0), Sense::hover());
    ui.painter().rect_filled(rect, 3.0, c.well);
    let mut done = rect;
    done.set_width(rect.width() * progress.clamp(0.0, 1.0));
    if progress > 0.0 {
        let colour = if progress >= 1.0 { palette(ui).status(ChangeStatus::Added) } else { c.accent };
        ui.painter().rect_filled(done, 3.0, colour);
    }
}
