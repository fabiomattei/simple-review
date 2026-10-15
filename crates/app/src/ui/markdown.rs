//! A small Markdown renderer for comment bodies: paragraphs, emphasis, inline code, code blocks,
//! headings, lists, quotes and links.

use egui::text::LayoutJob;
use egui::{Color32, FontId, RichText, Stroke, TextFormat, TextStyle, Ui};
use pulldown_cmark::{CodeBlockKind, Event, HeadingLevel, Options, Parser, Tag, TagEnd};

use super::palette;

#[derive(Default)]
struct Style {
    strong: usize,
    emphasis: usize,
    strike: usize,
    link: usize,
    heading: Option<HeadingLevel>,
}

pub fn show(ui: &mut Ui, text: &str) {
    let pal = palette(ui);
    let body = TextStyle::Body.resolve(ui.style());
    let mono = TextStyle::Monospace.resolve(ui.style());
    let mut job = LayoutJob::default();
    let mut style = Style::default();
    let mut lists: Vec<Option<u64>> = Vec::new();
    let mut quote = 0usize;
    let mut code_block: Option<String> = None;

    let flush = |ui: &mut Ui, job: &mut LayoutJob, quote: usize| {
        if job.text.is_empty() {
            return;
        }
        let mut j = std::mem::take(job);
        j.wrap.max_width = ui.available_width() - 12.0 * quote as f32;
        if quote > 0 {
            ui.horizontal(|ui| {
                ui.add_space(4.0);
                let (rect, _) = ui.allocate_exact_size(egui::vec2(3.0, 16.0), egui::Sense::hover());
                ui.painter().rect_filled(rect, 1.0, pal.weak);
                ui.label(j);
            });
        } else {
            ui.label(j);
        }
    };

    for event in Parser::new_ext(text, Options::ENABLE_STRIKETHROUGH) {
        match event {
            Event::Start(Tag::CodeBlock(kind)) => {
                flush(ui, &mut job, quote);
                let _ = kind == CodeBlockKind::Indented;
                code_block = Some(String::new());
            }
            Event::End(TagEnd::CodeBlock) => {
                if let Some(code) = code_block.take() {
                    egui::Frame::new()
                        .fill(ui.visuals().extreme_bg_color)
                        .inner_margin(6.0)
                        .corner_radius(4.0)
                        .show(ui, |ui| {
                            ui.label(RichText::new(code.trim_end()).monospace());
                        });
                }
            }
            Event::Text(t) if code_block.is_some() => code_block.as_mut().unwrap().push_str(&t),
            Event::Start(Tag::Paragraph) => {}
            Event::End(TagEnd::Paragraph) => flush(ui, &mut job, quote),
            Event::Start(Tag::Heading { level, .. }) => {
                flush(ui, &mut job, quote);
                style.heading = Some(level);
            }
            Event::End(TagEnd::Heading(_)) => {
                flush(ui, &mut job, quote);
                style.heading = None;
            }
            Event::Start(Tag::BlockQuote(_)) => {
                flush(ui, &mut job, quote);
                quote += 1;
            }
            Event::End(TagEnd::BlockQuote(_)) => {
                flush(ui, &mut job, quote);
                quote = quote.saturating_sub(1);
            }
            Event::Start(Tag::List(start)) => {
                flush(ui, &mut job, quote);
                lists.push(start);
            }
            Event::End(TagEnd::List(_)) => {
                flush(ui, &mut job, quote);
                lists.pop();
            }
            Event::Start(Tag::Item) => {
                flush(ui, &mut job, quote);
                let indent = "    ".repeat(lists.len().saturating_sub(1));
                let bullet = match lists.last_mut() {
                    Some(Some(n)) => {
                        let b = format!("{indent}{n}. ");
                        *n += 1;
                        b
                    }
                    _ => format!("{indent}• "),
                };
                job.append(&bullet, 0.0, TextFormat { font_id: body.clone(), color: pal.weak, ..Default::default() });
            }
            Event::End(TagEnd::Item) => flush(ui, &mut job, quote),
            Event::Start(Tag::Strong) => style.strong += 1,
            Event::End(TagEnd::Strong) => style.strong = style.strong.saturating_sub(1),
            Event::Start(Tag::Emphasis) => style.emphasis += 1,
            Event::End(TagEnd::Emphasis) => style.emphasis = style.emphasis.saturating_sub(1),
            Event::Start(Tag::Strikethrough) => style.strike += 1,
            Event::End(TagEnd::Strikethrough) => style.strike = style.strike.saturating_sub(1),
            Event::Start(Tag::Link { .. }) => style.link += 1,
            Event::End(TagEnd::Link) => style.link = style.link.saturating_sub(1),
            Event::Text(t) => append(&mut job, &t, &style, &body, pal.text, pal.accent, false),
            Event::Code(t) => append(&mut job, &t, &style, &mono, pal.text, pal.accent, true),
            Event::SoftBreak => job.append(" ", 0.0, TextFormat { font_id: body.clone(), ..Default::default() }),
            Event::HardBreak => job.append("\n", 0.0, TextFormat { font_id: body.clone(), ..Default::default() }),
            Event::Rule => {
                flush(ui, &mut job, quote);
                ui.separator();
            }
            _ => {}
        }
    }
    flush(ui, &mut job, quote);
}

fn append(job: &mut LayoutJob, text: &str, style: &Style, font: &FontId, text_colour: Color32, accent: Color32, code: bool) {
    let mut font = font.clone();
    if let Some(level) = style.heading {
        font.size *= match level {
            HeadingLevel::H1 => 1.4,
            HeadingLevel::H2 => 1.25,
            _ => 1.1,
        };
    }
    let colour = if style.link > 0 { accent } else if style.strong > 0 || style.heading.is_some() { text_colour } else { text_colour.gamma_multiply(0.92) };
    let format = TextFormat {
        font_id: font,
        color: colour,
        italics: style.emphasis > 0,
        background: if code { Color32::from_gray(128).gamma_multiply(0.25) } else { Color32::TRANSPARENT },
        underline: if style.link > 0 { Stroke::new(1.0, accent) } else { Stroke::NONE },
        strikethrough: if style.strike > 0 { Stroke::new(1.0, text_colour) } else { Stroke::NONE },
        ..Default::default()
    };
    job.append(text, 0.0, format);
}
