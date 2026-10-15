//! Toolbar, commit details and the file header above the diff.

use std::path::PathBuf;

use egui::{RichText, Ui};
use review_core::diff::DiffOptions;
use review_core::file_view::FileView;
use review_core::git::{ChangeStatus, format_time};

use super::{Action, palette, theme};
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
        if s.repo_name.is_some() {
            if ui.add_enabled_ui(s.can_back, |ui| theme::pill(ui, "◀")).inner.on_hover_text("Back (Alt+Left)").clicked() {
                actions.push(Action::Back);
            }
            if ui.add_enabled_ui(s.can_forward, |ui| theme::pill(ui, "▶")).inner.on_hover_text("Forward (Alt+Right)").clicked() {
                actions.push(Action::Forward);
            }
            ui.add_space(4.0);
        }
        let title = s.repo_name.as_deref().unwrap_or("Simple Review");
        theme::pill_menu(ui, RichText::new(format!("{title}  ⏷")).font(theme::bold(15.0)), |ui| {
            if ui.button("Open repository…").on_hover_text("Cmd/Ctrl+O").clicked() {
                actions.push(Action::OpenDialog);
                ui.close();
            }
            if ui.button("Open in a new tab…").on_hover_text("Cmd/Ctrl+T").clicked() {
                actions.push(Action::NewTab);
                ui.close();
            }
            ui.separator();
            if s.recent.is_empty() {
                ui.weak("No recent repositories");
            } else {
                ui.label(RichText::new("RECENT").font(theme::bold(10.5)).color(theme::chrome(ui).muted));
            }
            for path in s.recent {
                let name = path.file_name().map_or_else(|| path.display().to_string(), |n| n.to_string_lossy().into_owned());
                if ui.button(name).on_hover_text(path.display().to_string()).clicked() {
                    actions.push(Action::OpenRepo(path.clone()));
                    ui.close();
                }
            }
        })
        .on_hover_text("Open a repository or pick a recent one");
        if s.repo_name.is_some() {
            if theme::pill(ui, "⟳").on_hover_text("Reload commits (F5)").clicked() {
                actions.push(Action::Reload);
            }
            if let Some(range) = &s.range {
                let accent = theme::chrome(ui).accent;
                if theme::pill(ui, RichText::new(format!("{range}  ×")).color(accent)).on_hover_text("Reviewing a range; click to leave it").clicked() {
                    actions.push(Action::ExitRange);
                }
            } else if theme::pill(ui, "Compare…").on_hover_text("Review a range: branch vs base, A...B, A..B").clicked() {
                actions.push(Action::OpenCompare);
            }
        }

        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if theme::pill(ui, "?").on_hover_text("Keyboard shortcuts (?)").clicked() {
                actions.push(Action::ToggleHelp);
            }
            if theme::pill(ui, "⚙").on_hover_text("Preferences: theme, zoom, font size, defaults (Cmd/Ctrl+,)").clicked() {
                actions.push(Action::OpenPrefs);
            }
            if let Some((version, notes)) = &s.update
                && theme::toggle_pill(ui, true, &format!("Update to {version}")).on_hover_text(format!("Download, install and restart.\n\n{notes}")).clicked()
            {
                actions.push(Action::InstallUpdate);
            }
            if s.repo_name.is_none() {
                return;
            }
            theme::pill_menu(ui, "•••", |ui| {
                if ui.button("Repository settings…").clicked() {
                    actions.push(Action::OpenSettings);
                    ui.close();
                }
                ui.separator();
                ui.weak("Bisect: mark the selected commit");
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
                if ui.button("Reset bisect").clicked() {
                    actions.push(Action::BisectReset);
                    ui.close();
                }
                ui.weak("Nothing is checked out; test each suggested commit yourself.");
            })
            .on_hover_text("Settings and bisect");
            let export = if s.open_comments > 0 { format!("Export  {}", s.open_comments) } else { "Export".to_string() };
            theme::pill_menu(ui, export, |ui| {
                if ui.button("Write feedback file for Claude Code").on_hover_text("Open comments as instructions, in the repository (see Settings)").clicked() {
                    actions.push(Action::WriteFeedback);
                    ui.close();
                }
                if ui.button("Copy open comments as prompt").clicked() {
                    actions.push(Action::CopyFeedback);
                    ui.close();
                }
                ui.separator();
                if ui.button("Share comments to .reviews/").on_hover_text("One Markdown file per thread, in the repository").clicked() {
                    actions.push(Action::ShareThreads);
                    ui.close();
                }
                if ui.button("Import comments from .reviews/").clicked() {
                    actions.push(Action::ImportThreads);
                    ui.close();
                }
                ui.separator();
                if ui.button("Save review report…").clicked() {
                    actions.push(Action::SaveReport);
                    ui.close();
                }
            })
            .on_hover_text("Open comments: export, share, report");
            ui.add_space(8.0);

            // The view options, laid out left to right inside the right-to-left row.
            ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
                ui.add_space((ui.available_width() - VIEW_OPTIONS_WIDTH).max(0.0) / 2.0);
                let views = [(false, "Split", "Side-by-side view (u)"), (true, "Unified", "Unified view (u)")];
                if theme::segmented(ui, s.unified, &views, false).is_some_and(|u| u != s.unified) {
                    actions.push(Action::ToggleUnified);
                }
                ui.add_space(6.0);
                if theme::toggle_pill(ui, s.opts.ignore_whitespace, "Whitespace").on_hover_text("Ignore whitespace (w)").clicked() {
                    actions.push(Action::ToggleWhitespace);
                }
                if theme::toggle_pill(ui, s.opts.intra_line, "Word diff").on_hover_text("Intra-line highlighting (i)").clicked() {
                    actions.push(Action::ToggleIntraLine);
                }
                if theme::toggle_pill(ui, s.blame_on, "Blame").on_hover_text("Blame gutter (b)").clicked() {
                    actions.push(Action::ToggleBlame);
                }
                if s.blame_on {
                    let mode = if s.blame_by_committer { "by committer" } else { "by author" };
                    if theme::pill(ui, mode).on_hover_text("Switch author / committer (Shift+B)").clicked() {
                        actions.push(Action::ToggleBlameMode);
                    }
                }
            });
        });
    });
}

/// About how wide the view options are, to centre them in the space left.
const VIEW_OPTIONS_WIDTH: f32 = 380.0;

pub fn commit_details(ui: &mut Ui, sc: &SelectedCommit, range: Option<&str>, actions: &mut Vec<Action>) {
    let pal = palette(ui);
    let c = &sc.info;
    if let (Some(base), Some(range)) = (sc.base, range) {
        ui.label(RichText::new(format!("Range {range}")).font(theme::bold(17.0)));
        ui.horizontal_wrapped(|ui| {
            ui.monospace(format!("{}..{}", base.to_hex_with_len(8), c.short_id()));
            ui.weak(format!("tip: {} — {}", c.summary, c.author.name));
        });
        return;
    }
    ui.horizontal_wrapped(|ui| {
        ui.label(RichText::new(&c.summary).font(theme::bold(17.0)));
        if c.ai_assisted {
            tag(ui, "AI-assisted", pal.ai).on_hover_text("Has a Co-Authored-By: Claude trailer");
        }
    });
    ui.horizontal_wrapped(|ui| {
        if ui.link(RichText::new(c.short_id()).monospace()).on_hover_text("Copy full hash").clicked() {
            ui.ctx().copy_text(c.id.to_string());
        }
        ui.label(RichText::new(&c.author.name).strong()).on_hover_text(&c.author.email);
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
    egui::Frame::new()
        .fill(ui.visuals().faint_bg_color)
        .stroke(egui::Stroke::new(1.0, theme::chrome(ui).separator))
        .corner_radius(8.0)
        .inner_margin(egui::Margin::symmetric(10, 5))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            file_header_row(ui, sf, reviewed, unified, actions);
        });
}

fn file_header_row(ui: &mut Ui, sf: &SelectedFile, reviewed: bool, unified: bool, actions: &mut Vec<Action>) {
    let pal = palette(ui);
    let change = &sf.change;
    ui.horizontal_wrapped(|ui| {
        if let Some(index) = sf.index {
            let label = if reviewed { "✔ Reviewed" } else { "Mark reviewed" };
            if theme::toggle_pill(ui, reviewed, label).on_hover_text("r").clicked() {
                actions.push(Action::SetReviewed(index, !reviewed));
            }
        }
        if let Some(b) = &sf.browse {
            ui.label(RichText::new(change.path()).monospace().strong());
            ui.weak(format!("whole file at {}", b.label));
            return;
        }
        tag(ui, &change.status.letter().to_string(), pal.status(change.status));
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
            ui.label(RichText::new(format!("+{}", diff.additions)).font(theme::bold(12.5)).color(pal.status(ChangeStatus::Added)));
            ui.label(RichText::new(format!("−{}", diff.deletions)).font(theme::bold(12.5)).color(pal.status(ChangeStatus::Deleted)));
            let hunks = sf.layout(unified).map_or(0, |l| l.hunks.len());
            if hunks > 0 {
                let cur = sf.current_hunk.map_or(0, |h| h + 1);
                ui.weak(format!("change {cur}/{hunks}"));
                if theme::pill(ui, "⏶").on_hover_text("Previous change (p)").clicked() {
                    actions.push(Action::PrevChange);
                }
                if theme::pill(ui, "⏷").on_hover_text("Next change (n)").clicked() {
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

/// A small rounded label tinted with `colour`.
fn tag(ui: &mut Ui, text: &str, colour: egui::Color32) -> egui::Response {
    egui::Frame::new()
        .fill(colour.gamma_multiply(0.16))
        .corner_radius(8.0)
        .inner_margin(egui::Margin::symmetric(6, 1))
        .show(ui, |ui| ui.label(RichText::new(text).font(theme::bold(11.0)).color(colour)))
        .inner
}

pub fn bisect_bar(ui: &mut Ui, verdict: &Result<review_core::bisect::Verdict, String>, actions: &mut Vec<Action>) {
    use review_core::bisect::Verdict;
    let pal = palette(ui);
    egui::Frame::new().fill(pal.moved_bg).inner_margin(egui::Margin::symmetric(10, 6)).corner_radius(8.0).show(ui, |ui| {
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
    let p = theme::chrome(ui);
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 4.0;
        for (i, label) in labels.iter().enumerate() {
            let selected = i == active;
            // The tab senses clicks as a container, registered before its children, so the
            // close button inside still gets its own clicks.
            let tab = ui.scope_builder(egui::UiBuilder::new().id_salt(("tab", i)).sense(egui::Sense::click()), |ui| {
                let hovered = ui.response().hovered();
                let fill = if selected {
                    p.content
                } else if hovered {
                    p.selected.gamma_multiply(0.6)
                } else {
                    egui::Color32::TRANSPARENT
                };
                egui::Frame::new()
                    .fill(fill)
                    .stroke(if selected { egui::Stroke::new(1.0, p.separator) } else { egui::Stroke::NONE })
                    .corner_radius(7.0)
                    .inner_margin(egui::Margin { left: 12, right: 4, top: 3, bottom: 3 })
                    .show(ui, |ui| {
                        ui.horizontal(|ui| {
                            ui.spacing_mut().item_spacing.x = 6.0;
                            let text = if selected { RichText::new(label).font(theme::bold(12.5)) } else { RichText::new(label).color(p.muted) };
                            ui.add(egui::Label::new(text).selectable(false).sense(egui::Sense::hover()));
                            let close = egui::Button::new(RichText::new("×").color(p.muted)).frame_when_inactive(false).corner_radius(5.0).min_size(egui::vec2(18.0, 18.0));
                            ui.add(close).on_hover_text("Close tab (Cmd/Ctrl+W)").clicked()
                        })
                        .inner
                    })
                    .inner
            });
            if tab.inner {
                actions.push(Action::CloseTab(i));
            } else if tab.response.clicked() {
                actions.push(Action::SwitchTab(i));
            }
        }
        let plus = egui::Button::new(RichText::new("+").color(p.muted)).frame_when_inactive(false).corner_radius(6.0).min_size(egui::vec2(24.0, 24.0));
        if ui.add(plus).on_hover_text("Open a repository in a new tab (Cmd/Ctrl+T)").clicked() {
            actions.push(Action::NewTab);
        }
    });
}
