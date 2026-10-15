//! The peek panel: definitions, references and line blame, shown under the diff.

use egui::{RichText, TextStyle, Ui};
use review_core::blame::BlameCommit;

use super::diff::{blame_tooltip, line_job};
use super::comments::CommentsView;
use super::{Action, palette};
use crate::state::{DefinitionState, Peek, PeekTab, ReferencesState};

pub struct BlameInfo<'a> {
    pub enabled: bool,
    pub loading: bool,
    /// 1-based line and its commit, for the cursor line.
    pub line: Option<(u32, Option<&'a BlameCommit>)>,
    pub by_author: Vec<(String, usize)>,
    pub by_committer: Vec<(String, usize)>,
    pub error: Option<&'a str>,
}

pub fn show(
    ui: &mut Ui,
    peek: &Peek,
    blame: &BlameInfo<'_>,
    comments: Option<&CommentsView>,
    quality: Option<&super::quality::QualityView>,
    actions: &mut Vec<Action>,
) {
    ui.horizontal(|ui| {
        for (tab, label, hint) in [
            (PeekTab::Definition, "Definition", "F12 / Cmd+click"),
            (PeekTab::References, "References", "Shift+F12"),
            (PeekTab::Blame, "Blame", "Line history for the cursor line"),
            (PeekTab::Comments, "Comments", "Review comments (c to add, . and , to step)"),
            (PeekTab::Quality, "Quality", "New warnings, secrets, complexity, coverage (q)"),
        ] {
            if ui.selectable_label(peek.tab == tab, label).on_hover_text(hint).clicked() {
                actions.push(Action::PeekTab(tab));
            }
        }
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if ui.small_button("×").on_hover_text("Close (Esc)").clicked() {
                actions.push(Action::ClosePeek);
            }
        });
    });
    ui.separator();
    match peek.tab {
        PeekTab::Definition => definition(ui, peek, actions),
        PeekTab::References => references(ui, peek, actions),
        PeekTab::Blame => blame_tab(ui, blame, actions),
        PeekTab::Quality => match quality {
            Some(view) => super::quality::show(ui, view, actions),
            None => {
                ui.weak("Select a commit.");
            }
        },
        PeekTab::Comments => match comments {
            Some(view) => super::comments::show(ui, view, actions),
            None => {
                ui.weak("Open a file to see its comments.");
            }
        },
    }
}

fn definition(ui: &mut Ui, peek: &Peek, actions: &mut Vec<Action>) {
    let pal = palette(ui);
    let Some(def) = &peek.definition else {
        ui.weak("Cmd/Ctrl+click a class, function, method or property name, or put the cursor on it and press F12.");
        return;
    };
    match &def.state {
        DefinitionState::Pending { .. } => {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label(format!("Indexing symbols at {}…", def.version.label));
            });
        }
        DefinitionState::NotFound(msg) => {
            ui.label(msg);
        }
        DefinitionState::LspPending { .. } => {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label(format!("Asking the language server at {}… (the first request starts it)", def.version.label));
            });
        }
        DefinitionState::Lsp { name, targets, selected } => {
            ui.horizontal(|ui| {
                ui.label(RichText::new(name).monospace().strong());
                ui.weak(format!("at {} · via language server", def.version.label));
            });
            if targets.len() > 1 {
                for (i, t) in targets.iter().enumerate() {
                    if ui.selectable_label(i == *selected, RichText::new(t.label()).monospace()).clicked() {
                        actions.push(Action::PeekSelect(i));
                    }
                }
                ui.separator();
            }
            if let Some(t) = targets.get(*selected) {
                ui.horizontal(|ui| {
                    ui.label(RichText::new(t.label()).monospace());
                    if t.rel.is_some() {
                        if ui.button("Open (o)").clicked() {
                            actions.push(Action::OpenDefinition);
                        }
                    } else {
                        ui.weak("outside the repository");
                    }
                });
            }
            snippet_view(ui, def.snippet.as_ref(), None);
        }
        DefinitionState::Ready { lookup, selected } => {
            ui.horizontal(|ui| {
                ui.label(RichText::new(&lookup.name).monospace().strong());
                ui.weak(format!("at {}", def.version.label));
                if lookup.on_definition {
                    ui.weak("· this is the definition; Shift+F12 lists references");
                }
            });
            if lookup.candidates.len() > 1 {
                ui.weak(format!("{} candidates (resolved by name):", lookup.candidates.len()));
                egui::ScrollArea::vertical().id_salt("def_candidates").max_height(90.0).show(ui, |ui| {
                    for (i, c) in lookup.candidates.iter().enumerate() {
                        let text = format!("{} {}  —  {}:{}", c.symbol.kind.label(), c.symbol.fqn, c.path, c.symbol.span.line + 1);
                        if ui.selectable_label(i == *selected, RichText::new(text).monospace()).clicked() {
                            actions.push(Action::PeekSelect(i));
                        }
                    }
                });
                ui.separator();
            }
            let Some(c) = lookup.candidates.get(*selected) else { return };
            ui.horizontal(|ui| {
                ui.label(RichText::new(c.symbol.kind.label()).color(pal.accent));
                ui.label(RichText::new(&c.symbol.fqn).monospace());
                ui.weak(format!("{}:{}", c.path, c.symbol.span.line + 1));
                if ui.button("Open (o)").clicked() {
                    actions.push(Action::OpenDefinition);
                }
            });
            let name = (c.symbol.span.line, c.symbol.span.start as usize..c.symbol.span.end as usize);
            snippet_view(ui, def.snippet.as_ref(), Some(name));
        }
    }
}

/// Source lines with numbers; `mark` highlights a byte range on one line.
fn snippet_view(ui: &mut Ui, snippet: Option<&crate::state::Snippet>, mark: Option<(u32, std::ops::Range<usize>)>) {
    let Some(snippet) = snippet else { return };
    let pal = palette(ui);
    let font = TextStyle::Monospace.resolve(ui.style());
    egui::ScrollArea::both().id_salt("def_snippet").auto_shrink([false, true]).show(ui, |ui| {
        for (i, line) in snippet.lines.iter().enumerate() {
            let n = snippet.first_line as usize + i;
            let spans = snippet.highlighted.as_ref().and_then(|h| h.get(i)).map(Vec::as_slice).unwrap_or(&[]);
            let inline = match &mark {
                Some((l, r)) if *l as usize == n => std::slice::from_ref(r),
                _ => &[],
            };
            ui.horizontal(|ui| {
                ui.add_sized([40.0, 14.0], egui::Label::new(RichText::new(format!("{}", n + 1)).monospace().color(pal.weak)));
                ui.label(line_job(line, spans, inline, &font, pal.text, pal.cursor_line.gamma_multiply(3.0)));
            });
        }
    });
}

fn references(ui: &mut Ui, peek: &Peek, actions: &mut Vec<Action>) {
    let pal = palette(ui);
    let Some(refs) = &peek.references else {
        ui.weak("Put the cursor on a name and press Shift+F12 to list its references.");
        return;
    };
    match &refs.state {
        ReferencesState::Pending => {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label(format!("Indexing symbols at {}…", refs.version.label));
            });
        }
        ReferencesState::NotFound(msg) => {
            ui.label(msg);
        }
        ReferencesState::LspPending { .. } => {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label("Asking the language server…");
            });
        }
        ReferencesState::Lsp { name, targets } => {
            ui.horizontal(|ui| {
                ui.label(format!("{} reference{} to", targets.len(), if targets.len() == 1 { "" } else { "s" }));
                ui.label(RichText::new(name).monospace());
                ui.weak(format!("at {} · via language server", refs.version.label));
            });
            egui::ScrollArea::vertical().id_salt("lsp_refs").auto_shrink([false, true]).show(ui, |ui| {
                for (i, t) in targets.iter().enumerate() {
                    let colour = if t.rel.is_some() { pal.text } else { pal.weak };
                    if ui.selectable_label(false, RichText::new(format!("{}  {}", t.label(), t.context)).monospace().color(colour)).clicked() {
                        actions.push(Action::OpenReference(i));
                    }
                }
            });
        }
        ReferencesState::Ready { target, hits } => {
            ui.horizontal(|ui| {
                ui.label(format!("{} reference{} to", hits.len(), if hits.len() == 1 { "" } else { "s" }));
                ui.label(RichText::new(format!("{} {}", target.symbol.kind.label(), target.symbol.fqn)).monospace());
                ui.weak(format!("at {}", refs.version.label));
            });
            let uncertain = hits.iter().filter(|h| !h.certain).count();
            if uncertain > 0 {
                ui.weak(format!("{uncertain} marked “?” match by name only (receiver type unknown)."));
            }
            egui::ScrollArea::vertical().id_salt("ref_hits").auto_shrink([false, true]).show(ui, |ui| {
                for (i, h) in hits.iter().enumerate() {
                    let marker = if h.certain { " " } else { "?" };
                    let text = format!("{marker} {}:{}  {}", h.path, h.reference.span.line + 1, h.reference.context);
                    let colour = if h.certain { pal.text } else { pal.weak };
                    if ui.selectable_label(false, RichText::new(text).monospace().color(colour)).clicked() {
                        actions.push(Action::OpenReference(i));
                    }
                }
            });
        }
    }
}

fn blame_tab(ui: &mut Ui, b: &BlameInfo<'_>, actions: &mut Vec<Action>) {
    if !b.enabled {
        ui.weak("Blame is off. Press b to show the blame gutter.");
        return;
    }
    if let Some(e) = b.error {
        ui.colored_label(ui.visuals().error_fg_color, e);
        return;
    }
    if b.loading {
        ui.horizontal(|ui| {
            ui.spinner();
            ui.label("Running git blame…");
        });
    }
    match b.line {
        Some((line, Some(commit))) => {
            ui.weak(format!("Line {line} last changed in:"));
            blame_tooltip(ui, commit);
            if ui.button("Open commit").clicked() {
                actions.push(Action::OpenCommit { id: commit.id, path: Some(commit.path.clone()) });
            }
        }
        Some((line, None)) => {
            ui.weak(format!("No blame for line {line} yet."));
        }
        None => {
            ui.weak("Click a line to see who changed it last.");
        }
    }
    let share = |ui: &mut Ui, title: &str, rows: &[(String, usize)]| {
        let total: usize = rows.iter().map(|r| r.1).sum();
        if total == 0 {
            return;
        }
        let parts: Vec<String> = rows
            .iter()
            .take(6)
            .map(|(who, n)| format!("{who} {:.0}%", *n as f32 * 100.0 / total as f32))
            .collect();
        ui.label(format!("{title}: {}", parts.join(" · ")));
    };
    ui.separator();
    share(ui, "Lines by author", &b.by_author);
    share(ui, "Lines by committer", &b.by_committer);
}
