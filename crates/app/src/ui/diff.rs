//! The side-by-side / unified diff view.
//!
//! Rows are painted directly (only the visible ones), so both panes share one vertical scroll
//! position and stay aligned. Horizontal scrolling is shared too.

use std::ops::Range;

use egui::text::LayoutJob;
use egui::{Align2, Color32, FontId, Pos2, Rect, Sense, Stroke, TextFormat, TextStyle, Ui, UiBuilder, Vec2, pos2, vec2};
use review_core::diff::{Cell, LineKind, Row};
use review_core::highlight::{Highlighted, Span};

use super::{Action, Palette, palette};

pub struct DiffInput<'a> {
    pub rows: &'a [Row],
    pub hunks: &'a [Range<usize>],
    pub old: &'a [String],
    pub new: &'a [String],
    pub hl_old: Option<&'a Highlighted>,
    pub hl_new: Option<&'a Highlighted>,
    pub unified: bool,
    pub current_hunk: Option<usize>,
    pub max_line_chars: usize,
}

/// Keyboard scrolling requested by the app.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum KeyScroll {
    Lines(f32),
    Pages(f32),
    Top,
    Bottom,
}

const OVERVIEW_WIDTH: f32 = 14.0;
/// Characters shown per line; the rest is cut to keep layout cheap on minified files.
const MAX_SHOWN_CHARS: usize = 1500;
const TAB: &str = "    ";

struct Metrics {
    font: FontId,
    char_w: f32,
    digits: usize,
}

pub fn show(
    ui: &mut Ui,
    input: &DiffInput<'_>,
    scroll_to_row: &mut Option<usize>,
    h_scroll: &mut f32,
    key_scroll: Option<KeyScroll>,
    actions: &mut Vec<Action>,
) {
    let pal = palette(ui);
    let font = TextStyle::Monospace.resolve(ui.style());
    let (row_h, char_w) = ui.fonts_mut(|f| (f.row_height(&font) + 3.0, f.glyph_width(&font, '0')));
    let digits = input.old.len().max(input.new.len()).max(1).to_string().len();
    let m = Metrics { font, char_w, digits };

    let full = ui.available_rect_before_wrap();
    let main = Rect::from_min_max(full.min, pos2(full.max.x - OVERVIEW_WIDTH, full.max.y));
    let bar = Rect::from_min_max(pos2(main.max.x, full.min.y), full.max);
    let total = input.rows.len();
    let content_h = total as f32 * row_h;

    let offset_id = ui.id().with("diff_offset");
    let last_offset: f32 = ui.data(|d| d.get_temp(offset_id)).unwrap_or(0.0);
    let page = (main.height() - row_h * 2.0).max(row_h);
    let mut wanted_offset = match key_scroll {
        Some(KeyScroll::Lines(n)) => Some(last_offset + n * row_h),
        Some(KeyScroll::Pages(n)) => Some(last_offset + n * page),
        Some(KeyScroll::Top) => Some(0.0),
        Some(KeyScroll::Bottom) => Some(content_h),
        None => None,
    };
    if let Some(row) = scroll_to_row.take() {
        wanted_offset = Some(row as f32 * row_h - main.height() / 3.0);
    }

    // Horizontal scrolling: trackpad / shift+wheel while hovering the diff.
    let text_area_w = text_area_width(&m, main.width(), input.unified);
    let max_h = (input.max_line_chars.min(MAX_SHOWN_CHARS) as f32 * char_w - text_area_w + 3.0 * char_w).max(0.0);
    if ui.rect_contains_pointer(main) {
        let dx = ui.input(|i| i.smooth_scroll_delta.x);
        *h_scroll -= dx;
    }
    *h_scroll = h_scroll.clamp(0.0, max_h);

    let mut area = egui::ScrollArea::vertical().id_salt("diff_rows").auto_shrink(false);
    if let Some(offset) = wanted_offset {
        area = area.vertical_scroll_offset(offset.clamp(0.0, (content_h - main.height()).max(0.0)));
    }
    let output = ui
        .scope_builder(UiBuilder::new().max_rect(main), |ui| {
            area.show_viewport(ui, |ui, viewport| {
                ui.set_height(content_h);
                let origin = ui.max_rect().min;
                let width = ui.max_rect().width();
                let rows_rect = Rect::from_min_size(origin, vec2(width, content_h));
                let response = ui.interact(rows_rect, ui.id().with("rows"), Sense::click());
                if response.clicked()
                    && let Some(pos) = response.interact_pointer_pos()
                {
                    let row = ((pos.y - origin.y) / row_h).floor() as usize;
                    if row < total {
                        actions.push(Action::ClickRow(row));
                    }
                }

                let first = (viewport.min.y / row_h).floor().max(0.0) as usize;
                let last = ((viewport.max.y / row_h).ceil() as usize + 1).min(total);
                let current = input.current_hunk.and_then(|h| input.hunks.get(h)).cloned().unwrap_or(0..0);
                let painter = ui.painter();
                for i in first..last {
                    let y = origin.y + i as f32 * row_h;
                    let row_rect = Rect::from_min_size(pos2(origin.x, y), vec2(width, row_h));
                    let row = &input.rows[i];
                    if input.unified {
                        paint_unified_row(painter, &m, &pal, row_rect, row, input, *h_scroll);
                    } else {
                        let half = width / 2.0;
                        let left = Rect::from_min_size(row_rect.min, vec2(half, row_h));
                        let right = Rect::from_min_size(pos2(origin.x + half, y), vec2(width - half, row_h));
                        paint_cell(painter, &m, &pal, left, row.left.as_ref(), input.old, input.hl_old, *h_scroll, Side::Old);
                        paint_cell(painter, &m, &pal, right, row.right.as_ref(), input.new, input.hl_new, *h_scroll, Side::New);
                        painter.line_segment(
                            [pos2(right.min.x, y), pos2(right.min.x, y + row_h)],
                            Stroke::new(1.0, ui.visuals().widgets.noninteractive.bg_stroke.color),
                        );
                    }
                    if current.contains(&i) {
                        painter.rect_filled(Rect::from_min_size(row_rect.min, vec2(3.0, row_h)), 0.0, pal.accent);
                    }
                }
            })
        })
        .inner;
    let offset = output.state.offset.y;
    ui.data_mut(|d| d.insert_temp(offset_id, offset));

    paint_overview(ui, &pal, bar, input, offset, main.height(), content_h, scroll_to_row);
}

#[derive(Clone, Copy, PartialEq)]
enum Side {
    Old,
    New,
}

fn text_area_width(m: &Metrics, width: f32, unified: bool) -> f32 {
    if unified {
        width - 2.0 * gutter_width(m) - marker_width(m)
    } else {
        width / 2.0 - gutter_width(m) - marker_width(m)
    }
}

fn gutter_width(m: &Metrics) -> f32 {
    m.digits as f32 * m.char_w + 14.0
}

fn marker_width(m: &Metrics) -> f32 {
    m.char_w + 8.0
}

fn kind_bg(pal: &Palette, kind: LineKind) -> Option<Color32> {
    match kind {
        LineKind::Context => None,
        LineKind::Removed => Some(pal.removed_bg),
        LineKind::Added => Some(pal.added_bg),
        LineKind::Moved => Some(pal.moved_bg),
    }
}

/// A non-colour cue for each line kind.
fn marker(kind: LineKind, side: Side) -> &'static str {
    match (kind, side) {
        (LineKind::Context, _) => "",
        (LineKind::Removed, _) => "-",
        (LineKind::Added, _) => "+",
        (LineKind::Moved, Side::Old) => "<",
        (LineKind::Moved, Side::New) => ">",
    }
}

#[allow(clippy::too_many_arguments)]
fn paint_cell(
    painter: &egui::Painter,
    m: &Metrics,
    pal: &Palette,
    rect: Rect,
    cell: Option<&Cell>,
    lines: &[String],
    hl: Option<&Highlighted>,
    h_scroll: f32,
    side: Side,
) {
    let Some(cell) = cell else {
        painter.rect_filled(rect, 0.0, pal.filler);
        return;
    };
    if let Some(bg) = kind_bg(pal, cell.kind) {
        painter.rect_filled(rect, 0.0, bg);
    }
    let gutter = gutter_width(m);
    painter.text(
        pos2(rect.min.x + gutter - 8.0, rect.center().y),
        Align2::RIGHT_CENTER,
        cell.line + 1,
        m.font.clone(),
        pal.weak,
    );
    let marker_x = rect.min.x + gutter;
    painter.text(pos2(marker_x + 2.0, rect.center().y), Align2::LEFT_CENTER, marker(cell.kind, side), m.font.clone(), pal.weak);
    let text_rect = Rect::from_min_max(pos2(marker_x + marker_width(m), rect.min.y), rect.max);
    paint_text(painter, m, pal, text_rect, cell, lines, hl, h_scroll);
}

fn paint_unified_row(
    painter: &egui::Painter,
    m: &Metrics,
    pal: &Palette,
    rect: Rect,
    row: &Row,
    input: &DiffInput<'_>,
    h_scroll: f32,
) {
    // Context rows carry both sides; show the new one. Change rows carry exactly one side.
    let (cell, lines, hl, side) = if let Some(r) = &row.right {
        (r, input.new, input.hl_new, Side::New)
    } else if let Some(l) = &row.left {
        (l, input.old, input.hl_old, Side::Old)
    } else {
        return;
    };
    if let Some(bg) = kind_bg(pal, cell.kind) {
        painter.rect_filled(rect, 0.0, bg);
    }
    let gutter = gutter_width(m);
    let y = rect.center().y;
    if let Some(l) = &row.left {
        painter.text(pos2(rect.min.x + gutter - 8.0, y), Align2::RIGHT_CENTER, l.line + 1, m.font.clone(), pal.weak);
    }
    if let Some(r) = &row.right {
        painter.text(pos2(rect.min.x + 2.0 * gutter - 8.0, y), Align2::RIGHT_CENTER, r.line + 1, m.font.clone(), pal.weak);
    }
    let marker_x = rect.min.x + 2.0 * gutter;
    painter.text(pos2(marker_x + 2.0, y), Align2::LEFT_CENTER, marker(cell.kind, side), m.font.clone(), pal.weak);
    let text_rect = Rect::from_min_max(pos2(marker_x + marker_width(m), rect.min.y), rect.max);
    paint_text(painter, m, pal, text_rect, cell, lines, hl, h_scroll);
}

#[allow(clippy::too_many_arguments)]
fn paint_text(
    painter: &egui::Painter,
    m: &Metrics,
    pal: &Palette,
    rect: Rect,
    cell: &Cell,
    lines: &[String],
    hl: Option<&Highlighted>,
    h_scroll: f32,
) {
    let Some(text) = lines.get(cell.line as usize) else { return };
    let spans = hl.and_then(|h| h.get(cell.line as usize)).map(Vec::as_slice).unwrap_or(&[]);
    let inline_bg = match cell.kind {
        LineKind::Removed => pal.removed_inline,
        _ => pal.added_inline,
    };
    let job = line_job(text, spans, &cell.inline, &m.font, pal.text, inline_bg);
    let galley = painter.layout_job(job);
    let clip = painter.clip_rect().intersect(rect);
    let pos = Pos2::new(rect.min.x - h_scroll, rect.center().y - galley.size().y / 2.0);
    painter.with_clip_rect(clip).galley(pos, galley, pal.text);
}

/// Build the coloured text of one line from syntax spans and intra-line change ranges.
pub fn line_job(
    text: &str,
    spans: &[Span],
    inline: &[Range<usize>],
    font: &FontId,
    default: Color32,
    inline_bg: Color32,
) -> LayoutJob {
    let shown = if text.len() > MAX_SHOWN_CHARS * 4 || text.chars().count() > MAX_SHOWN_CHARS {
        let end = text.char_indices().nth(MAX_SHOWN_CHARS).map_or(text.len(), |(i, _)| i);
        &text[..end]
    } else {
        text
    };
    let mut cuts: Vec<usize> = vec![0, shown.len()];
    for s in spans {
        cuts.extend([s.start as usize, s.end as usize]);
    }
    for r in inline {
        cuts.extend([r.start, r.end]);
    }
    cuts.retain(|&c| c <= shown.len() && shown.is_char_boundary(c));
    cuts.sort_unstable();
    cuts.dedup();

    let mut job = LayoutJob::default();
    let mut span_idx = 0;
    for w in cuts.windows(2) {
        let (a, b) = (w[0], w[1]);
        while span_idx < spans.len() && (spans[span_idx].end as usize) <= a {
            span_idx += 1;
        }
        let span = spans.get(span_idx).filter(|s| (s.start as usize) <= a);
        let changed = inline.iter().any(|r| r.start <= a && a < r.end);
        let format = TextFormat {
            font_id: font.clone(),
            color: span.map_or(default, |s| Color32::from_rgb(s.rgb[0], s.rgb[1], s.rgb[2])),
            background: if changed { inline_bg } else { Color32::TRANSPARENT },
            italics: span.is_some_and(|s| s.italic),
            ..Default::default()
        };
        let segment = &shown[a..b];
        if segment.contains('\t') {
            job.append(&segment.replace('\t', TAB), 0.0, format);
        } else {
            job.append(segment, 0.0, format);
        }
    }
    if shown.len() < text.len() {
        job.append(" …", 0.0, TextFormat { font_id: font.clone(), color: default, ..Default::default() });
    }
    job
}

#[allow(clippy::too_many_arguments)]
fn paint_overview(
    ui: &mut Ui,
    pal: &Palette,
    bar: Rect,
    input: &DiffInput<'_>,
    offset: f32,
    view_h: f32,
    content_h: f32,
    scroll_to_row: &mut Option<usize>,
) {
    let total = input.rows.len().max(1) as f32;
    let painter = ui.painter_at(bar);
    painter.rect_filled(bar, 0.0, ui.visuals().extreme_bg_color);
    for (i, hunk) in input.hunks.iter().enumerate() {
        let rows = &input.rows[hunk.clone()];
        let has = |k: LineKind| rows.iter().flat_map(|r| [&r.left, &r.right]).flatten().any(|c| c.kind == k);
        let colour = match (has(LineKind::Removed), has(LineKind::Added), has(LineKind::Moved)) {
            (_, _, true) => pal.accent,
            (true, false, _) => pal.removed_inline,
            (false, true, _) => pal.added_inline,
            _ => pal.warn,
        };
        let y0 = bar.min.y + hunk.start as f32 / total * bar.height();
        let h = (hunk.len() as f32 / total * bar.height()).max(3.0);
        let r = Rect::from_min_size(pos2(bar.min.x + 2.0, y0), vec2(bar.width() - 4.0, h));
        painter.rect_filled(r, 1.0, colour);
        if input.current_hunk == Some(i) {
            painter.rect_stroke(r.expand(1.0), 1.0, Stroke::new(1.5, pal.text), egui::StrokeKind::Outside);
        }
    }
    if content_h > view_h {
        let y0 = bar.min.y + offset / content_h * bar.height();
        let h = view_h / content_h * bar.height();
        let r = Rect::from_min_size(pos2(bar.min.x, y0), Vec2::new(bar.width(), h));
        painter.rect_stroke(r, 2.0, Stroke::new(1.0, pal.weak), egui::StrokeKind::Inside);
    }
    let response = ui.interact(bar, ui.id().with("overview"), Sense::click_and_drag());
    if (response.clicked() || response.dragged())
        && let Some(pos) = response.interact_pointer_pos()
    {
        let frac = ((pos.y - bar.min.y) / bar.height()).clamp(0.0, 1.0);
        *scroll_to_row = Some((frac * total) as usize);
    }
    response.on_hover_text("Change overview: click to jump");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn job_splits_at_span_and_inline_boundaries() {
        let font = FontId::monospace(12.0);
        let spans = [
            Span { start: 0, end: 6, rgb: [1, 2, 3], bold: false, italic: false },
            Span { start: 6, end: 12, rgb: [4, 5, 6], bold: false, italic: false },
        ];
        let job = line_job("return\t$x;!", &spans, std::slice::from_ref(&(7..9)), &font, Color32::WHITE, Color32::RED);
        assert_eq!(job.text, "return    $x;!");
        let backgrounds: Vec<_> = job.sections.iter().map(|s| s.format.background).collect();
        assert!(backgrounds.contains(&Color32::RED));
        assert_eq!(job.sections[0].format.color, Color32::from_rgb(1, 2, 3));
    }
}
