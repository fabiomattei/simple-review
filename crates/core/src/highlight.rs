//! Syntax highlighting with syntect. Produces per-line coloured spans; rendering is up to the UI.

use std::ops::Range;
use std::sync::{Arc, OnceLock};

use syntect::easy::HighlightLines;
use syntect::highlighting::{FontStyle, Theme, ThemeSet};
use syntect::parsing::{SyntaxReference, SyntaxSet};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Span {
    pub start: u32,
    pub end: u32,
    pub rgb: [u8; 3],
    pub bold: bool,
    pub italic: bool,
}

impl Span {
    pub fn range(&self) -> Range<usize> {
        self.start as usize..self.end as usize
    }
}

/// Highlighted spans for each line of a file.
pub type Highlighted = Vec<Vec<Span>>;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ThemeKind {
    Light,
    Dark,
}

pub struct Highlighter {
    syntaxes: SyntaxSet,
    light: Theme,
    dark: Theme,
}

/// Lines longer than this are left unhighlighted; regex highlighting on minified code is very slow.
const MAX_HIGHLIGHT_LINE: usize = 4_000;

impl Highlighter {
    /// The shared instance. Loading syntax definitions takes ~100 ms, so it happens once.
    pub fn global() -> Arc<Highlighter> {
        static INSTANCE: OnceLock<Arc<Highlighter>> = OnceLock::new();
        INSTANCE.get_or_init(|| Arc::new(Highlighter::new())).clone()
    }

    fn new() -> Self {
        let mut themes = ThemeSet::load_defaults().themes;
        Highlighter {
            syntaxes: SyntaxSet::load_defaults_newlines(),
            light: themes.remove("InspiredGitHub").expect("bundled theme"),
            dark: themes.remove("base16-ocean.dark").expect("bundled theme"),
        }
    }

    fn syntax_for(&self, path: &str, first_line: &str) -> &SyntaxReference {
        let name = path.rsplit('/').next().unwrap_or(path);
        let ext = match name.rsplit_once('.') {
            Some((_, ext)) => ext,
            None => name,
        };
        let alias = match ext {
            "vue" | "svelte" => "html",
            "ts" | "tsx" | "mjs" | "cjs" | "jsx" => "js",
            "env" | "example" => "sh",
            "neon" | "yml" => "yaml",
            "lock" => "json",
            other => other,
        };
        self.syntaxes
            .find_syntax_by_extension(alias)
            .or_else(|| self.syntaxes.find_syntax_by_first_line(first_line))
            .unwrap_or_else(|| self.syntaxes.find_syntax_plain_text())
    }

    pub fn language_name(&self, path: &str) -> &str {
        &self.syntax_for(path, "").name
    }

    /// Highlight `lines` (without line endings). `cancelled` is polled periodically; when it returns
    /// true highlighting stops and `None` is returned.
    pub fn highlight(
        &self,
        path: &str,
        lines: &[String],
        theme: ThemeKind,
        cancelled: &dyn Fn() -> bool,
    ) -> Option<Highlighted> {
        let syntax = self.syntax_for(path, lines.first().map(String::as_str).unwrap_or(""));
        let theme = match theme {
            ThemeKind::Light => &self.light,
            ThemeKind::Dark => &self.dark,
        };
        let mut h = HighlightLines::new(syntax, theme);
        let mut out = Vec::with_capacity(lines.len());
        let mut buf = String::new();
        for (i, line) in lines.iter().enumerate() {
            if i % 256 == 0 && cancelled() {
                return None;
            }
            if line.len() > MAX_HIGHLIGHT_LINE {
                out.push(Vec::new());
                continue;
            }
            buf.clear();
            buf.push_str(line);
            buf.push('\n');
            let Ok(regions) = h.highlight_line(&buf, &self.syntaxes) else {
                out.push(Vec::new());
                continue;
            };
            let mut spans = Vec::with_capacity(regions.len());
            let mut pos = 0usize;
            for (style, text) in regions {
                let start = pos;
                pos += text.len();
                let end = pos.min(line.len());
                if start >= end {
                    continue;
                }
                spans.push(Span {
                    start: start as u32,
                    end: end as u32,
                    rgb: [style.foreground.r, style.foreground.g, style.foreground.b],
                    bold: style.font_style.contains(FontStyle::BOLD),
                    italic: style.font_style.contains(FontStyle::ITALIC),
                });
            }
            out.push(spans);
        }
        Some(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn highlights_php() {
        let h = Highlighter::global();
        assert_eq!(h.language_name("app/Models/User.php"), "PHP");
        let lines: Vec<String> = ["<?php", "class User extends Model {}"].map(String::from).to_vec();
        let out = h.highlight("User.php", &lines, ThemeKind::Dark, &|| false).unwrap();
        assert_eq!(out.len(), 2);
        let colours: std::collections::HashSet<_> = out[1].iter().map(|s| s.rgb).collect();
        assert!(colours.len() > 1, "expected several colours, got {:?}", out[1]);
        assert_eq!(out[1].last().unwrap().end as usize, lines[1].len());
    }
}
