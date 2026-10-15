//! The History tab: pickaxe search and line/function history.

use egui::{RichText, Ui};
use review_core::git::format_time;
use review_core::history::Direction;

use super::{Action, palette};
use crate::state::HistoryPanel;

pub fn show(ui: &mut Ui, h: &mut HistoryPanel, has_file: bool, actions: &mut Vec<Action>) {
    let pal = palette(ui);
    ui.horizontal(|ui| {
        ui.label("When did");
        let edit = ui.add(egui::TextEdit::singleline(&mut h.query).hint_text("text or code").desired_width(220.0));
        ui.label("appear or disappear?");
        if edit.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
            actions.push(Action::PickaxeSearch);
        }
        ui.checkbox(&mut h.regex, "regex");
        ui.add_enabled(has_file, egui::Checkbox::new(&mut h.this_file, "this file only"));
        if ui.add_enabled(!h.query.trim().is_empty(), egui::Button::new("Search")).clicked() {
            actions.push(Action::PickaxeSearch);
        }
    });
    ui.horizontal(|ui| {
        if ui
            .button("History of these lines")
            .on_hover_text("The selected lines, or the function (or line) at the cursor; follows moves and renames")
            .clicked()
        {
            actions.push(Action::LineHistory);
        }
        ui.weak("Select lines or click inside a function first.");
    });
    ui.separator();

    egui::ScrollArea::vertical().id_salt("history").auto_shrink([false, true]).show(ui, |ui| {
        if let Some((label, result)) = &h.lines {
            ui.strong(format!("History of {label}"));
            match result {
                Err(None) => {
                    ui.spinner();
                }
                Err(Some(e)) => {
                    ui.colored_label(ui.visuals().error_fg_color, e);
                }
                Ok(result) => {
                    let steps = &result.steps;
                    if let Some(origin) = &result.origin {
                        ui.horizontal(|ui| {
                            ui.label("Originally written by");
                            ui.strong(&origin.author.name);
                            ui.weak(format!("in {} “{}”, {}", origin.short_id(), origin.summary, format_time(&origin.author)));
                            if origin.ai_assisted {
                                ui.colored_label(pal.ai, "AI-assisted");
                            }
                        });
                    }
                    for (i, (step, c)) in steps.iter().enumerate() {
                        ui.horizontal(|ui| {
                            let marker = if step.lines.is_none() { "deleted" } else if i + 1 == steps.len() { "origin" } else { "" };
                            if ui.link(RichText::new(c.short_id()).monospace()).clicked() {
                                actions.push(Action::OpenHistoryStep(i));
                            }
                            ui.label(&c.summary);
                            ui.weak(format!("{} · {}", c.author.name, format_time(&c.author)));
                            if !marker.is_empty() {
                                ui.weak(format!("({marker})"));
                            }
                        });
                    }
                }
            }
            ui.separator();
        }
        if let Some((label, result)) = &h.pickaxe {
            ui.strong(format!("Commits that changed {label}"));
            match result {
                Err(None) => {
                    ui.spinner();
                }
                Err(Some(e)) => {
                    ui.colored_label(ui.visuals().error_fg_color, e);
                }
                Ok(hits) if hits.is_empty() => {
                    ui.weak("No commit added or removed it.");
                }
                Ok(hits) => {
                    for hit in hits {
                        ui.horizontal_wrapped(|ui| {
                            ui.monospace(hit.commit.short_id());
                            ui.label(&hit.commit.summary);
                            ui.weak(format!("{} · {}", hit.commit.author.name, format_time(&hit.commit.author)));
                            for (path, dir) in &hit.files {
                                let (sign, colour) = match dir {
                                    Direction::Added => ("+", pal.status(review_core::git::ChangeStatus::Added)),
                                    Direction::Removed => ("-", pal.status(review_core::git::ChangeStatus::Deleted)),
                                    Direction::Changed => ("~", pal.warn),
                                };
                                if ui.link(RichText::new(format!("{sign} {path}")).color(colour)).clicked() {
                                    actions.push(Action::OpenPickaxeHit { commit: hit.commit.id, path: path.clone() });
                                }
                            }
                        });
                    }
                }
            }
        }
    });
}
