//! The Comments tab of the peek panel: threads with replies and status changes.

use egui::{RichText, Ui};
use review_core::review::{Placement, Thread, ThreadStatus};

use super::{Action, markdown, palette};
use crate::state::{CommentScope, StatusFilter};

pub struct ThreadRow {
    pub thread: Thread,
    /// Where the thread sits in the current view, if it is on this file.
    pub placement: Option<Placement>,
}

pub struct CommentsView {
    pub rows: Vec<ThreadRow>,
    pub scope: CommentScope,
    pub status: StatusFilter,
    pub focused: Option<i64>,
    /// Scroll the focused thread into view (once, after it was selected).
    pub reveal: bool,
    /// What a new comment would attach to, e.g. "lines 12–14 (new)".
    pub target: Option<String>,
}

pub fn show(ui: &mut Ui, v: &CommentsView, actions: &mut Vec<Action>) {
    let pal = palette(ui);
    ui.horizontal(|ui| {
        let scopes = [(CommentScope::File, "This file", ""), (CommentScope::Commit, "This review", ""), (CommentScope::All, "Repository", "")];
        if let Some(scope) = super::theme::segmented(ui, v.scope, &scopes, false) {
            actions.push(Action::CommentScope(scope));
        }
        let statuses = [(StatusFilter::Open, "Open", ""), (StatusFilter::Closed, "Closed", ""), (StatusFilter::All, "All", "")];
        if let Some(status) = super::theme::segmented(ui, v.status, &statuses, false) {
            actions.push(Action::CommentStatusFilter(status));
        }
        let label = match &v.target {
            Some(t) => format!("Comment on {t} (c)"),
            None => "Comment (c)".to_string(),
        };
        if ui.add_enabled(v.target.is_some(), egui::Button::new(label)).on_disabled_hover_text("Click or select lines in the diff first").clicked() {
            actions.push(Action::StartComment);
        }
    });
    ui.separator();
    if v.rows.is_empty() {
        ui.weak("No comments here. Select lines in the diff (drag, or Shift+click) and press c.");
        return;
    }
    egui::ScrollArea::vertical().id_salt("threads").auto_shrink([false, true]).show(ui, |ui| {
        for row in &v.rows {
            let t = &row.thread;
            let focused = v.focused == Some(t.id);
            let frame = egui::Frame::group(ui.style()).stroke(if focused {
                egui::Stroke::new(1.5, pal.accent)
            } else {
                ui.visuals().widgets.noninteractive.bg_stroke
            });
            let response = frame
                .show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    thread_header(ui, row, actions);
                    let snippet = t.anchor.lines.iter().take(6).cloned().collect::<Vec<_>>().join("\n");
                    egui::Frame::new().fill(ui.visuals().extreme_bg_color).inner_margin(4.0).show(ui, |ui| {
                        ui.label(RichText::new(snippet).monospace().color(pal.weak));
                    });
                    for c in &t.comments {
                        ui.horizontal(|ui| {
                            ui.strong(&c.author_name);
                            ui.weak(super::time_label(c.created_at));
                            if c.updated_at > c.created_at {
                                ui.weak("(edited)");
                            }
                            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                if ui.small_button("Delete").clicked() {
                                    actions.push(Action::DeleteComment(c.id));
                                }
                                if ui.small_button("Edit").clicked() {
                                    actions.push(Action::EditComment(c.id));
                                }
                            });
                        });
                        markdown::show(ui, &c.body);
                    }
                    ui.horizontal(|ui| {
                        if ui.button("Reply").clicked() {
                            actions.push(Action::ReplyTo(t.id));
                        }
                        match t.status {
                            ThreadStatus::Open => {
                                if ui.button("Resolve").clicked() {
                                    actions.push(Action::SetThreadStatus(t.id, ThreadStatus::Resolved));
                                }
                                if ui.button("Won't fix").clicked() {
                                    actions.push(Action::SetThreadStatus(t.id, ThreadStatus::WontFix));
                                }
                            }
                            _ => {
                                if ui.button("Reopen").clicked() {
                                    actions.push(Action::SetThreadStatus(t.id, ThreadStatus::Open));
                                }
                            }
                        }
                    });
                })
                .response;
            if focused && v.reveal {
                response.scroll_to_me(Some(egui::Align::Min));
            }
            ui.add_space(4.0);
        }
    });
}

fn thread_header(ui: &mut Ui, row: &ThreadRow, actions: &mut Vec<Action>) {
    let pal = palette(ui);
    let t = &row.thread;
    ui.horizontal(|ui| {
        let (status_colour, status_text) = match t.status {
            ThreadStatus::Open => (pal.accent, "Open"),
            ThreadStatus::Resolved => (pal.status(review_core::git::ChangeStatus::Added), "Resolved"),
            ThreadStatus::WontFix => (pal.weak, "Won't fix"),
        };
        egui::Frame::new().stroke(egui::Stroke::new(1.0, status_colour)).corner_radius(3.0).inner_margin(egui::Margin::symmetric(4, 0)).show(ui, |ui| {
            ui.label(RichText::new(status_text).small().color(status_colour));
        });
        let link = ui.link(RichText::new(format!("{}:{}", t.path, t.line_label())).monospace());
        if link.clicked() {
            actions.push(Action::GoToThread(t.id));
        }
        ui.weak(format!("{} side · {}", t.side.as_str(), &t.commit_id[..8.min(t.commit_id.len())]));
        match row.placement {
            Some(Placement::Moved { start, end }) => {
                ui.weak(format!("now at {}", review_core::review::model::line_label(start, end)));
            }
            Some(Placement::Fuzzy { start, end }) => {
                ui.label(RichText::new(format!("⚠ code changed, near {}", review_core::review::model::line_label(start, end))).color(pal.warn));
            }
            Some(Placement::Outdated) => {
                ui.label(RichText::new("⚠ outdated: code not found").color(pal.warn));
            }
            _ => {}
        }
    });
}
