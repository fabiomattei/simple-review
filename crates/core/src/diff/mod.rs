//! Line diffs aligned into side-by-side rows, with intra-line and moved-block detection.

pub mod content;

use std::collections::HashMap;
use std::ops::Range;

use imara_diff::{Algorithm, Diff, InternedInput, Interner, Token};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct DiffOptions {
    /// Compare lines with all whitespace removed (like `git diff -w`).
    pub ignore_whitespace: bool,
    /// Compute word-level highlights for changed line pairs.
    pub intra_line: bool,
}

/// A text file split into lines, with line endings normalised away.
#[derive(Debug, Clone, Default)]
pub struct TextFile {
    pub lines: Vec<String>,
    /// Majority line ending is CRLF.
    pub crlf: bool,
    pub missing_final_newline: bool,
}

impl TextFile {
    pub fn parse(text: &str) -> Self {
        let mut crlf_count = 0usize;
        let mut lines: Vec<String> = text
            .split('\n')
            .map(|l| match l.strip_suffix('\r') {
                Some(stripped) => {
                    crlf_count += 1;
                    stripped.to_string()
                }
                None => l.to_string(),
            })
            .collect();
        let missing_final_newline = !text.is_empty() && !text.ends_with('\n');
        if text.ends_with('\n') || text.is_empty() {
            lines.pop();
        }
        let crlf = crlf_count > 0 && crlf_count * 2 >= lines.len();
        TextFile { lines, crlf, missing_final_newline }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LineKind {
    Context,
    Removed,
    Added,
    /// Removed or added, but the same block appears on the other side elsewhere.
    Moved,
}

/// One line on one side of a row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cell {
    /// 0-based line index into the side's [`TextFile::lines`].
    pub line: u32,
    pub kind: LineKind,
    /// Byte ranges within the line that changed (intra-line highlight).
    pub inline: Vec<Range<usize>>,
}

/// A visual row: a line on the left, the right, or both. A missing side is drawn as filler.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Row {
    pub left: Option<Cell>,
    pub right: Option<Cell>,
}

impl Row {
    pub fn is_change(&self) -> bool {
        [&self.left, &self.right]
            .into_iter()
            .flatten()
            .any(|c| c.kind != LineKind::Context)
    }
}

#[derive(Debug, Clone, Default)]
pub struct FileDiff {
    pub old: TextFile,
    pub new: TextFile,
    /// Side-by-side rows.
    pub rows: Vec<Row>,
    /// Row ranges of contiguous changes in [`Self::rows`].
    pub hunks: Vec<Range<usize>>,
    pub additions: usize,
    pub deletions: usize,
}

impl FileDiff {
    pub fn line_endings_changed(&self) -> bool {
        !self.old.lines.is_empty() && !self.new.lines.is_empty() && self.old.crlf != self.new.crlf
    }

    /// Rows for a unified view: each change block lists its removed lines, then its added lines.
    pub fn unified_rows(&self) -> Vec<Row> {
        let mut out = Vec::with_capacity(self.rows.len() + self.deletions);
        let mut i = 0;
        while i < self.rows.len() {
            if !self.rows[i].is_change() {
                out.push(self.rows[i].clone());
                i += 1;
                continue;
            }
            let start = i;
            while i < self.rows.len() && self.rows[i].is_change() {
                i += 1;
            }
            let block = &self.rows[start..i];
            out.extend(block.iter().filter_map(|r| r.left.clone()).map(|c| Row { left: Some(c), right: None }));
            out.extend(block.iter().filter_map(|r| r.right.clone()).map(|c| Row { left: None, right: Some(c) }));
        }
        out
    }
}

/// Row ranges of contiguous changed rows.
pub fn hunk_ranges(rows: &[Row]) -> Vec<Range<usize>> {
    let mut hunks = Vec::new();
    let mut start = None;
    for (i, row) in rows.iter().enumerate() {
        match (row.is_change(), start) {
            (true, None) => start = Some(i),
            (false, Some(s)) => {
                hunks.push(s..i);
                start = None;
            }
            _ => {}
        }
    }
    if let Some(s) = start {
        hunks.push(s..rows.len());
    }
    hunks
}

fn strip_whitespace(line: &str) -> String {
    line.chars().filter(|c| !c.is_whitespace()).collect()
}

/// Diff two texts. Either side may be empty (added/deleted files).
pub fn diff_texts(old: &str, new: &str, opts: DiffOptions) -> FileDiff {
    let old = TextFile::parse(old);
    let new = TextFile::parse(new);

    let (old_keys, new_keys): (Vec<String>, Vec<String>) = if opts.ignore_whitespace {
        (
            old.lines.iter().map(|l| strip_whitespace(l)).collect(),
            new.lines.iter().map(|l| strip_whitespace(l)).collect(),
        )
    } else {
        (old.lines.clone(), new.lines.clone())
    };

    let mut input: InternedInput<&str> = InternedInput::default();
    input.update_before(old_keys.iter().map(String::as_str));
    input.update_after(new_keys.iter().map(String::as_str));
    let mut diff = Diff::compute(Algorithm::Histogram, &input);
    if opts.ignore_whitespace {
        diff.postprocess_no_heuristic(&input);
    } else {
        diff.postprocess_lines(&input);
    }

    let mut rows = Vec::with_capacity(old.lines.len().max(new.lines.len()));
    let mut additions = 0;
    let mut deletions = 0;
    let (mut o, mut n) = (0u32, 0u32);
    let context = |rows: &mut Vec<Row>, o: u32, n: u32| {
        rows.push(Row {
            left: Some(Cell { line: o, kind: LineKind::Context, inline: vec![] }),
            right: Some(Cell { line: n, kind: LineKind::Context, inline: vec![] }),
        });
    };
    for hunk in diff.hunks() {
        while o < hunk.before.start {
            context(&mut rows, o, n);
            o += 1;
            n += 1;
        }
        let removed = hunk.before.len() as u32;
        let added = hunk.after.len() as u32;
        deletions += removed as usize;
        additions += added as usize;
        for k in 0..removed.max(added) {
            let left = (k < removed).then(|| Cell { line: hunk.before.start + k, kind: LineKind::Removed, inline: vec![] });
            let right = (k < added).then(|| Cell { line: hunk.after.start + k, kind: LineKind::Added, inline: vec![] });
            rows.push(Row { left, right });
        }
        o = hunk.before.end;
        n = hunk.after.end;
    }
    while (o as usize) < old.lines.len() {
        context(&mut rows, o, n);
        o += 1;
        n += 1;
    }

    mark_moved(&mut rows, &old.lines, &new.lines);

    if opts.intra_line {
        for row in &mut rows {
            if let (Some(l), Some(r)) = (&mut row.left, &mut row.right)
                && l.kind == LineKind::Removed
                && r.kind == LineKind::Added
            {
                let (li, ri) = intra_line(&old.lines[l.line as usize], &new.lines[r.line as usize], opts.ignore_whitespace);
                l.inline = li;
                r.inline = ri;
            }
        }
    }

    let hunks = hunk_ranges(&rows);
    FileDiff { old, new, rows, hunks, additions, deletions }
}

/// Lines too trivial to count as evidence of a move (braces, blank lines).
fn is_significant(line: &str) -> bool {
    line.trim().len() > 3
}

const MIN_MOVED_LINES: usize = 3;

/// Mark blocks of removed lines that reappear verbatim (ignoring indentation) as added lines in a
/// different hunk.
fn mark_moved(rows: &mut [Row], old: &[String], new: &[String]) {
    let hunks = hunk_ranges(rows);
    // Per side: line index -> (row index, hunk index), for changed lines only.
    let mut removed: HashMap<u32, (usize, usize)> = HashMap::new();
    let mut added: HashMap<u32, (usize, usize)> = HashMap::new();
    let mut added_by_text: HashMap<&str, Vec<u32>> = HashMap::new();
    for (h, range) in hunks.iter().enumerate() {
        for r in range.clone() {
            if let Some(c) = &rows[r].left
                && c.kind == LineKind::Removed
            {
                removed.insert(c.line, (r, h));
            }
            if let Some(c) = &rows[r].right
                && c.kind == LineKind::Added
            {
                added.insert(c.line, (r, h));
                let text = new[c.line as usize].trim();
                if is_significant(text) {
                    added_by_text.entry(text).or_default().push(c.line);
                }
            }
        }
    }
    if removed.is_empty() || added_by_text.is_empty() {
        return;
    }

    let mut moved_old: Vec<u32> = Vec::new();
    let mut moved_new: Vec<u32> = Vec::new();
    let mut old_lines: Vec<u32> = removed.keys().copied().collect();
    old_lines.sort_unstable();
    let mut i = 0;
    while i < old_lines.len() {
        let start = old_lines[i];
        let (_, hunk) = removed[&start];
        let mut best: Option<(u32, usize)> = None;
        if let Some(candidates) = added_by_text.get(old[start as usize].trim()) {
            for &cand in candidates.iter().take(16) {
                if added[&cand].1 == hunk {
                    continue;
                }
                let mut len = 0usize;
                loop {
                    let (ol, nl) = (start + len as u32, cand + len as u32);
                    let ok = removed.contains_key(&ol)
                        && added.contains_key(&nl)
                        && old[ol as usize].trim() == new[nl as usize].trim();
                    if !ok {
                        break;
                    }
                    len += 1;
                }
                if best.is_none_or(|(_, l)| len > l) {
                    best = Some((cand, len));
                }
            }
        }
        match best {
            Some((cand, len))
                if len >= MIN_MOVED_LINES
                    && (0..len).filter(|k| is_significant(&old[start as usize + k])).count() >= 2 =>
            {
                moved_old.extend((0..len as u32).map(|k| start + k));
                moved_new.extend((0..len as u32).map(|k| cand + k));
                i += len;
            }
            _ => i += 1,
        }
    }
    for line in moved_old {
        let (r, _) = removed[&line];
        if let Some(c) = &mut rows[r].left {
            c.kind = LineKind::Moved;
        }
    }
    for line in moved_new {
        let (r, _) = added[&line];
        if let Some(c) = &mut rows[r].right {
            c.kind = LineKind::Moved;
        }
    }
}

/// Split a line into words, whitespace runs and single punctuation characters.
fn word_tokens(line: &str) -> Vec<Range<usize>> {
    #[derive(PartialEq)]
    enum Class {
        Word,
        Space,
        Other,
    }
    let class = |c: char| {
        if c.is_alphanumeric() || c == '_' {
            Class::Word
        } else if c.is_whitespace() {
            Class::Space
        } else {
            Class::Other
        }
    };
    let mut out: Vec<Range<usize>> = Vec::new();
    let mut prev: Option<Class> = None;
    for (i, c) in line.char_indices() {
        let cl = class(c);
        match (&prev, out.last_mut()) {
            (Some(p), Some(last)) if *p == cl && cl != Class::Other => last.end = i + c.len_utf8(),
            _ => out.push(i..i + c.len_utf8()),
        }
        prev = Some(cl);
    }
    out
}

/// Byte ranges that differ between two paired lines. Returns empty ranges when the lines are so
/// different that highlighting words would only add noise.
pub fn intra_line(old: &str, new: &str, ignore_whitespace: bool) -> (Vec<Range<usize>>, Vec<Range<usize>>) {
    let old_tokens = word_tokens(old);
    let new_tokens = word_tokens(new);
    let mut interner: Interner<&str> = Interner::new(old_tokens.len() + new_tokens.len());
    let before: Vec<Token> = old_tokens.iter().map(|r| interner.intern(&old[r.clone()])).collect();
    let after: Vec<Token> = new_tokens.iter().map(|r| interner.intern(&new[r.clone()])).collect();
    let mut diff = Diff::default();
    diff.compute_with(Algorithm::Histogram, &before, &after, interner.num_tokens());

    let collect = |tokens: &[Range<usize>], text: &str, changed: &dyn Fn(u32) -> bool| {
        let mut ranges: Vec<Range<usize>> = Vec::new();
        for (i, r) in tokens.iter().enumerate() {
            if !changed(i as u32) || (ignore_whitespace && text[r.clone()].trim().is_empty()) {
                continue;
            }
            match ranges.last_mut() {
                Some(last) if last.end == r.start => last.end = r.end,
                _ => ranges.push(r.clone()),
            }
        }
        ranges
    };
    let left = collect(&old_tokens, old, &|i| diff.is_removed(i));
    let right = collect(&new_tokens, new, &|i| diff.is_added(i));

    let mostly_changed = |ranges: &[Range<usize>], text: &str| {
        let significant = text.trim().len().max(1);
        let changed: usize = ranges.iter().map(|r| text[r.clone()].trim().len()).sum();
        changed * 10 > significant * 7
    };
    if mostly_changed(&left, old) && mostly_changed(&right, new) {
        return (vec![], vec![]);
    }
    (left, right)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kinds(rows: &[Row]) -> Vec<(Option<LineKind>, Option<LineKind>)> {
        rows.iter()
            .map(|r| (r.left.as_ref().map(|c| c.kind), r.right.as_ref().map(|c| c.kind)))
            .collect()
    }

    #[test]
    fn aligns_rows_with_filler() {
        let d = diff_texts("a\nb\nc\n", "a\nB\nx\nc\n", DiffOptions::default());
        use LineKind::*;
        assert_eq!(
            kinds(&d.rows),
            vec![
                (Some(Context), Some(Context)),
                (Some(Removed), Some(Added)),
                (None, Some(Added)),
                (Some(Context), Some(Context)),
            ]
        );
        assert_eq!(d.hunks, vec![1..3]);
        assert_eq!((d.additions, d.deletions), (2, 1));
    }

    #[test]
    fn added_and_deleted_files() {
        let d = diff_texts("", "x\ny\n", DiffOptions::default());
        assert!(d.rows.iter().all(|r| r.left.is_none()));
        assert_eq!(d.additions, 2);
        let d = diff_texts("x\n", "", DiffOptions::default());
        assert!(d.rows.iter().all(|r| r.right.is_none()));
        assert_eq!(d.deletions, 1);
    }

    #[test]
    fn ignores_whitespace() {
        let old = "if (x) {\n  foo();\n}\n";
        let new = "if (x) {\n\tfoo( );\n}\n";
        assert_eq!(diff_texts(old, new, DiffOptions::default()).hunks.len(), 1);
        let d = diff_texts(old, new, DiffOptions { ignore_whitespace: true, ..Default::default() });
        assert!(d.hunks.is_empty());
    }

    #[test]
    fn normalises_line_endings_but_reports_change() {
        let d = diff_texts("a\nb\n", "a\r\nb\r\n", DiffOptions::default());
        assert!(d.hunks.is_empty());
        assert!(d.line_endings_changed());
    }

    #[test]
    fn intra_line_highlights_changed_word() {
        let (l, r) = intra_line("return $user->name;", "return $user->email;", false);
        assert_eq!(&"return $user->name;"[l[0].clone()], "name");
        assert_eq!(&"return $user->email;"[r[0].clone()], "email");
        let (l, r) = intra_line("completely different", "nothing alike here", false);
        assert!(l.is_empty() && r.is_empty());
    }

    #[test]
    fn detects_moved_block() {
        let old = "function a() {\n    $x = compute();\n    $y = transform($x);\n    return save($y);\n}\nmiddle1\nmiddle2\nmiddle3\nfunction b() {}\n";
        let new = "middle1\nmiddle2\nmiddle3\nfunction b() {}\nfunction a() {\n    $x = compute();\n    $y = transform($x);\n    return save($y);\n}\n";
        let d = diff_texts(old, new, DiffOptions::default());
        let moved_left = d.rows.iter().filter(|r| r.left.as_ref().is_some_and(|c| c.kind == LineKind::Moved)).count();
        let moved_right = d.rows.iter().filter(|r| r.right.as_ref().is_some_and(|c| c.kind == LineKind::Moved)).count();
        assert!(moved_left >= 4, "{:?}", kinds(&d.rows));
        assert_eq!(moved_left, moved_right);
    }

    #[test]
    fn unified_rows_list_removed_before_added() {
        let d = diff_texts("a\nb\nc\n", "a\nB\nC\n", DiffOptions::default());
        let u = d.unified_rows();
        let sides: Vec<_> = u.iter().map(|r| (r.left.is_some(), r.right.is_some())).collect();
        assert_eq!(sides, vec![(true, true), (true, false), (true, false), (false, true), (false, true)]);
    }

    #[test]
    fn tracks_missing_final_newline() {
        assert!(TextFile::parse("a\nb").missing_final_newline);
        assert_eq!(TextFile::parse("a\nb").lines, vec!["a", "b"]);
        assert!(!TextFile::parse("a\n").missing_final_newline);
    }
}
