//! Lazily loaded commit list.

use egui::{Align2, FontId, Rect, RichText, Sense, TextStyle, Ui, pos2, vec2};
use review_core::git::format_time;

use std::collections::{HashMap, HashSet};

use review_core::ObjectId;
use review_core::laravel::risk::Risk;

use super::{Action, palette, theme};
use crate::state::RepoState;

const ROW_H: f32 = 40.0;
/// Ask for more commits when the last visible row is this close to the end of what's loaded.
const PREFETCH: usize = 60;

/// Per-commit decorations computed elsewhere.
pub struct Marks<'a> {
    pub risks: &'a HashMap<ObjectId, Vec<Risk>>,
    pub good: HashSet<ObjectId>,
    pub bad: Option<ObjectId>,
    pub skip: HashSet<ObjectId>,
}

pub fn show(ui: &mut Ui, rs: &mut RepoState, marks: &Marks<'_>, actions: &mut Vec<Action>) {
    ui.horizontal(|ui| {
        theme::section(ui, "Commits");
        let loaded = if rs.pager_done { format!("{}", rs.commits.len()) } else { format!("{}+", rs.commits.len()) };
        ui.label(RichText::new(loaded).small().color(theme::chrome(ui).muted));
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if theme::toggle_pill(ui, rs.ai_only, "AI only").on_hover_text("Show only AI-assisted commits (a)").clicked() {
                actions.push(Action::ToggleAiOnly);
            }
        });
    });
    ui.add_space(2.0);
    if let Some(range) = &rs.range {
        let whole = rs.commit.as_ref().is_some_and(|c| c.base.is_some());
        let (rect, response) = ui.allocate_exact_size(vec2(ui.available_width(), 28.0), Sense::click());
        theme::row_highlight(ui, rect, whole, response.hovered());
        let accent = theme::chrome(ui).accent;
        ui.painter().text(pos2(rect.min.x + 8.0, rect.center().y), Align2::LEFT_CENTER, format!("▶  Whole range {}", range.label), theme::bold(13.0), accent);
        if response.on_hover_text("Review all changes in the range at once").clicked() {
            actions.push(Action::SelectWholeRange);
        }
        ui.add_space(2.0);
    }

    let pal = palette(ui);
    let selected = rs.commit.as_ref().map(|c| c.info.id);
    let mut area = egui::ScrollArea::vertical().id_salt("commit_list").auto_shrink(false);
    if std::mem::take(&mut rs.reveal_commit)
        && let Some(idx) = rs.selected_visible_index()
    {
        area = area.vertical_scroll_offset((idx as f32 * ROW_H - ui.available_height() / 2.0).max(0.0));
    }
    let body = TextStyle::Body.resolve(ui.style());
    let small = FontId::proportional(body.size - 1.5);
    let mono = FontId::monospace(body.size - 1.5);

    area.show_rows(ui, ROW_H, rs.visible.len(), |ui, range| {
        if range.end + PREFETCH > rs.visible.len() && !rs.pager_done && !rs.pager_waiting {
            actions.push(Action::LoadMoreCommits);
        }
        let missing: Vec<ObjectId> = range.clone().map(|vi| rs.commits[rs.visible[vi]].id).filter(|id| !marks.risks.contains_key(id)).collect();
        if !missing.is_empty() {
            actions.push(Action::ComputeRisks(missing));
        }
        for vi in range {
            let idx = rs.visible[vi];
            let c = &rs.commits[idx];
            let (rect, response) = ui.allocate_exact_size(vec2(ui.available_width(), ROW_H), Sense::click());
            let is_selected = selected == Some(c.id) && rs.commit.as_ref().is_some_and(|sc| sc.base.is_none());
            theme::row_highlight(ui, rect.shrink2(vec2(0.0, 1.0)), is_selected, response.hovered());
            if is_selected {
                let bar = Rect::from_min_size(pos2(rect.min.x + 2.0, rect.min.y + 9.0), vec2(3.0, ROW_H - 18.0));
                ui.painter().rect_filled(bar, 2.0, pal.accent);
            }
            let painter = ui.painter_at(rect.shrink(2.0));
            let text_colour = pal.text;
            let x = rect.min.x + 10.0;
            let line1 = rect.min.y + 11.0;
            let line2 = rect.min.y + 29.0;

            // Badges on the right of the first line, so the summary is clipped before them.
            let mut right = rect.max.x - 8.0;
            let mut badge = |label: &str, colour: egui::Color32| {
                right = theme::paint_tag(&painter, right, line1, label, colour) - 4.0;
            };
            if marks.bad == Some(c.id) {
                badge("bad", pal.status(review_core::git::ChangeStatus::Deleted));
            } else if marks.good.contains(&c.id) {
                badge("good", pal.status(review_core::git::ChangeStatus::Added));
            } else if marks.skip.contains(&c.id) {
                badge("skip", pal.weak);
            }
            for r in marks.risks.get(&c.id).into_iter().flatten().rev() {
                badge(r.label(), pal.warn);
            }
            if c.ai_assisted {
                badge("AI", pal.ai);
            }
            if c.is_merge() {
                badge("merge", pal.weak);
            }
            let hash_w = painter
                .text(pos2(x, line1), Align2::LEFT_CENTER, c.short_id(), mono.clone(), pal.weak)
                .width();
            painter.with_clip_rect(Rect::from_min_max(rect.min, pos2(right, rect.max.y))).text(
                pos2(x + hash_w + 8.0, line1),
                Align2::LEFT_CENTER,
                &c.summary,
                if is_selected { theme::bold(body.size) } else { body.clone() },
                text_colour,
            );
            let mut meta = format!("{}  ·  {}", c.author.name, format_time(&c.author));
            if c.author.email != c.committer.email {
                meta.push_str(&format!("  ·  committed by {}", c.committer.name));
            }
            painter.text(pos2(x, line2), Align2::LEFT_CENTER, meta, small.clone(), pal.weak);

            if response.clicked() {
                actions.push(Action::SelectCommit(idx));
            }
            response.on_hover_ui(|ui| {
                ui.monospace(c.id.to_string());
                ui.label(&c.message);
            });
        }
    });
}
