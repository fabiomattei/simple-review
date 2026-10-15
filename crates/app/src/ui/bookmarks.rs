//! The Bookmarks tab.

use egui::{RichText, Ui};
use review_core::review::bookmarks::Bookmark;

use super::{Action, time_label};

/// `notes` holds the editable note of each bookmark, by position.
pub fn show(ui: &mut Ui, bookmarks: &[Bookmark], notes: &mut Vec<String>, actions: &mut Vec<Action>) {
    if bookmarks.is_empty() {
        ui.weak("No bookmarks yet. Click a line and press Cmd/Ctrl+D.");
        return;
    }
    if notes.len() != bookmarks.len() || notes.iter().zip(bookmarks).any(|(n, b)| n.is_empty() && !b.note.is_empty()) {
        *notes = bookmarks.iter().map(|b| b.note.clone()).collect();
    }
    egui::ScrollArea::vertical().id_salt("bookmarks").auto_shrink([false, true]).show(ui, |ui| {
        for (b, note) in bookmarks.iter().zip(notes.iter_mut()) {
            ui.horizontal(|ui| {
                let edit = ui.add(egui::TextEdit::singleline(note).desired_width(260.0));
                if edit.lost_focus() && *note != b.note {
                    actions.push(Action::RenameBookmark(b.id, note.clone()));
                }
                if ui.link(RichText::new(format!("{}:{}", b.path, b.line + 1)).monospace()).clicked() {
                    actions.push(Action::OpenBookmark(b.id));
                }
                ui.weak(format!("{} side · {} · {}", b.side.as_str(), &b.commit_id[..8.min(b.commit_id.len())], time_label(b.created_at)));
                if ui.small_button("Delete").clicked() {
                    actions.push(Action::DeleteBookmark(b.id));
                }
            });
        }
    });
}
