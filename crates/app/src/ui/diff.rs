//! The side-by-side / unified diff view.
//!
//! Rows are painted directly (only the visible ones), so both panes share one vertical scroll
//! position and stay aligned. Horizontal scrolling is shared too.

use std::hash::{Hash, Hasher};
use std::ops::Range;

use egui::text::LayoutJob;
use egui::{
    Align2, Color32, CursorIcon, FontId, Pos2, Rect, Sense, Stroke, TextFormat, TextStyle, Ui, UiBuilder, Vec2, pos2,
    vec2,
};
use review_core::blame::{Blame, BlameCommit};
use review_core::diff::{Cell, LineKind, Row};
use review_core::git::format_time;
use review_core::highlight::{Highlighted, Span};

use super::{Action, Palette, palette};
use crate::state::{Cursor, Side};

pub struct DiffInput<'a> {
    pub rows: &'a [Row],
    pub hunks: &'a [Range<usize>],
    pub old: &'a [String],
    pub new: &'a [String],
    pub hl_old: Option<&'a Highlighted>,
    pub hl_new: Option<&'a Highlighted>,
    pub unified: bool,
    /// One pane with one line-number column (browsing a file, not a diff).
    pub single: bool,
    pub current_hunk: Option<usize>,
    pub max_line_chars: usize,
    /// `Some` when the blame gutter is shown; a side's blame may still be loading.
    pub blame: Option<BlameInput<'a>>,
    pub cursor: Option<Cursor>,
}

#[derive(Clone, Copy)]
pub struct BlameInput<'a> {
    pub old: Option<&'a Blame>,
    pub new: Option<&'a Blame>,
    pub by_committer: bool,
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
const TAB_WIDTH: usize = 4;
/// Blame gutter: two initials and two marker characters.
const BLAME_CHARS: f32 = 4.0;

struct Metrics {
    font: FontId,
    char_w: f32,
    digits: usize,
    blame: bool,
}

impl Metrics {
    fn blame_w(&self) -> f32 {
        if self.blame { BLAME_CHARS * self.char_w + 10.0 } else { 0.0 }
    }

    fn number_w(&self) -> f32 {
        self.digits as f32 * self.char_w + 14.0
    }

    fn marker_w(&self) -> f32 {
        self.char_w + 8.0
    }
}

/// Horizontal layout of one pane.
struct Pane {
    rect: Rect,
    blame_x: f32,
    /// Right edges of the line-number columns.
    numbers: Vec<f32>,
    marker_x: f32,
    text_x: f32,
}

impl Pane {
    fn new(m: &Metrics, rect: Rect, number_columns: usize, marker: bool) -> Self {
        let blame_x = rect.min.x;
        let mut x = blame_x + m.blame_w();
        let numbers = (0..number_columns)
            .map(|_| {
                x += m.number_w();
                x - 8.0
            })
            .collect();
        let marker_x = x;
        if marker {
            x += m.marker_w();
        }
        Pane { rect, blame_x, numbers, marker_x, text_x: x }
    }

    fn text_rect(&self, row: Rect) -> Rect {
        Rect::from_min_max(pos2(self.text_x, row.min.y), pos2(self.rect.max.x, row.max.y))
    }
}

/// The panes for a given content rect.
fn panes(m: &Metrics, rect: Rect, input: &DiffInput<'_>) -> Vec<(Side, Pane)> {
    if input.single {
        vec![(Side::New, Pane::new(m, rect, 1, false))]
    } else if input.unified {
        vec![(Side::New, Pane::new(m, rect, 2, true))]
    } else {
        let half = rect.width() / 2.0;
        let left = Rect::from_min_size(rect.min, vec2(half, rect.height()));
        let right = Rect::from_min_max(pos2(rect.min.x + half, rect.min.y), rect.max);
        vec![(Side::Old, Pane::new(m, left, 1, true)), (Side::New, Pane::new(m, right, 1, true))]
    }
}

/// The cell a pane shows for a row, with the side it belongs to.
fn cell_for<'r>(row: &'r Row, pane_side: Side, input: &DiffInput<'_>) -> Option<(&'r Cell, Side)> {
    if input.single || input.unified {
        // Context rows carry both sides; show the new one. Change rows carry exactly one side.
        match (&row.left, &row.right) {
            (_, Some(r)) => Some((r, Side::New)),
            (Some(l), None) => Some((l, Side::Old)),
            _ => None,
        }
    } else {
        match pane_side {
            Side::Old => row.left.as_ref().map(|c| (c, Side::Old)),
            Side::New => row.right.as_ref().map(|c| (c, Side::New)),
        }
    }
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
    let m = Metrics { font, char_w, digits, blame: input.blame.is_some() };

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
    let probe = panes(&m, main, input);
    let text_area_w = probe.last().map_or(main.width(), |(_, p)| p.rect.max.x - p.text_x);
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
                let panes = panes(&m, rows_rect, input);
                let response = ui.interact(rows_rect, ui.id().with("rows"), Sense::click());
                handle_pointer(ui, &response, &m, &panes, input, origin, row_h, *h_scroll, actions);

                let first = (viewport.min.y / row_h).floor().max(0.0) as usize;
                let last = ((viewport.max.y / row_h).ceil() as usize + 1).min(total);
                let current = input.current_hunk.and_then(|h| input.hunks.get(h)).cloned().unwrap_or(0..0);
                let painter = ui.painter();
                let separator = ui.visuals().widgets.noninteractive.bg_stroke.color;
                for i in first..last {
                    let y = origin.y + i as f32 * row_h;
                    let row = &input.rows[i];
                    for (pane_side, pane) in &panes {
                        let rect = Rect::from_min_max(pos2(pane.rect.min.x, y), pos2(pane.rect.max.x, y + row_h));
                        let cell = cell_for(row, *pane_side, input);
                        paint_cell(painter, &m, &pal, pane, rect, row, cell, input, *h_scroll, i == first);
                        if *pane_side == Side::New && panes.len() == 2 {
                            painter.line_segment([pos2(rect.min.x, y), pos2(rect.min.x, y + row_h)], Stroke::new(1.0, separator));
                        }
                    }
                    if current.contains(&i) {
                        painter.rect_filled(Rect::from_min_size(pos2(origin.x, y), vec2(3.0, row_h)), 0.0, pal.accent);
                    }
                }
            })
        })
        .inner;
    let offset = output.state.offset.y;
    ui.data_mut(|d| d.insert_temp(offset_id, offset));

    paint_overview(ui, &pal, bar, input, offset, main.height(), content_h, scroll_to_row);
}

/// What is under the pointer.
enum Hit<'a> {
    Text { cursor: Cursor },
    Blame { commit: &'a BlameCommit },
    Other,
}

#[allow(clippy::too_many_arguments)]
fn hit_test<'a>(
    pos: Pos2,
    m: &Metrics,
    panes: &[(Side, Pane)],
    input: &'a DiffInput<'_>,
    origin: Pos2,
    row_h: f32,
    h_scroll: f32,
) -> (Option<usize>, Hit<'a>) {
    let row_idx = ((pos.y - origin.y) / row_h).floor();
    if row_idx < 0.0 || row_idx as usize >= input.rows.len() {
        return (None, Hit::Other);
    }
    let row_idx = row_idx as usize;
    let Some((pane_side, pane)) = panes.iter().find(|(_, p)| p.rect.x_range().contains(pos.x)) else {
        return (Some(row_idx), Hit::Other);
    };
    let Some((cell, side)) = cell_for(&input.rows[row_idx], *pane_side, input) else {
        return (Some(row_idx), Hit::Other);
    };
    if m.blame && pos.x < pane.blame_x + m.blame_w() {
        let blame = input.blame.and_then(|b| if side == Side::Old { b.old } else { b.new });
        return match blame.and_then(|b| b.commit_for_line(cell.line as usize)) {
            Some(commit) => (Some(row_idx), Hit::Blame { commit }),
            None => (Some(row_idx), Hit::Other),
        };
    }
    if pos.x >= pane.text_x {
        let lines = if side == Side::Old { input.old } else { input.new };
        let text = lines.get(cell.line as usize).map_or("", String::as_str);
        let display_col = ((pos.x - pane.text_x + h_scroll) / m.char_w).floor().max(0.0) as usize;
        let col = byte_at_display_col(text, display_col) as u32;
        return (Some(row_idx), Hit::Text { cursor: Cursor { side, line: cell.line, col } });
    }
    (Some(row_idx), Hit::Other)
}

#[allow(clippy::too_many_arguments)]
fn handle_pointer(
    ui: &Ui,
    response: &egui::Response,
    m: &Metrics,
    panes: &[(Side, Pane)],
    input: &DiffInput<'_>,
    origin: Pos2,
    row_h: f32,
    h_scroll: f32,
    actions: &mut Vec<Action>,
) {
    let command = ui.input(|i| i.modifiers.command);
    if let Some(pos) = response.hover_pos() {
        match hit_test(pos, m, panes, input, origin, row_h, h_scroll).1 {
            Hit::Blame { commit } => {
                ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
                response.clone().on_hover_ui_at_pointer(|ui| blame_tooltip(ui, commit));
            }
            Hit::Text { .. } if command => ui.ctx().set_cursor_icon(CursorIcon::PointingHand),
            _ => {}
        }
    }
    if response.clicked()
        && let Some(pos) = response.interact_pointer_pos()
    {
        let (row, hit) = hit_test(pos, m, panes, input, origin, row_h, h_scroll);
        if let Some(row) = row {
            actions.push(Action::ClickRow(row));
        }
        match hit {
            Hit::Text { cursor } => {
                actions.push(Action::SetCursor(cursor));
                if command {
                    actions.push(Action::GoToDefinition);
                }
            }
            Hit::Blame { commit } => actions.push(Action::OpenCommit { id: commit.id, path: Some(commit.path.clone()) }),
            Hit::Other => {}
        }
    }
}

pub fn blame_tooltip(ui: &mut Ui, c: &BlameCommit) {
    let pal = palette(ui);
    ui.horizontal(|ui| {
        ui.monospace(c.id.to_hex_with_len(10).to_string());
        if c.ai_assisted {
            ui.colored_label(pal.ai, "AI-assisted");
        }
        if c.boundary {
            ui.weak("(boundary)");
        }
    });
    ui.strong(&c.summary);
    ui.label(format!("Author: {} <{}>  {}", c.author.name, c.author.email, format_time(&c.author)));
    let committer = format!("Committer: {} <{}>  {}", c.committer.name, c.committer.email, format_time(&c.committer));
    if c.committer_differs() {
        ui.colored_label(pal.warn, format!("* {committer}"));
    } else {
        ui.label(committer);
    }
    for co in &c.co_authors {
        ui.label(format!("Co-author: {co}"));
    }
    ui.weak("Click to open this commit");
}

/// Byte offset of the character at a display column (tabs count as four columns).
pub fn byte_at_display_col(text: &str, col: usize) -> usize {
    let mut x = 0;
    for (i, c) in text.char_indices() {
        let w = if c == '\t' { TAB_WIDTH } else { 1 };
        if col < x + w {
            return i;
        }
        x += w;
    }
    text.len()
}

/// A stable colour per person.
pub fn person_colour(identity: &str, dark: bool) -> Color32 {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    identity.to_ascii_lowercase().hash(&mut h);
    let hue = (h.finish() % 360) as f32 / 360.0;
    let (s, v) = if dark { (0.55, 0.85) } else { (0.75, 0.6) };
    egui::ecolor::Hsva::new(hue, s, v, 1.0).into()
}

pub fn initials(name: &str) -> String {
    let words: Vec<&str> = name.split_whitespace().collect();
    let s: String = match words.as_slice() {
        [] => "?".into(),
        [one] => one.chars().take(2).collect(),
        [first, .., last] => first.chars().take(1).chain(last.chars().take(1)).collect(),
    };
    s.to_uppercase()
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
    pane: &Pane,
    rect: Rect,
    row: &Row,
    cell: Option<(&Cell, Side)>,
    input: &DiffInput<'_>,
    h_scroll: f32,
    first_visible: bool,
) {
    let Some((cell, side)) = cell else {
        painter.rect_filled(rect, 0.0, pal.filler);
        return;
    };
    if let Some(bg) = kind_bg(pal, cell.kind) {
        painter.rect_filled(rect, 0.0, bg);
    }
    if input.cursor.is_some_and(|c| c.side == side && c.line == cell.line) {
        painter.rect_filled(rect, 0.0, pal.cursor_line);
    }
    let y = rect.center().y;

    if let Some(blame) = input.blame {
        let b = if side == Side::Old { blame.old } else { blame.new };
        if let Some(b) = b {
            paint_blame(painter, m, pal, pane, rect, b, cell.line as usize, blame.by_committer, first_visible);
        }
    }

    // Line numbers: unified rows show old and new numbers in separate columns.
    let numbers: Vec<Option<u32>> = if pane.numbers.len() == 2 {
        vec![row.left.as_ref().map(|c| c.line), row.right.as_ref().map(|c| c.line)]
    } else {
        vec![Some(cell.line)]
    };
    for (x, n) in pane.numbers.iter().zip(numbers) {
        if let Some(n) = n {
            painter.text(pos2(*x, y), Align2::RIGHT_CENTER, n + 1, m.font.clone(), pal.weak);
        }
    }
    if !input.single {
        painter.text(pos2(pane.marker_x + 2.0, y), Align2::LEFT_CENTER, marker(cell.kind, side), m.font.clone(), pal.weak);
    }
    let (lines, hl) = match side {
        Side::Old => (input.old, input.hl_old),
        Side::New => (input.new, input.hl_new),
    };
    paint_text(painter, m, pal, pane.text_rect(rect), cell, lines, hl, h_scroll);
}

#[allow(clippy::too_many_arguments)]
fn paint_blame(
    painter: &egui::Painter,
    m: &Metrics,
    pal: &Palette,
    pane: &Pane,
    rect: Rect,
    blame: &Blame,
    line: usize,
    by_committer: bool,
    first_visible: bool,
) {
    let Some(&idx) = blame.lines.get(line) else { return };
    let c = &blame.commits[idx as usize];
    let who = if by_committer { &c.committer } else { &c.author };
    let dark = pal.dark;
    let colour = person_colour(&who.email, dark);
    let gutter = Rect::from_min_max(pos2(pane.blame_x, rect.min.y), pos2(pane.blame_x + m.blame_w() - 4.0, rect.max.y));
    painter.rect_filled(Rect::from_min_size(gutter.min, vec2(3.0, rect.height())), 0.0, colour);
    let group_start = line == 0 || blame.lines.get(line - 1) != Some(&idx);
    if group_start || first_visible {
        let mut label = initials(&who.name);
        label.push(if c.committer_differs() { '*' } else { ' ' });
        label.push(if c.ai_assisted { '^' } else { ' ' });
        painter.text(pos2(gutter.min.x + 6.0, rect.center().y), Align2::LEFT_CENTER, label, m.font.clone(), colour);
    }
    if group_start && line > 0 {
        painter.line_segment([pos2(gutter.min.x, rect.min.y), pos2(gutter.max.x, rect.min.y)], Stroke::new(1.0, pal.filler));
    }
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

    #[test]
    fn display_columns_map_to_bytes() {
        assert_eq!(byte_at_display_col("\tfoo", 0), 0);
        assert_eq!(byte_at_display_col("\tfoo", 3), 0);
        assert_eq!(byte_at_display_col("\tfoo", 4), 1);
        assert_eq!(byte_at_display_col("é$x", 1), 2);
        assert_eq!(byte_at_display_col("ab", 9), 2);
    }

    #[test]
    fn initials_from_names() {
        assert_eq!(initials("Alice Admin"), "AA");
        assert_eq!(initials("Mary Ann Smith"), "MS");
        assert_eq!(initials("bob"), "BO");
    }
}
