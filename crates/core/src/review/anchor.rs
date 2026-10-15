//! Find a thread's lines again in another version of the file.

use std::collections::HashSet;

use super::model::Anchor;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Placement {
    /// Same text at the same lines.
    Exact { start: u32, end: u32 },
    /// Same text (possibly re-indented) elsewhere.
    Moved { start: u32, end: u32 },
    /// Similar text: the lines were edited.
    Fuzzy { start: u32, end: u32 },
    /// Not found: the code was removed or rewritten.
    Outdated,
}

impl Placement {
    pub fn lines(self) -> Option<(u32, u32)> {
        match self {
            Placement::Exact { start, end } | Placement::Moved { start, end } | Placement::Fuzzy { start, end } => {
                Some((start, end))
            }
            Placement::Outdated => None,
        }
    }
}

/// Minimum average similarity for a fuzzy match.
const FUZZY_THRESHOLD: f32 = 0.6;

fn normalise(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn tokens(s: &str) -> HashSet<&str> {
    s.split(|c: char| !(c.is_alphanumeric() || c == '_' || c == '$')).filter(|t| !t.is_empty()).collect()
}

fn similarity(a: &HashSet<&str>, b: &HashSet<&str>) -> f32 {
    if a.is_empty() && b.is_empty() {
        return 1.0;
    }
    let shared = a.intersection(b).count() as f32;
    shared / (a.len() + b.len()) as f32 * 2.0
}

/// How well the anchor's context matches around a candidate position: 0.0..=1.0.
fn context_score(anchor: &Anchor, file: &[String], start: usize, len: usize) -> f32 {
    let mut matched = 0;
    let mut total = 0;
    for (i, line) in anchor.before.iter().rev().enumerate() {
        total += 1;
        if start > i && normalise(&file[start - 1 - i]) == normalise(line) {
            matched += 1;
        }
    }
    for (i, line) in anchor.after.iter().enumerate() {
        total += 1;
        if file.get(start + len + i).is_some_and(|l| normalise(l) == normalise(line)) {
            matched += 1;
        }
    }
    if total == 0 { 1.0 } else { matched as f32 / total as f32 }
}

/// Pick the best of several candidate starts: context first, then closeness to the old position.
fn best(candidates: &[usize], anchor: &Anchor, file: &[String], hint: u32) -> Option<usize> {
    let len = anchor.lines.len();
    candidates.iter().copied().max_by(|&a, &b| {
        let ka = (context_score(anchor, file, a, len), -(a as i64 - hint as i64).abs());
        let kb = (context_score(anchor, file, b, len), -(b as i64 - hint as i64).abs());
        ka.partial_cmp(&kb).unwrap_or(std::cmp::Ordering::Equal)
    })
}

/// Relocate `anchor`, originally at `hint` (0-based start line), in `file`.
pub fn relocate(anchor: &Anchor, hint: u32, file: &[String]) -> Placement {
    let len = anchor.lines.len();
    if len == 0 || len > file.len() {
        return Placement::Outdated;
    }
    let span = |start: usize| (start as u32, (start + len - 1) as u32);
    let windows = file.len() - len + 1;

    let exact: Vec<usize> = (0..windows).filter(|&i| file[i..i + len] == anchor.lines[..]).collect();
    if exact.contains(&(hint as usize)) && context_score(anchor, file, hint as usize, len) >= 0.5 {
        let (start, end) = span(hint as usize);
        return Placement::Exact { start, end };
    }
    if let Some(i) = best(&exact, anchor, file, hint) {
        let (start, end) = span(i);
        return if i == hint as usize { Placement::Exact { start, end } } else { Placement::Moved { start, end } };
    }

    let wanted: Vec<String> = anchor.lines.iter().map(|l| normalise(l)).collect();
    let normalised: Vec<String> = file.iter().map(|l| normalise(l)).collect();
    let loose: Vec<usize> = (0..windows).filter(|&i| normalised[i..i + len] == wanted[..]).collect();
    if let Some(i) = best(&loose, anchor, file, hint) {
        let (start, end) = span(i);
        return Placement::Moved { start, end };
    }

    // Fuzzy: average token similarity of the window, with context as a tie-breaker.
    let anchor_tokens: Vec<HashSet<&str>> = anchor.lines.iter().map(|l| tokens(l)).collect();
    let file_tokens: Vec<HashSet<&str>> = file.iter().map(|l| tokens(l)).collect();
    let mut best_fuzzy: Option<(f32, usize)> = None;
    for i in 0..windows {
        let score: f32 =
            (0..len).map(|k| similarity(&anchor_tokens[k], &file_tokens[i + k])).sum::<f32>() / len as f32;
        if score < FUZZY_THRESHOLD {
            continue;
        }
        let total = score + 0.25 * context_score(anchor, file, i, len) - (i as f32 - hint as f32).abs() * 1e-6;
        if best_fuzzy.is_none_or(|(s, _)| total > s) {
            best_fuzzy = Some((total, i));
        }
    }
    match best_fuzzy {
        Some((_, i)) => {
            let (start, end) = span(i);
            Placement::Fuzzy { start, end }
        }
        None => Placement::Outdated,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lines(s: &str) -> Vec<String> {
        s.lines().map(str::to_string).collect()
    }

    const FILE: &str = "<?php\nclass A\n{\n    public function total()\n    {\n        return $this->sum * 2;\n    }\n}\n";

    fn anchor() -> Anchor {
        Anchor::capture(&lines(FILE), 5, 5)
    }

    #[test]
    fn exact_when_unchanged() {
        assert_eq!(relocate(&anchor(), 5, &lines(FILE)), Placement::Exact { start: 5, end: 5 });
    }

    #[test]
    fn follows_inserted_lines_and_reindent() {
        let moved = FILE.replace("class A\n", "class A\n// one\n// two\n");
        assert_eq!(relocate(&anchor(), 5, &lines(&moved)), Placement::Moved { start: 7, end: 7 });
        let reindented = FILE.replace("        return", "            return");
        assert_eq!(relocate(&anchor(), 5, &lines(&reindented)), Placement::Moved { start: 5, end: 5 });
    }

    #[test]
    fn fuzzy_when_edited() {
        let edited = FILE.replace("$this->sum * 2", "$this->sum * 3");
        assert_eq!(relocate(&anchor(), 5, &lines(&edited)), Placement::Fuzzy { start: 5, end: 5 });
    }

    #[test]
    fn outdated_when_removed() {
        let removed = FILE.replace("        return $this->sum * 2;\n", "        throw new Exception();\n");
        assert_eq!(relocate(&anchor(), 5, &lines(&removed)), Placement::Outdated);
    }

    #[test]
    fn context_picks_among_duplicates() {
        let file = lines("a\n}\nb\n}\nc\n");
        let anchor = Anchor::capture(&file, 3, 3);
        let shifted = lines("x\na\n}\nb\n}\nc\n");
        assert_eq!(relocate(&anchor, 3, &shifted), Placement::Moved { start: 4, end: 4 });
    }
}
