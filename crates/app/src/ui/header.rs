//! Toolbar, commit details and the file header above the diff.

use std::path::PathBuf;

use egui::{RichText, Ui};
use review_core::diff::DiffOptions;
use review_core::file_view::FileView;
use review_core::git::{ChangeStatus, format_time};

use super::{Action, palette};
use crate::state::{SelectedCommit, SelectedFile, ViewState};

pub struct ToolbarState<'a> {
    pub repo_name: Option<String>,
    pub recent: &'a [PathBuf],
    pub can_back: bool,
    pub can_forward: bool,
    pub opts: DiffOptions,
    pub unified: bool,
    pub blame_on: bool,
    pub blame_by_committer: bool,
    /// Label of the range under review, if any.
    pub range: Option<String>,
    pub open_comments: usize,
    /// A newer version that can be installed, with its release notes.
    pub update: Option<(String, String)>,
}

pub fn toolbar(ui: &mut Ui, s: &ToolbarState<'_>, actions: &mut Vec<Action>) {
    ui.horizontal(|ui| {
        if ui.button("Open…").on_hover_text("Open a repository (Cmd/Ctrl+O)").clicked() {
            actions.push(Action::OpenDialog);
        }
        ui.menu_button("Recent", |ui| {
            if s.recent.is_empty() {
                ui.weak("No recent repositories");
            }
            for path in s.recent {
                if ui.button(path.display().to_string()).clicked() {
                    actions.push(Action::OpenRepo(path.clone()));
                    ui.close();
                }
            }
        });
        if let Some(name) = &s.repo_name {
            ui.separator();
            ui.strong(name);
            if ui.small_button("⟳").on_hover_text("Reload commits (F5)").clicked() {
                actions.push(Action::Reload);
            }
            if ui.button("Compare…").on_hover_text("Review a range: branch vs base, A...B, A..B").clicked() {
                actions.push(Action::OpenCompare);
            }
            if let Some(range) = &s.range {
                ui.label(RichText::new(format!("Range {range}")).color(palette(ui).accent));
                if ui.small_button("×").on_hover_text("Leave range review").clicked() {
                    actions.push(Action::ExitRange);
                }
            }
            ui.menu_button(format!("Export ({})", s.open_comments), |ui| {
                if ui.button("Write feedback file for Claude Code").on_hover_text("Open comments as instructions, in the repository (see Settings)").clicked() {
                    actions.push(Action::WriteFeedback);
                    ui.close();
                }
                if ui.button("Copy open comments as prompt").clicked() {
                    actions.push(Action::CopyFeedback);
                    ui.close();
                }
                if ui.button("Save review report…").clicked() {
                    actions.push(Action::SaveReport);
                    ui.close();
                }
            });
            ui.menu_button("Bisect", |ui| {
                ui.weak("Mark the selected commit:");
                for (mark, label) in [
                    (super::BisectMark::Bad, "Bad (has the problem)"),
                    (super::BisectMark::Good, "Good (before the problem)"),
                    (super::BisectMark::Skip, "Skip (can't test)"),
                ] {
                    if ui.button(label).clicked() {
                        actions.push(Action::BisectMark(mark));
                        ui.close();
                    }
                }
                ui.separator();
                if ui.button("Reset bisect").clicked() {
                    actions.push(Action::BisectReset);
                    ui.close();
                }
                ui.weak("Nothing is checked out; test each suggested commit yourself.");
            });
            if ui.button("Settings").on_hover_text("Settings for this repository").clicked() {
                actions.push(Action::OpenSettings);
            }
        }
        ui.separator();
        if ui.add_enabled(s.can_back, egui::Button::new("◀")).on_hover_text("Back (Alt+Left)").clicked() {
            actions.push(Action::Back);
        }
        if ui.add_enabled(s.can_forward, egui::Button::new("▶")).on_hover_text("Forward (Alt+Right)").clicked() {
            actions.push(Action::Forward);
        }
        ui.separator();
        if ui.selectable_label(!s.unified, "Split").on_hover_text("Side-by-side view (u)").clicked() && s.unified {
            actions.push(Action::ToggleUnified);
        }
        if ui.selectable_label(s.unified, "Unified").on_hover_text("Unified view (u)").clicked() && !s.unified {
            actions.push(Action::ToggleUnified);
        }
        ui.separator();
        let mut ws = s.opts.ignore_whitespace;
        if ui.toggle_value(&mut ws, "Ignore whitespace").on_hover_text("w").changed() {
            actions.push(Action::ToggleWhitespace);
        }
        let mut il = s.opts.intra_line;
        if ui.toggle_value(&mut il, "Word diff").on_hover_text("Intra-line highlighting (i)").changed() {
            actions.push(Action::ToggleIntraLine);
        }
        ui.separator();
        let mut blame = s.blame_on;
        if ui.toggle_value(&mut blame, "Blame").on_hover_text("Blame gutter (b)").changed() {
            actions.push(Action::ToggleBlame);
        }
        if s.blame_on {
            let mode = if s.blame_by_committer { "by committer" } else { "by author" };
            if ui.small_button(mode).on_hover_text("Switch author / committer (Shift+B)").clicked() {
                actions.push(Action::ToggleBlameMode);
            }
        }
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if ui.button("?").on_hover_text("Keyboard shortcuts (?)").clicked() {
                actions.push(Action::ToggleHelp);
            }
            if let Some((version, notes)) = &s.update
                && ui
                    .button(RichText::new(format!("Update to {version}")).color(palette(ui).accent))
                    .on_hover_text(format!("Download, install and restart.\n\n{notes}"))
                    .clicked()
            {
                actions.push(Action::InstallUpdate);
            }
            if ui.button("Preferences").on_hover_text("Theme, zoom, font size, defaults (Cmd/Ctrl+,)").clicked() {
                actions.push(Action::OpenPrefs);
            }
        });
    });
}

pub fn commit_details(ui: &mut Ui, sc: &SelectedCommit, range: Option<&str>, actions: &mut Vec<Action>) {
    let pal = palette(ui);
    let c = &sc.info;
    if let (Some(base), Some(range)) = (sc.base, range) {
        ui.label(RichText::new(format!("Range {range}")).strong().size(16.0));
        ui.horizontal_wrapped(|ui| {
            ui.monospace(format!("{}..{}", base.to_hex_with_len(8), c.short_id()));
            ui.weak(format!("tip: {} — {}", c.summary, c.author.name));
        });
        return;
    }
    ui.horizontal_wrapped(|ui| {
        ui.label(RichText::new(&c.summary).strong().size(16.0));
        if c.ai_assisted {
            ui.label(RichText::new("AI-assisted").color(pal.ai).small())
                .on_hover_text("Has a Co-Authored-By: Claude trailer");
        }
    });
    ui.horizontal_wrapped(|ui| {
        if ui.link(RichText::new(c.short_id()).monospace()).on_hover_text("Copy full hash").clicked() {
            ui.ctx().copy_text(c.id.to_string());
        }
        ui.label(format!("{} <{}>", c.author.name, c.author.email));
        ui.weak(format_time(&c.author));
        if c.author.email != c.committer.email || c.author.name != c.committer.name {
            ui.label(RichText::new(format!("⚠ committed by {} <{}> {}", c.committer.name, c.committer.email, format_time(&c.committer))).color(pal.warn))
                .on_hover_text("Committer differs from author (rebase, cherry-pick or applied patch)");
        }
        for co in &c.co_authors {
            ui.weak(format!("co-author: {co}"));
        }
    });
    if c.is_merge() {
        ui.horizontal(|ui| {
            ui.label("Compare against parent:");
            egui::ComboBox::from_id_salt("merge_parent")
                .selected_text(format!("{} {}", sc.parent_idx + 1, c.parents[sc.parent_idx].to_hex_with_len(8)))
                .show_ui(ui, |ui| {
                    for (i, p) in c.parents.iter().enumerate() {
                        if ui.selectable_label(i == sc.parent_idx, format!("{} {}", i + 1, p.to_hex_with_len(8))).clicked() {
                            actions.push(Action::SetParent(i));
                        }
                    }
                });
            ui.weak("(m to cycle)");
        });
    }
    let body = c.message.trim().strip_prefix(c.summary.trim()).unwrap_or("").trim();
    if !body.is_empty() {
        egui::CollapsingHeader::new("Message").id_salt(("message", c.id)).default_open(body.lines().count() <= 6).show(ui, |ui| {
            ui.label(body);
        });
    }
}

pub fn file_header(ui: &mut Ui, sf: &SelectedFile, reviewed: bool, unified: bool, actions: &mut Vec<Action>) {
    let pal = palette(ui);
    let change = &sf.change;
    ui.horizontal_wrapped(|ui| {
        if let Some(index) = sf.index {
            let mut r = reviewed;
            if ui.checkbox(&mut r, "Reviewed").on_hover_text("r").changed() {
                actions.push(Action::SetReviewed(index, r));
            }
        }
        if let Some(b) = &sf.browse {
            ui.label(RichText::new(change.path()).monospace().strong());
            ui.weak(format!("whole file at {}", b.label));
            return;
        }
        ui.label(RichText::new(change.status.letter().to_string()).monospace().color(pal.status(change.status)));
        if matches!(change.status, ChangeStatus::Renamed | ChangeStatus::Copied)
            && let Some(old) = &change.old
        {
            ui.label(RichText::new(&old.path).monospace().color(pal.weak));
            ui.label("->");
        }
        ui.label(RichText::new(change.path()).monospace().strong());
        if let ViewState::Ready(view) = &sf.view
            && let FileView::Text { diff, non_utf8 } = view.as_ref()
        {
            ui.label(RichText::new(format!("+{}", diff.additions)).color(pal.status(ChangeStatus::Added)));
            ui.label(RichText::new(format!("−{}", diff.deletions)).color(pal.status(ChangeStatus::Deleted)));
            let hunks = sf.layout(unified).map_or(0, |l| l.hunks.len());
            if hunks > 0 {
                let cur = sf.current_hunk.map_or(0, |h| h + 1);
                ui.weak(format!("change {cur}/{hunks}"));
                if ui.small_button("⏶").on_hover_text("Previous change (p)").clicked() {
                    actions.push(Action::PrevChange);
                }
                if ui.small_button("⏷").on_hover_text("Next change (n)").clicked() {
                    actions.push(Action::NextChange);
                }
            }
            let notice = |ui: &mut Ui, text: &str| ui.label(RichText::new(format!("⚠ {text}")).color(pal.warn));
            if diff.line_endings_changed() {
                let (from, to) = if diff.new.crlf { ("LF", "CRLF") } else { ("CRLF", "LF") };
                notice(ui, &format!("line endings changed from {from} to {to}"));
            }
            if *non_utf8 {
                notice(ui, "not UTF-8, shown as Latin-1");
            }
            if diff.new.missing_final_newline && !diff.new.lines.is_empty() {
                ui.weak("no newline at end of file");
            }
        }
    });
}

pub fn bisect_bar(ui: &mut Ui, verdict: &Result<review_core::bisect::Verdict, String>, actions: &mut Vec<Action>) {
    use review_core::bisect::Verdict;
    let pal = palette(ui);
    egui::Frame::new().fill(pal.moved_bg).inner_margin(4.0).corner_radius(4.0).show(ui, |ui| {
        ui.horizontal_wrapped(|ui| {
            ui.strong("Bisect");
            match verdict {
                Err(e) => {
                    ui.colored_label(ui.visuals().error_fg_color, e);
                }
                Ok(Verdict::NeedBad) => {
                    ui.label("Select a commit that has the problem and mark it Bad (Bisect menu).");
                }
                Ok(Verdict::NeedGood) => {
                    ui.label("Now select an older commit without the problem and mark it Good.");
                }
                Ok(Verdict::Next { commit, remaining }) => {
                    ui.label(format!("{remaining} commits could be the culprit. Test next:"));
                    if ui.link(RichText::new(commit.to_hex_with_len(8).to_string()).monospace()).clicked() {
                        actions.push(Action::BisectGo);
                    }
                    ui.weak("then mark it Good, Bad or Skip.");
                }
                Ok(Verdict::Found(commit)) => {
                    ui.label(RichText::new("First bad commit:").color(pal.warn));
                    if ui.link(RichText::new(commit.to_hex_with_len(8).to_string()).monospace()).clicked() {
                        actions.push(Action::BisectGo);
                    }
                }
                Ok(Verdict::OnlySkipped(c)) => {
                    ui.label(format!("Only skipped commits are left ({}); one of them introduced the problem.", c.len()));
                }
            }
            for (mark, label) in [(super::BisectMark::Good, "Good"), (super::BisectMark::Bad, "Bad"), (super::BisectMark::Skip, "Skip")] {
                if ui.small_button(label).on_hover_text("Mark the selected commit").clicked() {
                    actions.push(Action::BisectMark(mark));
                }
            }
            if ui.small_button("Reset").clicked() {
                actions.push(Action::BisectReset);
            }
        });
    });
}

pub fn tab_strip(ui: &mut Ui, labels: &[String], active: usize, actions: &mut Vec<Action>) {
    ui.horizontal(|ui| {
        for (i, label) in labels.iter().enumerate() {
            let selected = i == active;
            egui::Frame::new()
                .fill(if selected { ui.visuals().selection.bg_fill } else { ui.visuals().faint_bg_color })
                .corner_radius(4.0)
                .inner_margin(egui::Margin::symmetric(6, 2))
                .show(ui, |ui| {
                    ui.horizontal(|ui| {
                        let text = if selected { RichText::new(label).strong() } else { RichText::new(label) };
                        if ui.add(egui::Label::new(text).sense(egui::Sense::click())).clicked() {
                            actions.push(Action::SwitchTab(i));
                        }
                        if ui.small_button("×").on_hover_text("Close tab (Cmd/Ctrl+W)").clicked() {
                            actions.push(Action::CloseTab(i));
                        }
                    });
                });
        }
        if ui.small_button("+").on_hover_text("Open a repository in a new tab (Cmd/Ctrl+T)").clicked() {
            actions.push(Action::NewTab);
        }
    });
}
