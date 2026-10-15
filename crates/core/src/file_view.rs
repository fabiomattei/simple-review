//! Turn a [`FileChange`] into something the UI can show: a text diff or a placeholder.

use anyhow::Result;

use crate::diff::content::{self, Content};
use crate::diff::{DiffOptions, FileDiff, diff_texts};
use crate::git::{EntryType, FileChange, FileSide, Repo};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Placeholder {
    Binary { old_size: Option<u64>, new_size: Option<u64> },
    Image { old_size: Option<u64>, new_size: Option<u64> },
    LfsPointer { oid: String, size: u64 },
    TooLarge { bytes: usize, lines: usize },
    Submodule { old: Option<String>, new: Option<String> },
}

#[derive(Debug, Clone)]
pub enum FileView {
    Text {
        diff: FileDiff,
        /// At least one side was not UTF-8 and was decoded as Latin-1.
        non_utf8: bool,
    },
    Placeholder(Placeholder),
}

/// Read both sides of `change` and diff them. With `force`, size limits are ignored.
pub fn load(repo: &Repo, change: &FileChange, opts: DiffOptions, force: bool) -> Result<FileView> {
    if change.is_submodule() {
        let id = |s: &Option<FileSide>| {
            s.as_ref().filter(|s| s.entry == EntryType::Submodule).map(|s| s.id.to_hex_with_len(10).to_string())
        };
        return Ok(FileView::Placeholder(Placeholder::Submodule { old: id(&change.old), new: id(&change.new) }));
    }

    let read = |side: &Option<FileSide>| -> Result<Option<(Content, u64)>> {
        let Some(side) = side else { return Ok(None) };
        let bytes = repo.read_blob(side.id)?;
        let size = bytes.len() as u64;
        Ok(Some((content::classify(&side.path, &bytes, force), size)))
    };
    let old = read(&change.old)?;
    let new = read(&change.new)?;
    let sizes = || (old.as_ref().map(|(_, s)| *s), new.as_ref().map(|(_, s)| *s));

    let contents = [&old, &new];
    let mut sides = contents.iter().filter_map(|s| s.as_ref().map(|(c, _)| c));
    if let Some(placeholder) = sides.find_map(|c| match c {
        Content::Text { .. } => None,
        Content::Binary => {
            let (old_size, new_size) = sizes();
            Some(Placeholder::Binary { old_size, new_size })
        }
        Content::Image => {
            let (old_size, new_size) = sizes();
            Some(Placeholder::Image { old_size, new_size })
        }
        Content::LfsPointer { oid, size } => Some(Placeholder::LfsPointer { oid: oid.clone(), size: *size }),
        Content::TooLarge { bytes, lines } => Some(Placeholder::TooLarge { bytes: *bytes, lines: *lines }),
    }) {
        return Ok(FileView::Placeholder(placeholder));
    }

    let text = |s: &Option<(Content, u64)>| match s {
        Some((Content::Text { text, non_utf8 }, _)) => (text.clone(), *non_utf8),
        _ => (String::new(), false),
    };
    let (old_text, old_lossy) = text(&old);
    let (new_text, new_lossy) = text(&new);
    Ok(FileView::Text { diff: diff_texts(&old_text, &new_text, opts), non_utf8: old_lossy || new_lossy })
}
