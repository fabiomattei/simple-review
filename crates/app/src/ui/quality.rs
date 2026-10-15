//! The Quality tab: new warnings, possible secrets, complexity and coverage.

use std::sync::Arc;

use egui::{RichText, Ui};
use review_core::analysis::AnalysisResult;
use review_core::analysis::secrets::SecretFinding;

use super::{Action, palette};
use crate::state::FileQuality;

pub struct QualityView {
    pub trusted: bool,
    /// `Ok` done, `Err(None)` running, `Err(Some)` failed; `None` not run.
    pub analysis: Option<Result<Arc<AnalysisResult>, Option<String>>>,
    pub secrets: Option<Vec<(String, SecretFinding)>>,
    pub file: Option<String>,
    pub quality: Option<FileQuality>,
    pub coverage_note: Option<String>,
    pub coverage_on: bool,
}

pub fn show(ui: &mut Ui, v: &QualityView, actions: &mut Vec<Action>) {
    let pal = palette(ui);
    egui::ScrollArea::vertical().id_salt("quality").auto_shrink([false, true]).show(ui, |ui| {
        // Secrets first: they are the most urgent.
        match &v.secrets {
            None => {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.weak("Scanning the commit for secrets…");
                });
            }
            Some(found) if found.is_empty() => {
                ui.weak("No secrets found in the added lines of this commit.");
            }
            Some(found) => {
                ui.label(RichText::new(format!("⚠ {} possible secret(s) added", found.len())).color(pal.warn).strong());
                for (path, s) in found {
                    let text = format!("{path}:{}  {} — {}", s.line + 1, s.kind, s.masked);
                    if ui.link(RichText::new(text).monospace()).clicked() {
                        actions.push(Action::GoToLine { path: path.clone(), line: s.line });
                    }
                }
            }
        }
        ui.separator();

        ui.horizontal(|ui| {
            ui.strong("New warnings (PHPStan, Pint)");
            if v.trusted {
                let running = matches!(v.analysis, Some(Err(None)));
                if ui.add_enabled(!running, egui::Button::new(if v.analysis.is_some() { "Run again" } else { "Run analysis" })).clicked() {
                    actions.push(Action::RunAnalysis);
                }
            } else if ui.button("Settings…").clicked() {
                actions.push(Action::OpenSettings);
            }
        });
        match &v.analysis {
            _ if !v.trusted => {
                ui.weak("These tools run code from the repository, so they are off until you mark it as trusted in Settings.");
            }
            None => {
                ui.weak("Runs the project's PHPStan and Pint on the changed files of both versions and shows only what this commit introduced.");
            }
            Some(Err(None)) => {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label("Analysing… (see the status bar)");
                });
            }
            Some(Err(Some(e))) => {
                ui.colored_label(ui.visuals().error_fg_color, e);
            }
            Some(Ok(r)) => {
                ui.weak(format!(
                    "{} new · {} before, {} after on the changed files · ran {}",
                    r.introduced.len(),
                    r.old_total,
                    r.new_total,
                    r.ran.iter().map(|t| t.label()).collect::<Vec<_>>().join(", ")
                ));
                for note in &r.notes {
                    ui.label(RichText::new(format!("⚠ {note}")).color(pal.warn).small());
                }
                for w in &r.introduced {
                    let location = match w.line {
                        Some(l) => format!("{}:{}", w.path, l + 1),
                        None => w.path.clone(),
                    };
                    ui.horizontal_wrapped(|ui| {
                        ui.label(RichText::new(w.tool.label()).small().color(pal.warn));
                        if ui.link(RichText::new(location).monospace()).clicked()
                            && let Some(line) = w.line
                        {
                            actions.push(Action::GoToLine { path: w.path.clone(), line });
                        }
                        ui.label(&w.message);
                    });
                }
            }
        }
        ui.separator();

        let Some(file) = &v.file else { return };
        ui.strong(format!("This file: {file}"));
        let Some(q) = &v.quality else {
            ui.weak("Open a changed file to see its complexity and coverage.");
            return;
        };
        if q.complexity.is_empty() {
            ui.weak("No function changed size or branching.");
        } else {
            egui::Grid::new("complexity").striped(true).num_columns(3).show(ui, |ui| {
                ui.weak("Function");
                ui.weak("Lines");
                ui.weak("Branches (cyclomatic)");
                ui.end_row();
                for c in &q.complexity {
                    if let Some(m) = c.new.or(c.old)
                        && ui.link(RichText::new(&c.name).monospace()).clicked()
                    {
                        actions.push(Action::GoToLine { path: file.clone(), line: m.line });
                    }
                    let fmt = |o: Option<u32>| o.map_or("—".to_string(), |n| n.to_string());
                    ui.label(format!("{} -> {}", fmt(c.old.map(|m| m.lines)), fmt(c.new.map(|m| m.lines))));
                    let delta = c.branch_delta();
                    let colour = if delta > 0 { pal.warn } else { pal.text };
                    ui.label(
                        RichText::new(format!(
                            "{} -> {}{}",
                            fmt(c.old.map(|m| m.cyclomatic())),
                            fmt(c.new.map(|m| m.cyclomatic())),
                            if delta > 0 { format!("  (+{delta})") } else { String::new() }
                        ))
                        .color(colour),
                    );
                    ui.end_row();
                }
            });
        }
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            let mut on = v.coverage_on;
            if ui.toggle_value(&mut on, "Coverage overlay").on_hover_text("v").changed() {
                actions.push(Action::ToggleCoverage);
            }
            match (&q.uncovered, &v.coverage_note) {
                (Some(lines), _) => {
                    ui.label(if lines.is_empty() {
                        "All added executable lines are covered.".to_string()
                    } else {
                        format!("{} added line(s) not covered by tests.", lines.len())
                    });
                }
                (None, Some(note)) if note.starts_with("No coverage") || note.starts_with("Could not") => {
                    ui.weak(note);
                }
                (None, _) => {
                    ui.weak("No coverage data for this file.");
                }
            }
        });
        if let Some(note) = &v.coverage_note {
            ui.weak(note);
        }
    });
}
