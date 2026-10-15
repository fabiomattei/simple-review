//! Per-function size and branch counts, compared between two versions of a PHP file.

use std::collections::BTreeMap;

use tree_sitter::{Node, Parser};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Metrics {
    pub lines: u32,
    /// Decision points: if/elseif/case/loops/catch/ternaries/&&/||/??/match arms.
    pub branches: u32,
    /// 0-based first line of the function.
    pub line: u32,
}

impl Metrics {
    /// Cyclomatic complexity.
    pub fn cyclomatic(&self) -> u32 {
        self.branches + 1
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ComplexityChange {
    /// `Class::method` or `function`.
    pub name: String,
    pub old: Option<Metrics>,
    pub new: Option<Metrics>,
}

impl ComplexityChange {
    pub fn branch_delta(&self) -> i64 {
        self.new.map_or(0, |m| m.branches as i64) - self.old.map_or(0, |m| m.branches as i64)
    }

    pub fn line_delta(&self) -> i64 {
        self.new.map_or(0, |m| m.lines as i64) - self.old.map_or(0, |m| m.lines as i64)
    }
}

/// Metrics for every function and method in `text`.
pub fn functions(text: &str) -> BTreeMap<String, Metrics> {
    let mut out = BTreeMap::new();
    let mut parser = Parser::new();
    if parser.set_language(&tree_sitter_php::LANGUAGE_PHP.into()).is_err() {
        return out;
    }
    let Some(tree) = parser.parse(text, None) else { return out };
    collect(tree.root_node(), text.as_bytes(), None, &mut out);
    out
}

fn collect(node: Node<'_>, src: &[u8], class: Option<&str>, out: &mut BTreeMap<String, Metrics>) {
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        match child.kind() {
            "class_declaration" | "trait_declaration" | "enum_declaration" | "interface_declaration" => {
                let name = child.child_by_field_name("name").and_then(|n| n.utf8_text(src).ok()).unwrap_or("?");
                if let Some(body) = child.child_by_field_name("body") {
                    collect(body, src, Some(name), out);
                }
            }
            "method_declaration" | "function_definition" => {
                let name = child.child_by_field_name("name").and_then(|n| n.utf8_text(src).ok()).unwrap_or("?");
                let key = match (child.kind(), class) {
                    ("method_declaration", Some(c)) => format!("{c}::{name}"),
                    _ => name.to_string(),
                };
                let start = child.start_position().row as u32;
                let lines = child.end_position().row as u32 - start + 1;
                let branches = child.child_by_field_name("body").map_or(0, |b| count_branches(b, src));
                out.insert(key, Metrics { lines, branches, line: start });
            }
            _ => collect(child, src, class, out),
        }
    }
}

fn count_branches(node: Node<'_>, src: &[u8]) -> u32 {
    let own = match node.kind() {
        "if_statement" | "else_if_clause" | "case_statement" | "for_statement" | "foreach_statement"
        | "while_statement" | "do_statement" | "catch_clause" | "conditional_expression" | "match_conditional_expression" => 1,
        "binary_expression" => {
            let op = node.child_by_field_name("operator").and_then(|o| o.utf8_text(src).ok()).unwrap_or("");
            u32::from(matches!(op, "&&" | "||" | "and" | "or" | "??"))
        }
        // Closures and nested functions are measured on their own, not added to the parent.
        "anonymous_function" | "arrow_function" | "function_definition" => return 0,
        _ => 0,
    };
    let mut cursor = node.walk();
    own + node.named_children(&mut cursor).map(|c| count_branches(c, src)).sum::<u32>()
}

/// Functions whose size or branching changed between `old` and `new`, biggest increase first.
pub fn compare(old: &str, new: &str) -> Vec<ComplexityChange> {
    let (a, b) = (functions(old), functions(new));
    let mut names: Vec<&String> = a.keys().chain(b.keys()).collect();
    names.sort();
    names.dedup();
    let mut out: Vec<ComplexityChange> = names
        .into_iter()
        .map(|n| ComplexityChange { name: n.clone(), old: a.get(n).copied(), new: b.get(n).copied() })
        .filter(|c| {
            c.old.map(|m| (m.lines, m.branches)) != c.new.map(|m| (m.lines, m.branches))
        })
        .collect();
    out.sort_by_key(|c| (-c.branch_delta(), -c.line_delta(), c.name.clone()));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const OLD: &str = r#"<?php
class Invoice {
    public function total(array $lines): float {
        $sum = 0;
        foreach ($lines as $l) { $sum += $l; }
        return $sum;
    }
    public function gone() {}
}
"#;

    const NEW: &str = r#"<?php
class Invoice {
    public function total(array $lines): float {
        $sum = 0;
        foreach ($lines as $l) {
            if ($l > 0 && $l < 100) { $sum += $l; } elseif ($l >= 100) { $sum += 100; }
        }
        $f = fn($x) => $x ?? 0;
        return $sum > 1000 ? 1000 : $sum;
    }
}
function helper($x) { return $x ?: 1; }
"#;

    #[test]
    fn counts_branches() {
        let f = functions(NEW);
        // foreach, if, &&, elseif, ternary (the arrow function's ?? is not counted).
        assert_eq!(f["Invoice::total"].branches, 5);
        assert_eq!(f["Invoice::total"].cyclomatic(), 6);
        assert_eq!(f["Invoice::total"].line, 2);
        assert_eq!(f["helper"].branches, 1);
    }

    #[test]
    fn compares_versions() {
        let changes = compare(OLD, NEW);
        let names: Vec<_> = changes.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(names, vec!["Invoice::total", "helper", "Invoice::gone"]);
        assert_eq!(changes[0].branch_delta(), 4);
        assert!(changes[2].new.is_none());
    }
}
