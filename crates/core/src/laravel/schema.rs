//! Rebuild the database schema at a commit by reading Laravel migrations (no database, no PHP).
//!
//! Each migration's `up()` is read with tree-sitter and its `Schema::create/table/drop/rename`
//! calls are applied in filename order. Raw SQL and conditional logic are not interpreted.

use std::collections::BTreeMap;

use anyhow::Result;
use gix::ObjectId;
use tree_sitter::{Node, Parser};

use crate::git::Repo;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Column {
    pub name: String,
    /// e.g. `string(100)`, `decimal(8, 2)`, `bigIncrements`.
    pub kind: String,
    /// e.g. `nullable`, `unique`, `default('draft')`, `→ users.id`.
    pub modifiers: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Table {
    pub name: String,
    pub columns: Vec<Column>,
    /// Table-level indexes and keys, as written (e.g. `unique(email)`).
    pub indexes: Vec<String>,
    /// The migrations that touched this table, oldest first.
    pub migrations: Vec<String>,
}

impl Table {
    pub fn column(&self, name: &str) -> Option<&Column> {
        self.columns.iter().find(|c| c.name == name)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Schema {
    pub tables: BTreeMap<String, Table>,
    /// Migrations that were read, in order.
    pub migrations: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TableChange {
    Created(Table),
    Dropped(Table),
    Altered { table: String, added: Vec<Column>, removed: Vec<Column>, changed: Vec<(Column, Column)>, indexes_added: Vec<String>, indexes_removed: Vec<String> },
}

/// Read the schema from `database/migrations/*.php` at `commit`.
pub fn schema_at(repo: &Repo, commit: ObjectId) -> Result<Schema> {
    let mut files: Vec<_> = repo
        .list_files(commit)?
        .into_iter()
        .filter(|f| {
            f.path.strip_prefix("database/migrations/").is_some_and(|rest| !rest.contains('/') && rest.ends_with(".php"))
        })
        .collect();
    files.sort_by(|a, b| a.path.cmp(&b.path));
    let mut schema = Schema::default();
    for f in files {
        let text = String::from_utf8_lossy(&repo.read_blob(f.id)?).into_owned();
        let name = f.path.rsplit('/').next().unwrap_or(&f.path).trim_end_matches(".php").to_string();
        apply_migration(&mut schema, &name, &text);
    }
    Ok(schema)
}

/// Apply one migration's `up()` to `schema`.
pub fn apply_migration(schema: &mut Schema, name: &str, text: &str) {
    let mut parser = Parser::new();
    if parser.set_language(&tree_sitter_php::LANGUAGE_PHP.into()).is_err() {
        return;
    }
    let Some(tree) = parser.parse(text, None) else { return };
    let src = text.as_bytes();
    let Some(up) = find_up(tree.root_node(), src) else { return };
    schema.migrations.push(name.to_string());
    let mut calls = Vec::new();
    collect_schema_calls(up, src, &mut calls);
    for call in calls {
        apply_schema_call(schema, name, call, src);
    }
}

fn text<'a>(n: Node<'_>, src: &'a [u8]) -> &'a str {
    n.utf8_text(src).unwrap_or("")
}

fn find_up<'t>(node: Node<'t>, src: &[u8]) -> Option<Node<'t>> {
    if node.kind() == "method_declaration" && node.child_by_field_name("name").is_some_and(|n| text(n, src) == "up") {
        return node.child_by_field_name("body");
    }
    let mut cursor = node.walk();
    let children: Vec<_> = node.named_children(&mut cursor).collect();
    children.into_iter().find_map(|c| find_up(c, src))
}

/// `Schema::xxx(...)` calls, in source order.
fn collect_schema_calls<'t>(node: Node<'t>, src: &[u8], out: &mut Vec<Node<'t>>) {
    if node.kind() == "scoped_call_expression"
        && node.child_by_field_name("scope").is_some_and(|s| text(s, src).trim_start_matches('\\').ends_with("Schema"))
    {
        out.push(node);
        return;
    }
    let mut cursor = node.walk();
    let children: Vec<_> = node.named_children(&mut cursor).collect();
    for c in children {
        collect_schema_calls(c, src, out);
    }
}

#[derive(Debug, Clone)]
enum Arg {
    Str(String),
    List(Vec<String>),
    Other(String),
}

impl Arg {
    fn as_str(&self) -> Option<&str> {
        match self {
            Arg::Str(s) => Some(s),
            _ => None,
        }
    }

    fn names(&self) -> Vec<String> {
        match self {
            Arg::Str(s) => vec![s.clone()],
            Arg::List(l) => l.clone(),
            Arg::Other(_) => Vec::new(),
        }
    }

    fn display(&self) -> String {
        match self {
            Arg::Str(s) => format!("'{s}'"),
            Arg::List(l) => format!("[{}]", l.iter().map(|s| format!("'{s}'")).collect::<Vec<_>>().join(", ")),
            Arg::Other(o) => o.clone(),
        }
    }
}

fn unquote(s: &str) -> Option<String> {
    let s = s.trim();
    let quoted = (s.starts_with('\'') && s.ends_with('\'')) || (s.starts_with('"') && s.ends_with('"'));
    (quoted && s.len() >= 2).then(|| s[1..s.len() - 1].to_string())
}

fn args(call: Node<'_>, src: &[u8]) -> Vec<Arg> {
    let Some(arguments) = call.child_by_field_name("arguments") else { return Vec::new() };
    let mut cursor = arguments.walk();
    arguments
        .named_children(&mut cursor)
        .filter(|a| a.kind() == "argument")
        .map(|a| {
            let value = a.named_child(a.named_child_count().saturating_sub(1) as u32).unwrap_or(a);
            let raw = text(value, src);
            if let Some(s) = unquote(raw) {
                return Arg::Str(s);
            }
            if value.kind() == "array_creation_expression" {
                let items: Vec<String> = raw
                    .trim_matches(|c| c == '[' || c == ']')
                    .split(',')
                    .filter_map(unquote)
                    .collect();
                return Arg::List(items);
            }
            Arg::Other(raw.to_string())
        })
        .collect()
}

/// The closure passed to `Schema::create/table`.
fn closure_body<'t>(call: Node<'t>) -> Option<Node<'t>> {
    let arguments = call.child_by_field_name("arguments")?;
    let mut cursor = arguments.walk();
    let found = arguments.named_children(&mut cursor).find_map(|a| {
        let v = a.named_child(0)?;
        matches!(v.kind(), "anonymous_function" | "arrow_function").then_some(v)
    });
    found.and_then(|f| f.child_by_field_name("body"))
}

/// Flatten `$table->a(..)->b(..)` into `[(a, args), (b, args)]`.
fn chain(expr: Node<'_>, src: &[u8]) -> Option<Vec<(String, Vec<Arg>)>> {
    let mut calls = Vec::new();
    let mut node = expr;
    loop {
        match node.kind() {
            "member_call_expression" | "nullsafe_member_call_expression" => {
                let name = text(node.child_by_field_name("name")?, src).to_string();
                calls.push((name, args(node, src)));
                node = node.child_by_field_name("object")?;
            }
            "variable_name" => break,
            _ => return None,
        }
    }
    calls.reverse();
    Some(calls)
}

fn statements<'t>(body: Node<'t>) -> Vec<Node<'t>> {
    if body.kind() != "compound_statement" {
        // Arrow function: the body is the expression itself.
        return vec![body];
    }
    let mut cursor = body.walk();
    body.named_children(&mut cursor)
        .filter(|s| s.kind() == "expression_statement")
        .filter_map(|s| s.named_child(0))
        .collect()
}

fn apply_schema_call(schema: &mut Schema, migration: &str, call: Node<'_>, src: &[u8]) {
    let method = call.child_by_field_name("name").map(|n| text(n, src)).unwrap_or("");
    let a = args(call, src);
    let table_name = a.first().and_then(Arg::as_str).map(String::from);
    match method {
        "create" => {
            let Some(name) = table_name else { return };
            let mut table = Table { name: name.clone(), migrations: vec![migration.to_string()], ..Default::default() };
            if let Some(body) = closure_body(call) {
                for stmt in statements(body) {
                    if let Some(c) = chain(stmt, src) {
                        apply_blueprint(&mut table, c);
                    }
                }
            }
            schema.tables.insert(name, table);
        }
        "table" => {
            let Some(name) = table_name else { return };
            let table = schema.tables.entry(name.clone()).or_insert_with(|| Table { name: name.clone(), ..Default::default() });
            table.migrations.push(migration.to_string());
            if let Some(body) = closure_body(call) {
                for stmt in statements(body) {
                    if let Some(c) = chain(stmt, src) {
                        apply_blueprint(table, c);
                    }
                }
            }
        }
        "drop" | "dropIfExists" => {
            if let Some(name) = table_name {
                schema.tables.remove(&name);
            }
        }
        "rename" => {
            if let (Some(from), Some(to)) = (table_name, a.get(1).and_then(Arg::as_str))
                && let Some(mut t) = schema.tables.remove(&from)
            {
                t.name = to.to_string();
                t.migrations.push(migration.to_string());
                schema.tables.insert(to.to_string(), t);
            }
        }
        _ => {}
    }
}

/// Column types whose first argument is the column name.
const COLUMN_TYPES: &[&str] = &[
    "bigIncrements", "bigInteger", "binary", "boolean", "char", "dateTimeTz", "dateTime", "date", "decimal", "double",
    "enum", "float", "foreignId", "foreignUlid", "foreignUuid", "geography", "geometry", "increments", "integer",
    "ipAddress", "json", "jsonb", "longText", "macAddress", "mediumIncrements", "mediumInteger", "mediumText", "set",
    "smallIncrements", "smallInteger", "string", "text", "timeTz", "time", "timestampTz", "timestamp", "tinyIncrements",
    "tinyInteger", "tinyText", "unsignedBigInteger", "unsignedInteger", "unsignedMediumInteger", "unsignedSmallInteger",
    "unsignedTinyInteger", "ulid", "uuid", "year", "vector",
];

fn guess_table_for(column: &str) -> String {
    let base = column.trim_end_matches("_id");
    if base.ends_with('y') && !base.ends_with("ey") {
        format!("{}ies", &base[..base.len() - 1])
    } else if base.ends_with('s') {
        format!("{base}es")
    } else {
        format!("{base}s")
    }
}

fn snake(s: &str) -> String {
    let mut out = String::new();
    for (i, c) in s.chars().enumerate() {
        if c.is_uppercase() {
            if i > 0 {
                out.push('_');
            }
            out.extend(c.to_lowercase());
        } else {
            out.push(c);
        }
    }
    out
}

fn upsert(table: &mut Table, col: Column) {
    match table.columns.iter_mut().find(|c| c.name == col.name) {
        Some(existing) => *existing = col,
        None => table.columns.push(col),
    }
}

/// Apply one `$table->…` chain.
fn apply_blueprint(table: &mut Table, calls: Vec<(String, Vec<Arg>)>) {
    let Some((first, first_args)) = calls.first() else { return };
    let first_name = first_args.first().and_then(Arg::as_str).map(String::from);
    let modifiers: Vec<String> = calls[1..]
        .iter()
        .filter(|(m, _)| m != "change")
        .map(|(m, a)| match (m.as_str(), a.as_slice()) {
            ("constrained", []) => String::new(),
            (m, []) => m.to_string(),
            (m, a) => format!("{m}({})", a.iter().map(Arg::display).collect::<Vec<_>>().join(", ")),
        })
        .collect();
    let constrained = calls[1..].iter().find(|(m, _)| m == "constrained");
    let mut column = |name: String, kind: String| {
        let mut mods: Vec<String> = modifiers.iter().filter(|m| !m.is_empty()).cloned().collect();
        if let Some((_, a)) = constrained {
            let target = a.first().and_then(Arg::as_str).map(String::from).unwrap_or_else(|| guess_table_for(&name));
            mods.push(format!("→ {target}.id"));
        }
        upsert(table, Column { name, kind, modifiers: mods });
    };
    match first.as_str() {
        "id" => column(first_name.unwrap_or_else(|| "id".into()), "bigIncrements".into()),
        "timestamps" | "timestampsTz" | "nullableTimestamps" => {
            column("created_at".into(), "timestamp".into());
            column("updated_at".into(), "timestamp".into());
        }
        "softDeletes" | "softDeletesTz" => column(first_name.unwrap_or_else(|| "deleted_at".into()), "timestamp".into()),
        "rememberToken" => column("remember_token".into(), "string(100)".into()),
        "morphs" | "nullableMorphs" | "uuidMorphs" | "ulidMorphs" => {
            if let Some(n) = first_name {
                column(format!("{n}_id"), "unsignedBigInteger".into());
                column(format!("{n}_type"), "string".into());
            }
        }
        "foreignIdFor" => {
            let model = first_args.first().map(Arg::display).unwrap_or_default();
            let short = model.trim_end_matches("::class").rsplit('\\').next().unwrap_or("").to_string();
            let name = first_args.get(1).and_then(Arg::as_str).map(String::from).unwrap_or_else(|| format!("{}_id", snake(&short)));
            column(name, "foreignId".into());
        }
        "dropColumn" | "dropColumns" => {
            let names: Vec<String> = first_args.iter().flat_map(Arg::names).collect();
            table.columns.retain(|c| !names.contains(&c.name));
        }
        "renameColumn" => {
            if let (Some(from), Some(to)) = (first_name, first_args.get(1).and_then(Arg::as_str))
                && let Some(c) = table.columns.iter_mut().find(|c| c.name == from)
            {
                c.name = to.to_string();
            }
        }
        "dropTimestamps" | "dropTimestampsTz" => table.columns.retain(|c| c.name != "created_at" && c.name != "updated_at"),
        "dropSoftDeletes" | "dropSoftDeletesTz" => table.columns.retain(|c| c.name != "deleted_at"),
        "dropRememberToken" => table.columns.retain(|c| c.name != "remember_token"),
        "dropMorphs" => {
            if let Some(n) = first_name {
                table.columns.retain(|c| c.name != format!("{n}_id") && c.name != format!("{n}_type"));
            }
        }
        "index" | "unique" | "primary" | "fullText" | "spatialIndex" | "foreign" => {
            let cols: Vec<String> = first_args.first().map(Arg::names).unwrap_or_default();
            let mut s = format!("{first}({})", cols.join(", "));
            for m in modifiers.iter().filter(|m| !m.is_empty()) {
                s.push_str(&format!(" {m}"));
            }
            if !table.indexes.contains(&s) {
                table.indexes.push(s);
            }
        }
        "dropIndex" | "dropUnique" | "dropPrimary" | "dropForeign" | "dropFullText" => {
            // `dropIndex(['a', 'b'])` names columns; `dropIndex('t_a_b_index')` uses the
            // conventional name, which contains the column names.
            let kind = {
                let k = first.trim_start_matches("drop");
                let mut c = k.chars();
                c.next().map(|f| f.to_lowercase().chain(c).collect::<String>()).unwrap_or_default()
            };
            let matches = |index: &String| -> bool {
                let Some(cols) = index.strip_prefix(&format!("{kind}(")).and_then(|r| r.split(')').next()) else { return false };
                let cols: Vec<&str> = cols.split(", ").collect();
                match first_args.first() {
                    Some(Arg::List(l)) => l.iter().map(String::as_str).eq(cols.iter().copied()),
                    Some(Arg::Str(name)) => cols.iter().all(|c| name.contains(c)),
                    _ => false,
                }
            };
            table.indexes.retain(|i| !matches(i));
        }
        t if COLUMN_TYPES.contains(&t) => {
            let Some(name) = first_name else { return };
            let extra: Vec<String> = first_args[1..].iter().map(Arg::display).collect();
            let kind = if extra.is_empty() { t.to_string() } else { format!("{t}({})", extra.join(", ")) };
            column(name, kind);
        }
        _ => {}
    }
}

/// What changed between two schemas.
pub fn diff(old: &Schema, new: &Schema) -> Vec<TableChange> {
    let mut out = Vec::new();
    for (name, t) in &new.tables {
        match old.tables.get(name) {
            None => out.push(TableChange::Created(t.clone())),
            Some(o) => {
                let added: Vec<Column> = t.columns.iter().filter(|c| o.column(&c.name).is_none()).cloned().collect();
                let removed: Vec<Column> = o.columns.iter().filter(|c| t.column(&c.name).is_none()).cloned().collect();
                let changed: Vec<(Column, Column)> = t
                    .columns
                    .iter()
                    .filter_map(|c| o.column(&c.name).filter(|oc| *oc != c).map(|oc| (oc.clone(), c.clone())))
                    .collect();
                let indexes_added: Vec<String> = t.indexes.iter().filter(|i| !o.indexes.contains(i)).cloned().collect();
                let indexes_removed: Vec<String> = o.indexes.iter().filter(|i| !t.indexes.contains(i)).cloned().collect();
                if !(added.is_empty() && removed.is_empty() && changed.is_empty() && indexes_added.is_empty() && indexes_removed.is_empty()) {
                    out.push(TableChange::Altered { table: name.clone(), added, removed, changed, indexes_added, indexes_removed });
                }
            }
        }
    }
    for (name, t) in &old.tables {
        if !new.tables.contains_key(name) {
            out.push(TableChange::Dropped(t.clone()));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const USERS: &str = r#"<?php
use Illuminate\Database\Migrations\Migration;
use Illuminate\Database\Schema\Blueprint;
use Illuminate\Support\Facades\Schema;

return new class extends Migration
{
    public function up(): void
    {
        Schema::create('users', function (Blueprint $table) {
            $table->id();
            $table->string('name');
            $table->string('email', 191)->unique();
            $table->rememberToken();
            $table->timestamps();
        });
        Schema::create('invoices', function (Blueprint $table) {
            $table->id();
            $table->foreignId('user_id')->constrained()->cascadeOnDelete();
            $table->decimal('total', 8, 2)->default(0);
            $table->index(['user_id', 'total']);
        });
    }

    public function down(): void
    {
        Schema::dropIfExists('invoices');
        Schema::dropIfExists('users');
    }
};
"#;

    const ALTER: &str = r#"<?php
return new class extends Migration {
    public function up(): void
    {
        Schema::table('invoices', function (Blueprint $table) {
            $table->string('status')->default('draft')->after('total');
            $table->renameColumn('total', 'amount');
            $table->decimal('amount', 10, 2)->change();
            $table->dropIndex(['user_id', 'total']);
        });
        Schema::table('users', fn (Blueprint $table) => $table->dropColumn(['remember_token']));
        Schema::create('sessions', function ($table) { $table->string('id')->primary(); });
    }
};
"#;

    #[test]
    fn creates_tables() {
        let mut s = Schema::default();
        apply_migration(&mut s, "2024_01_01_000000_create_users", USERS);
        let users = &s.tables["users"];
        let names: Vec<_> = users.columns.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(names, vec!["id", "name", "email", "remember_token", "created_at", "updated_at"]);
        assert_eq!(users.column("email").unwrap().kind, "string(191)");
        assert_eq!(users.column("email").unwrap().modifiers, vec!["unique"]);
        let invoices = &s.tables["invoices"];
        assert_eq!(invoices.column("user_id").unwrap().modifiers, vec!["cascadeOnDelete", "→ users.id"]);
        assert_eq!(invoices.column("total").unwrap().kind, "decimal(8, 2)");
        assert_eq!(invoices.column("total").unwrap().modifiers, vec!["default(0)"]);
        assert_eq!(invoices.indexes, vec!["index(user_id, total)"]);
    }

    #[test]
    fn alters_and_diffs() {
        let mut before = Schema::default();
        apply_migration(&mut before, "a", USERS);
        let mut after = before.clone();
        apply_migration(&mut after, "b", ALTER);
        let inv = &after.tables["invoices"];
        let names: Vec<_> = inv.columns.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(names, vec!["id", "user_id", "amount", "status"]);
        assert_eq!(inv.column("amount").unwrap().kind, "decimal(10, 2)");
        assert!(inv.indexes.is_empty());
        assert!(after.tables["users"].column("remember_token").is_none());
        assert_eq!(inv.migrations, vec!["a", "b"]);

        let changes = diff(&before, &after);
        assert_eq!(changes.len(), 3);
        let TableChange::Altered { added, removed, .. } = &changes[0] else { panic!("{changes:?}") };
        assert_eq!(added.iter().map(|c| c.name.as_str()).collect::<Vec<_>>(), vec!["amount", "status"]);
        assert_eq!(removed[0].name, "total");
        assert!(matches!(&changes[1], TableChange::Created(t) if t.name == "sessions"));
        assert!(matches!(&changes[2], TableChange::Altered { table, .. } if table == "users"));
    }
}
