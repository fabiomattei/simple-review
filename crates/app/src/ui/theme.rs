//! The app's look: a macOS-style light and dark theme (grey sidebar, white
//! content, blue accent, rounded controls) and the widgets the window chrome
//! is drawn with (segmented controls, pill buttons, list rows, section titles).

use std::sync::Arc;

use egui::{Color32, CornerRadius, FontFamily, FontId, Margin, RichText, Shadow, Stroke, TextStyle, Theme, Vec2};

/// Colours the chrome uses beyond egui's own visuals.
#[derive(Clone, Copy)]
pub struct Chrome {
    pub sidebar: Color32,
    pub content: Color32,
    pub separator: Color32,
    pub accent: Color32,
    /// The selected row in a sidebar list.
    pub selected: Color32,
    /// Behind segmented controls and search fields.
    pub well: Color32,
    pub muted: Color32,
}

pub fn chrome(ui: &egui::Ui) -> Chrome {
    chrome_for(ui.visuals().dark_mode)
}

fn chrome_for(dark: bool) -> Chrome {
    if dark {
        Chrome {
            sidebar: Color32::from_rgb(0x2b, 0x2b, 0x2d),
            content: Color32::from_rgb(0x1e, 0x1e, 0x1f),
            separator: Color32::from_rgb(0x3a, 0x3a, 0x3c),
            accent: Color32::from_rgb(0x0a, 0x84, 0xff),
            selected: Color32::from_rgb(0x46, 0x46, 0x4a),
            well: Color32::from_rgb(0x38, 0x38, 0x3b),
            muted: Color32::from_gray(0x98),
        }
    } else {
        Chrome {
            sidebar: Color32::from_rgb(0xec, 0xec, 0xec),
            content: Color32::WHITE,
            separator: Color32::from_gray(0xdd),
            accent: Color32::from_rgb(0x00, 0x7a, 0xff),
            selected: Color32::from_gray(0xd8),
            well: Color32::from_gray(0xe2),
            muted: Color32::from_gray(0x86),
        }
    }
}

/// The semibold face, for titles and section headers.
pub fn bold(size: f32) -> FontId {
    FontId::new(size, FontFamily::Name("bold".into()))
}

/// Fonts and both themes' styles; egui picks one by the system or the preferences.
pub fn install(ctx: &egui::Context) {
    let mut fonts = egui::FontDefinitions::default();
    fonts.font_data.insert("inter".into(), Arc::new(egui::FontData::from_static(include_bytes!("../../assets/fonts/Inter-Regular.ttf"))));
    fonts
        .font_data
        .insert("inter-semibold".into(), Arc::new(egui::FontData::from_static(include_bytes!("../../assets/fonts/Inter-SemiBold.ttf"))));
    let proportional = fonts.families.entry(FontFamily::Proportional).or_default();
    proportional.insert(0, "inter".into());
    // Symbols Inter lacks come from egui's own fonts, after it.
    let fallbacks: Vec<String> = proportional.iter().skip(1).cloned().collect();
    fonts.families.insert(FontFamily::Name("bold".into()), std::iter::once("inter-semibold".to_string()).chain(fallbacks).collect());
    ctx.set_fonts(fonts);

    for theme in [Theme::Light, Theme::Dark] {
        ctx.style_mut_of(theme, |style| {
            style.text_styles = [
                (TextStyle::Small, FontId::proportional(10.5)),
                (TextStyle::Body, FontId::proportional(13.0)),
                (TextStyle::Button, FontId::proportional(13.0)),
                (TextStyle::Heading, bold(18.0)),
                (TextStyle::Monospace, FontId::monospace(12.5)),
            ]
            .into();
            let s = &mut style.spacing;
            s.item_spacing = Vec2::new(8.0, 5.0);
            s.button_padding = Vec2::new(9.0, 3.0);
            s.interact_size.y = 22.0;
            s.menu_margin = Margin::same(6);
            s.window_margin = Margin::same(16);
            s.scroll = egui::style::ScrollStyle::floating();
            style.visuals = visuals(theme == Theme::Dark);
        });
    }
}

fn visuals(dark: bool) -> egui::Visuals {
    let p = chrome_for(dark);
    let mut v = if dark { egui::Visuals::dark() } else { egui::Visuals::light() };
    let gray = Color32::from_gray;
    let w = &mut v.widgets;
    if dark {
        w.noninteractive.bg_fill = p.content;
        w.noninteractive.weak_bg_fill = p.content;
        w.noninteractive.bg_stroke = Stroke::new(1.0, p.separator);
        w.noninteractive.fg_stroke = Stroke::new(1.0, gray(0xdc));
        w.inactive.weak_bg_fill = gray(0x3a);
        w.inactive.bg_fill = gray(0x46);
        w.inactive.bg_stroke = Stroke::new(1.0, gray(0x48));
        w.inactive.fg_stroke = Stroke::new(1.0, gray(0xe4));
        w.hovered.weak_bg_fill = gray(0x46);
        w.hovered.bg_fill = gray(0x50);
        w.hovered.bg_stroke = Stroke::new(1.0, gray(0x5a));
        w.hovered.fg_stroke = Stroke::new(1.5, Color32::WHITE);
        w.active.weak_bg_fill = gray(0x55);
        w.active.bg_fill = gray(0x5c);
        w.active.bg_stroke = Stroke::new(1.0, gray(0x6a));
        w.active.fg_stroke = Stroke::new(1.5, Color32::WHITE);
        w.open.weak_bg_fill = gray(0x40);
        w.open.bg_fill = p.content;
        w.open.bg_stroke = Stroke::new(1.0, gray(0x50));
        v.selection.bg_fill = Color32::from_rgb(0x1f, 0x4f, 0x86);
        v.selection.stroke = Stroke::new(1.0, Color32::from_rgb(0xb4, 0xd5, 0xff));
        v.faint_bg_color = Color32::from_rgb(0x26, 0x26, 0x28);
        v.code_bg_color = gray(0x33);
        v.warn_fg_color = Color32::from_rgb(0xff, 0x9f, 0x0a);
        v.error_fg_color = Color32::from_rgb(0xff, 0x45, 0x3a);
        v.window_shadow = Shadow { offset: [0, 10], blur: 30, spread: 0, color: Color32::from_black_alpha(110) };
        v.popup_shadow = Shadow { offset: [0, 6], blur: 16, spread: 0, color: Color32::from_black_alpha(90) };
        v.window_stroke = Stroke::new(1.0, gray(0x48));
        v.window_fill = Color32::from_rgb(0x28, 0x28, 0x2a);
    } else {
        w.noninteractive.bg_fill = p.content;
        w.noninteractive.weak_bg_fill = p.content;
        w.noninteractive.bg_stroke = Stroke::new(1.0, p.separator);
        w.noninteractive.fg_stroke = Stroke::new(1.0, Color32::from_rgb(0x1d, 0x1d, 0x1f));
        w.inactive.weak_bg_fill = Color32::WHITE;
        w.inactive.bg_fill = gray(0xe6);
        w.inactive.bg_stroke = Stroke::new(1.0, gray(0xd2));
        w.inactive.fg_stroke = Stroke::new(1.0, gray(0x26));
        w.hovered.weak_bg_fill = gray(0xf2);
        w.hovered.bg_fill = gray(0xdc);
        w.hovered.bg_stroke = Stroke::new(1.0, gray(0xb8));
        w.hovered.fg_stroke = Stroke::new(1.5, Color32::BLACK);
        w.active.weak_bg_fill = gray(0xe0);
        w.active.bg_fill = gray(0xcc);
        w.active.bg_stroke = Stroke::new(1.0, gray(0xa8));
        w.active.fg_stroke = Stroke::new(1.5, Color32::BLACK);
        w.open.weak_bg_fill = gray(0xea);
        w.open.bg_fill = Color32::WHITE;
        w.open.bg_stroke = Stroke::new(1.0, gray(0xc8));
        v.selection.bg_fill = Color32::from_rgb(0xb3, 0xd7, 0xff);
        v.selection.stroke = Stroke::new(1.0, Color32::from_rgb(0x00, 0x4f, 0xb0));
        v.faint_bg_color = gray(0xf5);
        v.code_bg_color = gray(0xee);
        v.warn_fg_color = Color32::from_rgb(0xc4, 0x6a, 0x00);
        v.error_fg_color = Color32::from_rgb(0xd7, 0x00, 0x15);
        v.window_shadow = Shadow { offset: [0, 10], blur: 30, spread: 0, color: Color32::from_black_alpha(45) };
        v.popup_shadow = Shadow { offset: [0, 6], blur: 16, spread: 0, color: Color32::from_black_alpha(35) };
        v.window_stroke = Stroke::new(1.0, gray(0xd0));
        v.window_fill = Color32::WHITE;
    }
    for wv in [&mut w.noninteractive, &mut w.inactive, &mut w.hovered, &mut w.active, &mut w.open] {
        wv.corner_radius = CornerRadius::same(6);
        wv.expansion = 0.0;
    }
    v.panel_fill = p.content;
    v.extreme_bg_color = p.content;
    v.text_edit_bg_color = Some(p.content);
    v.hyperlink_color = p.accent;
    v.window_corner_radius = CornerRadius::same(12);
    v.menu_corner_radius = CornerRadius::same(8);
    v.indent_has_left_vline = false;
    v
}

/// A segmented control: one rounded track, the chosen segment filled with the
/// accent; `fill` stretches it to the full width. Returns the segment clicked, if any.
pub fn segmented<T: Copy + PartialEq>(ui: &mut egui::Ui, current: T, items: &[(T, &str, &str)], fill: bool) -> Option<T> {
    let p = chrome(ui);
    let mut clicked = None;
    egui::Frame::new().fill(p.well).corner_radius(CornerRadius::same(7)).inner_margin(Margin::same(2)).show(ui, |ui| {
        ui.spacing_mut().item_spacing.x = 2.0;
        let width = if fill { (ui.available_width() - 2.0 * (items.len() as f32 - 1.0)) / items.len() as f32 } else { 0.0 };
        ui.horizontal(|ui| {
            for &(value, text, hover) in items {
                let on = value == current;
                let text = if on { RichText::new(text).color(Color32::WHITE) } else { RichText::new(text) };
                let b = egui::Button::new(text)
                    .corner_radius(CornerRadius::same(5))
                    .stroke(Stroke::NONE)
                    .min_size(Vec2::new(width, 20.0))
                    .fill(if on { p.accent } else { Color32::TRANSPARENT });
                let resp = ui.add(b);
                let resp = if hover.is_empty() { resp } else { resp.on_hover_text(hover) };
                if resp.clicked() {
                    clicked = Some(value);
                }
            }
        });
    });
    clicked
}

/// A round-cornered toolbar button with a light border, like the ones in a macOS title bar.
pub fn pill(ui: &mut egui::Ui, text: impl Into<egui::WidgetText>) -> egui::Response {
    ui.add(egui::Button::new(text).corner_radius(CornerRadius::same(13)).min_size(Vec2::new(28.0, 26.0)))
}

/// A pill that stays tinted with the accent while `on`; for view toggles.
pub fn toggle_pill(ui: &mut egui::Ui, on: bool, text: &str) -> egui::Response {
    let p = chrome(ui);
    let mut b = egui::Button::new(if on { RichText::new(text).color(Color32::WHITE) } else { RichText::new(text) })
        .corner_radius(CornerRadius::same(13))
        .min_size(Vec2::new(28.0, 26.0));
    if on {
        b = b.fill(p.accent).stroke(Stroke::new(1.0, p.accent));
    }
    ui.add(b)
}

/// A pill holding a drop-down menu.
pub fn pill_menu<R>(ui: &mut egui::Ui, text: impl Into<egui::WidgetText>, add: impl FnOnce(&mut egui::Ui) -> R) -> egui::Response {
    let button = egui::Button::new(text).corner_radius(CornerRadius::same(13)).min_size(Vec2::new(28.0, 26.0));
    egui::containers::menu::MenuButton::from_button(button).ui(ui, add).0
}

/// A rounded search field with a magnifier, on the well colour.
pub fn search_field(ui: &mut egui::Ui, text: &mut String, hint: &str) -> egui::Response {
    let p = chrome(ui);
    egui::Frame::new()
        .fill(p.well)
        .corner_radius(CornerRadius::same(7))
        .inner_margin(Margin::symmetric(8, 3))
        .show(ui, |ui| {
            let row = Vec2::new(ui.available_width(), ui.spacing().interact_size.y);
            ui.allocate_ui_with_layout(row, egui::Layout::left_to_right(egui::Align::Center), |ui| {
                ui.spacing_mut().item_spacing.x = 4.0;
                ui.label(RichText::new("🔍").color(p.muted));
                let resp = ui.add(egui::TextEdit::singleline(text).hint_text(hint).frame(egui::Frame::NONE).desired_width(ui.available_width()));
                if !text.is_empty() && resp.has_focus() && ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                    text.clear();
                }
                resp
            })
            .inner
        })
        .inner
}

/// The rounded highlight behind a sidebar row: filled when selected, faint when hovered.
pub fn row_highlight(ui: &egui::Ui, rect: egui::Rect, selected: bool, hovered: bool) {
    let p = chrome(ui);
    if selected {
        ui.painter().rect_filled(rect, CornerRadius::same(6), p.selected);
    } else if hovered {
        ui.painter().rect_filled(rect, CornerRadius::same(6), p.selected.gamma_multiply(0.45));
    }
}

/// A small grey section title (Commits, Files …) in the sidebar.
pub fn section(ui: &mut egui::Ui, title: &str) {
    ui.label(RichText::new(title.to_uppercase()).font(bold(10.5)).color(chrome(ui).muted));
}

/// A small rounded tag in `colour` (AI, merge, routes …), painted at `right`
/// ending on the given baseline centre `y`. Returns its left edge.
pub fn paint_tag(painter: &egui::Painter, right: f32, y: f32, label: &str, colour: Color32) -> f32 {
    let galley = painter.layout_no_wrap(label.to_string(), bold(10.0), colour);
    let r = egui::Rect::from_min_size(egui::pos2(right - galley.size().x - 10.0, y - 8.0), Vec2::new(galley.size().x + 10.0, 16.0));
    painter.rect_filled(r, CornerRadius::same(8), colour.gamma_multiply(0.16));
    painter.galley(egui::pos2(r.min.x + 5.0, r.center().y - galley.size().y / 2.0), galley, colour);
    r.min.x
}

/// Simple drawn icons, so they don't depend on what the fonts hold.
#[derive(Clone, Copy)]
pub enum Glyph {
    Folder,
    Branch,
}

/// `glyph` centred on `c`, `scale` times its 16 px size.
pub fn paint_glyph(painter: &egui::Painter, glyph: Glyph, c: egui::Pos2, scale: f32, color: Color32) {
    let stroke = Stroke::new(1.3 * scale.sqrt(), color);
    let v = |x: f32, y: f32| c + Vec2::new(x, y) * scale;
    match glyph {
        Glyph::Folder => {
            let pts = vec![v(-7.0, -5.0), v(-2.5, -5.0), v(-1.0, -3.0), v(7.0, -3.0), v(7.0, 5.5), v(-7.0, 5.5)];
            painter.add(egui::Shape::closed_line(pts, stroke));
            painter.line_segment([v(-7.0, -1.0), v(7.0, -1.0)], stroke);
        }
        Glyph::Branch => {
            // A main line with a branch curving off it, a dot at each end.
            let r = 1.8 * scale;
            painter.line_segment([v(-3.5, -4.0), v(-3.5, 4.0)], stroke);
            let curve: Vec<egui::Pos2> = (0..=10)
                .map(|i| i as f32 / 10.0)
                .map(|t| {
                    let (a, b, cc) = (v(4.0, -4.0), v(4.0, 2.0), v(-3.5, 2.0));
                    let p = a.to_vec2() * (1.0 - t) * (1.0 - t) + b.to_vec2() * 2.0 * t * (1.0 - t) + cc.to_vec2() * t * t;
                    p.to_pos2()
                })
                .collect();
            painter.add(egui::Shape::line(curve, stroke));
            for (x, y) in [(-3.5, -5.5), (-3.5, 5.5), (4.0, -5.5)] {
                painter.circle_stroke(v(x, y), r, stroke);
            }
        }
    }
}

/// A large clickable row for the welcome screen: `glyph` on a tinted rounded
/// square, a title and a line under it. Fills the width it is given.
pub fn card(ui: &mut egui::Ui, glyph: Glyph, tint: Color32, title: &str, subtitle: &str) -> egui::Response {
    let height = if subtitle.is_empty() { 44.0 } else { 54.0 };
    let (rect, resp) = ui.allocate_exact_size(Vec2::new(ui.available_width(), height), egui::Sense::click());
    let p = chrome(ui);
    let w = ui.style().interact(&resp);
    let fill = if resp.hovered() { p.selected.gamma_multiply(0.6) } else { Color32::TRANSPARENT };
    ui.painter().rect(rect, CornerRadius::same(10), fill, if resp.hovered() { w.bg_stroke } else { Stroke::NONE }, egui::StrokeKind::Inside);
    let square = egui::Rect::from_min_size(rect.left_center() + Vec2::new(8.0, -17.0), Vec2::splat(34.0));
    ui.painter().rect_filled(square, CornerRadius::same(8), tint);
    paint_glyph(ui.painter(), glyph, square.center(), 1.3, Color32::WHITE);
    let x = square.right() + 12.0;
    let text = ui.visuals().text_color();
    // One line each, cut with "…" when too long.
    let line = |text: &str, font: FontId, color: Color32| {
        let mut job = egui::text::LayoutJob::simple_singleline(text.to_string(), font, color);
        job.wrap = egui::text::TextWrapping::truncate_at_width(rect.right() - x - 10.0);
        ui.fonts_mut(|f| f.layout_job(job))
    };
    let title_galley = line(title, bold(14.0), text);
    let sub_galley = (!subtitle.is_empty()).then(|| line(subtitle, FontId::proportional(11.5), p.muted));
    let total = title_galley.size().y + sub_galley.as_ref().map_or(0.0, |g| g.size().y + 2.0);
    let mut y = rect.center().y - total / 2.0;
    let th = title_galley.size().y;
    ui.painter().galley(egui::pos2(x, y), title_galley, text);
    if let Some(g) = sub_galley {
        y += th + 2.0;
        ui.painter().galley(egui::pos2(x, y), g, p.muted);
    }
    resp
}
