//! Lazily loaded commit list.

use egui::{Align2, FontId, Rect, Sense, TextStyle, Ui, pos2, vec2};
use review_core::git::format_time;

use super::{Action, palette};
use crate::state::RepoState;

const ROW_H: f32 = 40.0;
/// Ask for more commits when the last visible row is this close to the end of what's loaded.
const PREFETCH: usize = 60;

pub fn show(ui: &mut Ui, rs: &mut RepoState, actions: &mut Vec<Action>) {
    ui.horizontal(|ui| {
        ui.strong("Commits");
        let loaded = if rs.pager_done { format!("{}", rs.commits.len()) } else { format!("{}+", rs.commits.len()) };
        ui.weak(loaded);
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            let mut ai = rs.ai_only;
            if ui.toggle_value(&mut ai, "AI only").on_hover_text("Show only AI-assisted commits (a)").changed() {
                actions.push(Action::ToggleAiOnly);
            }
        });
    });
    ui.separator();

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
        for vi in range {
            let idx = rs.visible[vi];
            let c = &rs.commits[idx];
            let (rect, response) = ui.allocate_exact_size(vec2(ui.available_width(), ROW_H), Sense::click());
            let is_selected = selected == Some(c.id);
            if is_selected {
                ui.painter().rect_filled(rect, 3.0, ui.visuals().selection.bg_fill);
            } else if response.hovered() {
                ui.painter().rect_filled(rect, 3.0, ui.visuals().widgets.hovered.weak_bg_fill);
            }
            let painter = ui.painter_at(rect.shrink(2.0));
            let text_colour = if is_selected { ui.visuals().selection.stroke.color } else { pal.text };
            let x = rect.min.x + 6.0;
            let line1 = rect.min.y + 11.0;
            let line2 = rect.min.y + 29.0;

            // Badges on the right of the first line, so the summary is clipped before them.
            let mut right = rect.max.x - 6.0;
            let mut badge = |label: &str, colour: egui::Color32| {
                let galley = painter.layout_no_wrap(label.to_string(), small.clone(), colour);
                let r = Rect::from_min_size(pos2(right - galley.size().x - 8.0, line1 - 8.0), vec2(galley.size().x + 8.0, 16.0));
                painter.rect_stroke(r, 4.0, (1.0, colour), egui::StrokeKind::Inside);
                painter.galley(pos2(r.min.x + 4.0, r.center().y - galley.size().y / 2.0), galley, colour);
                right = r.min.x - 4.0;
            };
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
                body.clone(),
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
