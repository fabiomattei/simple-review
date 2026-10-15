//! Extract definitions and references from one PHP file with tree-sitter.

use std::collections::HashMap;

use tree_sitter::{Node, Parser};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SymbolKind {
    Class,
    Interface,
    Trait,
    Enum,
    Function,
    Method,
    Property,
    Constant,
    EnumCase,
}

impl SymbolKind {
    pub fn is_class_like(self) -> bool {
        matches!(self, SymbolKind::Class | SymbolKind::Interface | SymbolKind::Trait | SymbolKind::Enum)
    }

    pub fn label(self) -> &'static str {
        match self {
            SymbolKind::Class => "class",
            SymbolKind::Interface => "interface",
            SymbolKind::Trait => "trait",
            SymbolKind::Enum => "enum",
            SymbolKind::Function => "function",
            SymbolKind::Method => "method",
            SymbolKind::Property => "property",
            SymbolKind::Constant => "constant",
            SymbolKind::EnumCase => "enum case",
        }
    }
}

/// A position on one line: 0-based line, byte columns.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Span {
    pub line: u32,
    pub start: u32,
    pub end: u32,
}

impl Span {
    pub fn contains(&self, line: u32, col: u32) -> bool {
        self.line == line && self.start <= col && col < self.end.max(self.start + 1)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Symbol {
    pub name: String,
    /// Fully qualified name; members are `Class::name` (properties without `$`).
    pub fqn: String,
    pub kind: SymbolKind,
    /// Fully qualified class for members.
    pub container: Option<String>,
    /// Position of the name.
    pub span: Span,
    /// Line range of the whole declaration (0-based, inclusive).
    pub first_line: u32,
    pub last_line: u32,
    /// The declaration's first line, trimmed.
    pub signature: String,
    /// For classes: the parent class, implemented interfaces and used traits, fully qualified.
    pub supertypes: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RefKind {
    /// A class-like name, resolved to a fully qualified name.
    Class { fqn: String },
    /// A function call: fully qualified candidates in lookup order (namespaced, then global).
    Function { candidates: Vec<String> },
    /// `->name()` / `::name()`; `class` is known for `$this`, `self`, `static`, `parent` and `Foo::`.
    Method { class: Option<String> },
    /// `->name` / `::$name`.
    Property { class: Option<String> },
    /// `Foo::NAME` (constants and enum cases).
    Constant { class: Option<String> },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reference {
    pub name: String,
    pub kind: RefKind,
    pub span: Span,
    /// Enclosing class, if any.
    pub in_class: Option<String>,
    /// The line's text, trimmed and shortened, for listings.
    pub context: String,
}

#[derive(Debug, Clone, Default)]
pub struct FileSymbols {
    pub symbols: Vec<Symbol>,
    pub refs: Vec<Reference>,
}

impl FileSymbols {
    /// The definition whose name is at `line`/`col`.
    pub fn symbol_at(&self, line: u32, col: u32) -> Option<&Symbol> {
        self.symbols.iter().find(|s| s.span.contains(line, col))
    }

    pub fn reference_at(&self, line: u32, col: u32) -> Option<&Reference> {
        self.refs.iter().find(|r| r.span.contains(line, col))
    }
}

pub fn is_php_path(path: &str) -> bool {
    path.ends_with(".php") && !path.ends_with(".blade.php")
}

const CONTEXT_CHARS: usize = 140;

struct Ctx<'a> {
    src: &'a [u8],
    lines: Vec<&'a str>,
    namespace: String,
    /// alias (lower-case) -> fully qualified name
    uses: HashMap<String, String>,
    function_uses: HashMap<String, String>,
    out: FileSymbols,
}

#[derive(Clone, Default)]
struct Scope {
    class: Option<String>,
    parent: Option<String>,
}

pub fn parse_php(text: &str) -> FileSymbols {
    let mut parser = Parser::new();
    if parser.set_language(&tree_sitter_php::LANGUAGE_PHP.into()).is_err() {
        return FileSymbols::default();
    }
    let Some(tree) = parser.parse(text, None) else { return FileSymbols::default() };
    let mut ctx = Ctx {
        src: text.as_bytes(),
        lines: text.split('\n').collect(),
        namespace: String::new(),
        uses: HashMap::new(),
        function_uses: HashMap::new(),
        out: FileSymbols::default(),
    };
    visit_children(&mut ctx, tree.root_node(), &Scope::default());
    ctx.out
}

impl Ctx<'_> {
    fn text(&self, node: Node<'_>) -> &str {
        node.utf8_text(self.src).unwrap_or("")
    }

    fn span(&self, node: Node<'_>) -> Span {
        let s = node.start_position();
        let e = node.end_position();
        let end = if e.row == s.row { e.column } else { self.lines.get(s.row).map_or(s.column, |l| l.len()) };
        Span { line: s.row as u32, start: s.column as u32, end: end as u32 }
    }

    fn line_context(&self, row: usize) -> String {
        let line = self.lines.get(row).map_or("", |l| l.trim());
        match line.char_indices().nth(CONTEXT_CHARS) {
            Some((i, _)) => format!("{}…", &line[..i]),
            None => line.to_string(),
        }
    }

    fn qualify(&self, name: &str) -> String {
        if self.namespace.is_empty() { name.to_string() } else { format!("{}\\{name}", self.namespace) }
    }

    /// Resolve a class name as written to a fully qualified name.
    fn resolve_class(&self, written: &str, scope: &Scope) -> Option<String> {
        let lower = written.to_ascii_lowercase();
        match lower.as_str() {
            "self" | "static" | "$this" => return scope.class.clone(),
            "parent" => return scope.parent.clone(),
            _ => {}
        }
        if let Some(abs) = written.strip_prefix('\\') {
            return Some(abs.to_string());
        }
        if let Some(rest) = written.strip_prefix("namespace\\") {
            return Some(self.qualify(rest));
        }
        let (first, rest) = match written.split_once('\\') {
            Some((f, r)) => (f, Some(r)),
            None => (written, None),
        };
        if let Some(target) = self.uses.get(&first.to_ascii_lowercase()) {
            return Some(match rest {
                Some(r) => format!("{target}\\{r}"),
                None => target.clone(),
            });
        }
        Some(self.qualify(written))
    }

    fn function_candidates(&self, written: &str) -> Vec<String> {
        if let Some(abs) = written.strip_prefix('\\') {
            return vec![abs.to_string()];
        }
        if !written.contains('\\') {
            if let Some(target) = self.function_uses.get(&written.to_ascii_lowercase()) {
                return vec![target.clone()];
            }
            let mut c = vec![self.qualify(written)];
            if !self.namespace.is_empty() {
                c.push(written.to_string());
            }
            return c;
        }
        vec![self.resolve_class(written, &Scope::default()).unwrap_or_else(|| written.to_string())]
    }

    fn add_ref(&mut self, name_node: Node<'_>, name: String, kind: RefKind, scope: &Scope) {
        if name.is_empty() {
            return;
        }
        let span = self.span(name_node);
        let context = self.line_context(span.line as usize);
        self.out.refs.push(Reference { name, kind, span, in_class: scope.class.clone(), context });
    }

    fn add_class_ref(&mut self, node: Node<'_>, scope: &Scope) {
        if !matches!(node.kind(), "name" | "qualified_name" | "relative_name") {
            return;
        }
        let written = self.text(node).to_string();
        if matches!(written.to_ascii_lowercase().as_str(), "self" | "static" | "parent") && scope.class.is_none() {
            return;
        }
        if let Some(fqn) = self.resolve_class(&written, scope) {
            let short = fqn.rsplit('\\').next().unwrap_or(&fqn).to_string();
            // Point at the last segment so clicks on `Foo` in `App\Foo` hit, as do clicks elsewhere in it.
            self.add_ref(node, short, RefKind::Class { fqn }, scope);
        }
    }

    fn add_symbol(&mut self, decl: Node<'_>, name_node: Node<'_>, name: String, kind: SymbolKind, scope: &Scope) {
        let fqn = match (&scope.class, kind) {
            (Some(class), SymbolKind::Method | SymbolKind::Property | SymbolKind::Constant | SymbolKind::EnumCase) => {
                format!("{class}::{name}")
            }
            _ => self.qualify(&name),
        };
        let container = match kind {
            SymbolKind::Method | SymbolKind::Property | SymbolKind::Constant | SymbolKind::EnumCase => scope.class.clone(),
            _ => None,
        };
        let first_line = decl.start_position().row as u32;
        let signature = self.line_context(name_node.start_position().row);
        self.out.symbols.push(Symbol {
            name,
            fqn,
            kind,
            container,
            span: self.span(name_node),
            first_line,
            last_line: decl.end_position().row as u32,
            signature,
            supertypes: Vec::new(),
        });
    }
}

fn visit_children(ctx: &mut Ctx<'_>, node: Node<'_>, scope: &Scope) {
    let mut cursor = node.walk();
    let children: Vec<Node<'_>> = node.named_children(&mut cursor).collect();
    for child in children {
        visit(ctx, child, scope);
    }
}

fn strip_dollar(s: &str) -> String {
    s.trim_start_matches('$').to_string()
}

fn visit(ctx: &mut Ctx<'_>, node: Node<'_>, scope: &Scope) {
    match node.kind() {
        "namespace_definition" => {
            let name = node.child_by_field_name("name").map(|n| ctx.text(n).to_string()).unwrap_or_default();
            match node.child_by_field_name("body") {
                Some(body) => {
                    let saved = std::mem::replace(&mut ctx.namespace, name);
                    visit_children(ctx, body, scope);
                    ctx.namespace = saved;
                }
                // `namespace Foo;` applies to the rest of the file.
                None => ctx.namespace = name,
            }
        }
        "namespace_use_declaration" => {
            let is_function = node.child_by_field_name("type").is_some_and(|t| ctx.text(t) == "function");
            let group_prefix = {
                let mut c = node.walk();
                node.named_children(&mut c).find(|n| n.kind() == "namespace_name").map(|n| ctx.text(n).to_string())
            };
            let mut clauses = Vec::new();
            let mut c = node.walk();
            for child in node.named_children(&mut c) {
                match child.kind() {
                    "namespace_use_clause" => clauses.push(child),
                    "namespace_use_group" => {
                        let mut gc = child.walk();
                        clauses.extend(child.named_children(&mut gc).filter(|n| n.kind() == "namespace_use_clause"));
                    }
                    _ => {}
                }
            }
            for clause in clauses {
                let mut cc = clause.walk();
                let Some(target) = clause.named_children(&mut cc).find(|n| matches!(n.kind(), "name" | "qualified_name")) else {
                    continue;
                };
                let written = ctx.text(target).trim_start_matches('\\').to_string();
                let fqn = match &group_prefix {
                    Some(prefix) => format!("{prefix}\\{written}"),
                    None => written,
                };
                let alias = clause
                    .child_by_field_name("alias")
                    .map(|a| ctx.text(a).to_string())
                    .unwrap_or_else(|| fqn.rsplit('\\').next().unwrap_or(&fqn).to_string());
                let clause_is_function = is_function || clause.child_by_field_name("type").is_some_and(|t| ctx.text(t) == "function");
                if clause_is_function {
                    ctx.function_uses.insert(alias.to_ascii_lowercase(), fqn);
                } else {
                    let short = fqn.rsplit('\\').next().unwrap_or(&fqn).to_string();
                    ctx.add_ref(target, short, RefKind::Class { fqn: fqn.clone() }, scope);
                    ctx.uses.insert(alias.to_ascii_lowercase(), fqn);
                }
            }
        }
        "class_declaration" | "interface_declaration" | "trait_declaration" | "enum_declaration" => {
            let kind = match node.kind() {
                "class_declaration" => SymbolKind::Class,
                "interface_declaration" => SymbolKind::Interface,
                "trait_declaration" => SymbolKind::Trait,
                _ => SymbolKind::Enum,
            };
            let Some(name_node) = node.child_by_field_name("name") else { return };
            let name = ctx.text(name_node).to_string();
            let fqn = ctx.qualify(&name);
            let mut parent = None;
            let mut supertypes = Vec::new();
            let mut c = node.walk();
            for child in node.named_children(&mut c) {
                if matches!(child.kind(), "base_clause" | "class_interface_clause") {
                    let mut cc = child.walk();
                    for n in child.named_children(&mut cc) {
                        ctx.add_class_ref(n, scope);
                        if let Some(resolved) = ctx.resolve_class(ctx.text(n), scope) {
                            if child.kind() == "base_clause" && kind == SymbolKind::Class {
                                parent = Some(resolved.clone());
                            }
                            supertypes.push(resolved);
                        }
                    }
                }
            }
            ctx.add_symbol(node, name_node, name, kind, scope);
            let inner = Scope { class: Some(fqn), parent };
            if let Some(body) = node.child_by_field_name("body") {
                // Collect traits first so the class symbol lists them.
                let mut bc = body.walk();
                for item in body.named_children(&mut bc) {
                    if item.kind() == "use_declaration" {
                        let mut ic = item.walk();
                        for n in item.named_children(&mut ic) {
                            if let Some(resolved) = ctx.resolve_class(ctx.text(n), &inner)
                                && matches!(n.kind(), "name" | "qualified_name" | "relative_name")
                            {
                                supertypes.push(resolved);
                            }
                        }
                    }
                }
                let idx = ctx.out.symbols.len() - 1;
                ctx.out.symbols[idx].supertypes = supertypes;
                visit_children(ctx, body, &inner);
            }
        }
        "method_declaration" | "function_definition" => {
            if let Some(name_node) = node.child_by_field_name("name") {
                let name = ctx.text(name_node).to_string();
                let kind = if node.kind() == "method_declaration" { SymbolKind::Method } else { SymbolKind::Function };
                // Functions declared inside methods are still global functions.
                let sym_scope = if kind == SymbolKind::Function { Scope::default() } else { scope.clone() };
                ctx.add_symbol(node, name_node, name, kind, &sym_scope);
            }
            visit_children(ctx, node, scope);
        }
        "property_declaration" => {
            let mut c = node.walk();
            let elements: Vec<_> = node.named_children(&mut c).filter(|n| n.kind() == "property_element").collect();
            for el in elements {
                if let Some(name_node) = el.child_by_field_name("name") {
                    let name = strip_dollar(ctx.text(name_node));
                    ctx.add_symbol(node, name_node, name, SymbolKind::Property, scope);
                }
            }
            visit_children(ctx, node, scope);
        }
        "property_promotion_parameter" => {
            if let Some(name_node) = node.child_by_field_name("name") {
                let name = strip_dollar(ctx.text(name_node));
                ctx.add_symbol(node, name_node, name, SymbolKind::Property, scope);
            }
            visit_children(ctx, node, scope);
        }
        "const_declaration" => {
            let mut c = node.walk();
            let elements: Vec<_> = node.named_children(&mut c).filter(|n| n.kind() == "const_element").collect();
            for el in elements {
                let mut ec = el.walk();
                if let Some(name_node) = el.named_children(&mut ec).find(|n| n.kind() == "name") {
                    let name = ctx.text(name_node).to_string();
                    ctx.add_symbol(node, name_node, name, SymbolKind::Constant, scope);
                }
            }
            visit_children(ctx, node, scope);
        }
        "enum_case" => {
            if let Some(name_node) = node.child_by_field_name("name") {
                let name = ctx.text(name_node).to_string();
                ctx.add_symbol(node, name_node, name, SymbolKind::EnumCase, scope);
            }
            visit_children(ctx, node, scope);
        }
        "named_type" | "base_clause" | "class_interface_clause" | "attribute" | "use_declaration" => {
            let mut c = node.walk();
            let names: Vec<_> = node.named_children(&mut c).collect();
            for n in names {
                if matches!(n.kind(), "name" | "qualified_name" | "relative_name") {
                    ctx.add_class_ref(n, scope);
                } else {
                    visit(ctx, n, scope);
                }
            }
        }
        "object_creation_expression" => {
            let mut c = node.walk();
            let children: Vec<_> = node.named_children(&mut c).collect();
            for n in children {
                if matches!(n.kind(), "name" | "qualified_name" | "relative_name") {
                    ctx.add_class_ref(n, scope);
                } else {
                    visit(ctx, n, scope);
                }
            }
        }
        "function_call_expression" => {
            if let Some(f) = node.child_by_field_name("function")
                && matches!(f.kind(), "name" | "qualified_name" | "relative_name")
            {
                let written = ctx.text(f).to_string();
                let candidates = ctx.function_candidates(&written);
                let short = written.rsplit('\\').next().unwrap_or(&written).to_string();
                ctx.add_ref(f, short, RefKind::Function { candidates }, scope);
            }
            visit_children(ctx, node, scope);
        }
        "member_call_expression" | "nullsafe_member_call_expression" | "member_access_expression" | "nullsafe_member_access_expression" => {
            if let Some(name_node) = node.child_by_field_name("name")
                && name_node.kind() == "name"
            {
                let on_this = node.child_by_field_name("object").is_some_and(|o| ctx.text(o) == "$this");
                let class = if on_this { scope.class.clone() } else { None };
                let name = ctx.text(name_node).to_string();
                let kind = if node.kind().contains("call") { RefKind::Method { class } } else { RefKind::Property { class } };
                ctx.add_ref(name_node, name, kind, scope);
            }
            visit_children(ctx, node, scope);
        }
        "scoped_call_expression" | "scoped_property_access_expression" | "class_constant_access_expression" => {
            let (scope_node, name_node) = if node.kind() == "class_constant_access_expression" {
                let mut c = node.walk();
                let kids: Vec<_> = node.named_children(&mut c).collect();
                (kids.first().copied(), kids.get(1).copied())
            } else {
                (node.child_by_field_name("scope"), node.child_by_field_name("name"))
            };
            let class = scope_node.and_then(|s| {
                let written = ctx.text(s);
                match s.kind() {
                    "name" | "qualified_name" | "relative_name" | "relative_scope" => ctx.resolve_class(written, scope),
                    "variable_name" if written == "$this" => scope.class.clone(),
                    _ => None,
                }
            });
            if let Some(s) = scope_node {
                if matches!(s.kind(), "name" | "qualified_name" | "relative_name") {
                    ctx.add_class_ref(s, scope);
                } else if s.kind() != "relative_scope" {
                    visit(ctx, s, scope);
                }
            }
            if let Some(n) = name_node
                && matches!(n.kind(), "name" | "variable_name")
            {
                let name = strip_dollar(ctx.text(n));
                if name != "class" {
                    let kind = match node.kind() {
                        "scoped_call_expression" => RefKind::Method { class },
                        "scoped_property_access_expression" => RefKind::Property { class },
                        _ => RefKind::Constant { class },
                    };
                    ctx.add_ref(n, name, kind, scope);
                }
            }
            if let Some(args) = node.child_by_field_name("arguments") {
                visit_children(ctx, args, scope);
            }
        }
        _ => visit_children(ctx, node, scope),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SRC: &str = r#"<?php

namespace App\Services;

use App\Models\User;
use App\Support\{Money, Clock as Time};
use function App\Support\format_money;

#[Attribute]
class InvoiceService extends BaseService implements HasTotals
{
    use Loggable;

    public const RATE = 0.2;
    private array $lines = [];

    public function __construct(private Time $clock) {}

    public function total(User $user): Money
    {
        $sum = $this->round(self::RATE);
        $user->invoices()->get();
        $x = new Money($this->lines);
        parent::boot();
        return format_money(helper($sum), static::$cache);
    }

    private function round(float $v): float { return $v; }
}

function helper($x) { return $x; }
"#;

    fn find<'a>(f: &'a FileSymbols, name: &str) -> &'a Symbol {
        f.symbols.iter().find(|s| s.name == name).unwrap_or_else(|| panic!("no symbol {name}"))
    }

    fn refs<'a>(f: &'a FileSymbols, name: &str) -> Vec<&'a Reference> {
        f.refs.iter().filter(|r| r.name == name).collect()
    }

    #[test]
    fn extracts_definitions() {
        let f = parse_php(SRC);
        let class = find(&f, "InvoiceService");
        assert_eq!(class.fqn, "App\\Services\\InvoiceService");
        assert_eq!(class.kind, SymbolKind::Class);
        assert_eq!(
            class.supertypes,
            vec!["App\\Services\\BaseService", "App\\Services\\HasTotals", "App\\Services\\Loggable"]
        );
        let total = find(&f, "total");
        assert_eq!(total.fqn, "App\\Services\\InvoiceService::total");
        assert_eq!(total.container.as_deref(), Some("App\\Services\\InvoiceService"));
        assert_eq!(total.span.line, 18);
        assert_eq!(&SRC.lines().nth(18).unwrap()[total.span.start as usize..total.span.end as usize], "total");
        assert_eq!(find(&f, "lines").kind, SymbolKind::Property);
        assert_eq!(find(&f, "clock").kind, SymbolKind::Property);
        assert_eq!(find(&f, "RATE").kind, SymbolKind::Constant);
        assert_eq!(find(&f, "helper").fqn, "App\\Services\\helper");
    }

    #[test]
    fn resolves_references() {
        let f = parse_php(SRC);
        let class = "App\\Services\\InvoiceService".to_string();
        assert_eq!(refs(&f, "round")[0].kind, RefKind::Method { class: Some(class.clone()) });
        assert_eq!(refs(&f, "RATE")[0].kind, RefKind::Constant { class: Some(class.clone()) });
        assert_eq!(refs(&f, "invoices")[0].kind, RefKind::Method { class: None });
        assert_eq!(refs(&f, "lines")[0].kind, RefKind::Property { class: Some(class.clone()) });
        assert_eq!(refs(&f, "boot")[0].kind, RefKind::Method { class: Some("App\\Services\\BaseService".into()) });
        assert_eq!(refs(&f, "cache")[0].kind, RefKind::Property { class: Some(class) });

        let money: Vec<_> = refs(&f, "Money").iter().map(|r| r.kind.clone()).collect();
        assert!(money.iter().all(|k| *k == RefKind::Class { fqn: "App\\Support\\Money".into() }));
        assert_eq!(money.len(), 3, "use, return type, new");
        assert!(refs(&f, "Time").is_empty());
        assert_eq!(refs(&f, "Clock")[0].kind, RefKind::Class { fqn: "App\\Support\\Clock".into() });
        assert_eq!(refs(&f, "User").len(), 2);

        assert_eq!(refs(&f, "format_money")[0].kind, RefKind::Function { candidates: vec!["App\\Support\\format_money".into()] });
        assert_eq!(
            refs(&f, "helper")[0].kind,
            RefKind::Function { candidates: vec!["App\\Services\\helper".into(), "helper".into()] }
        );
    }

    #[test]
    fn lookup_by_position() {
        let f = parse_php(SRC);
        let line = SRC.lines().position(|l| l.contains("$this->round(")).unwrap() as u32;
        let col = SRC.lines().nth(line as usize).unwrap().find("round").unwrap() as u32 + 2;
        assert_eq!(f.reference_at(line, col).unwrap().name, "round");
        let decl = find(&f, "round").span;
        assert_eq!(f.symbol_at(decl.line, decl.start).unwrap().name, "round");
    }
}
