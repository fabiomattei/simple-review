//! Routes per commit: read statically from `routes/*.php`, or from `php artisan route:list`
//! (trusted repositories only), and compared between versions.

use std::collections::HashMap;
use std::path::Path;

use anyhow::{Context, Result};
use gix::ObjectId;
use serde_json::Value;
use tree_sitter::{Node, Parser};

use crate::git::Repo;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RouteAction {
    Controller { class: String, method: String },
    Closure,
    View(String),
    Redirect(String),
    Other(String),
}

impl RouteAction {
    pub fn label(&self) -> String {
        match self {
            RouteAction::Controller { class, method } => {
                let short = class.rsplit('\\').next().unwrap_or(class);
                if method == "__invoke" { short.to_string() } else { format!("{short}@{method}") }
            }
            RouteAction::Closure => "Closure".into(),
            RouteAction::View(v) => format!("view {v}"),
            RouteAction::Redirect(r) => format!("redirect {r}"),
            RouteAction::Other(o) => o.clone(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RouteDef {
    /// Upper-case verbs, e.g. ["GET"] or ["PUT", "PATCH"].
    pub methods: Vec<String>,
    /// Without leading slash; "/" for the root.
    pub uri: String,
    pub name: Option<String>,
    pub action: RouteAction,
    pub middleware: Vec<String>,
    /// Where it is defined (empty when read from artisan).
    pub file: String,
    /// 0-based.
    pub line: u32,
}

impl RouteDef {
    pub fn key(&self) -> String {
        format!("{} {}", self.methods.join("|"), self.uri)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RouteChange {
    Added(RouteDef),
    Removed(RouteDef),
    Changed { old: RouteDef, new: RouteDef },
}

#[derive(Clone, Default)]
struct Group {
    prefix: Vec<String>,
    name: String,
    middleware: Vec<String>,
    controller: Option<String>,
}

struct Ctx<'a> {
    src: &'a [u8],
    file: &'a str,
    uses: HashMap<String, String>,
    out: Vec<RouteDef>,
}

fn text<'a>(n: Node<'_>, src: &'a [u8]) -> &'a str {
    n.utf8_text(src).unwrap_or("")
}

fn unquote(s: &str) -> Option<String> {
    let s = s.trim();
    let quoted = (s.starts_with('\'') && s.ends_with('\'')) || (s.starts_with('"') && s.ends_with('"'));
    (quoted && s.len() >= 2).then(|| s[1..s.len() - 1].to_string())
}

fn list_or_str(raw: &str) -> Vec<String> {
    match unquote(raw) {
        Some(s) => vec![s],
        None => raw.trim_matches(|c| c == '[' || c == ']').split(',').filter_map(unquote).collect(),
    }
}

fn arg_nodes<'t>(call: Node<'t>) -> Vec<Node<'t>> {
    let Some(arguments) = call.child_by_field_name("arguments") else { return Vec::new() };
    let mut cursor = arguments.walk();
    arguments
        .named_children(&mut cursor)
        .filter(|a| a.kind() == "argument")
        .filter_map(|a| a.named_child(a.named_child_count().saturating_sub(1) as u32))
        .collect()
}

fn join_uri(prefix: &[String], uri: &str) -> String {
    let parts: Vec<&str> = prefix.iter().map(String::as_str).chain([uri]).flat_map(|p| p.split('/')).filter(|p| !p.is_empty()).collect();
    if parts.is_empty() { "/".into() } else { parts.join("/") }
}

impl Ctx<'_> {
    fn resolve(&self, class: &str) -> String {
        let class = class.trim().trim_end_matches("::class").trim_start_matches('\\');
        let (first, rest) = class.split_once('\\').map_or((class, None), |(f, r)| (f, Some(r)));
        match (self.uses.get(&first.to_ascii_lowercase()), rest) {
            (Some(full), Some(r)) => format!("{full}\\{r}"),
            (Some(full), None) => full.clone(),
            _ => class.to_string(),
        }
    }

    fn action(&self, node: Option<Node<'_>>, group: &Group) -> RouteAction {
        let Some(node) = node else { return RouteAction::Other(String::new()) };
        let raw = text(node, self.src);
        match node.kind() {
            "anonymous_function" | "arrow_function" => RouteAction::Closure,
            "array_creation_expression" => {
                let mut cursor = node.walk();
                let items: Vec<&str> = node
                    .named_children(&mut cursor)
                    .filter_map(|i| i.named_child(0))
                    .map(|v| text(v, self.src))
                    .collect();
                match items.as_slice() {
                    [class, method] => RouteAction::Controller {
                        class: self.resolve(class),
                        method: unquote(method).unwrap_or_else(|| method.to_string()),
                    },
                    [class] => RouteAction::Controller { class: self.resolve(class), method: "__invoke".into() },
                    _ => RouteAction::Other(raw.to_string()),
                }
            }
            "class_constant_access_expression" if raw.ends_with("::class") => {
                RouteAction::Controller { class: self.resolve(raw), method: "__invoke".into() }
            }
            _ => match unquote(raw) {
                Some(s) => match s.split_once('@') {
                    Some((c, m)) => RouteAction::Controller { class: self.resolve(c), method: m.into() },
                    None => match &group.controller {
                        Some(c) => RouteAction::Controller { class: c.clone(), method: s },
                        None => RouteAction::Other(s),
                    },
                },
                None => RouteAction::Other(raw.to_string()),
            },
        }
    }
}

/// `Route::a(..)->b(..)->c(..)`: the static call and the chained calls, in order.
fn route_chain<'t>(expr: Node<'t>, src: &[u8]) -> Option<Vec<(String, Node<'t>)>> {
    let mut calls = Vec::new();
    let mut node = expr;
    loop {
        match node.kind() {
            "member_call_expression" => {
                calls.push((text(node.child_by_field_name("name")?, src).to_string(), node));
                node = node.child_by_field_name("object")?;
            }
            "scoped_call_expression" => {
                let scope = text(node.child_by_field_name("scope")?, src).trim_start_matches('\\');
                if scope != "Route" && !scope.ends_with("\\Route") {
                    return None;
                }
                calls.push((text(node.child_by_field_name("name")?, src).to_string(), node));
                break;
            }
            _ => return None,
        }
    }
    calls.reverse();
    Some(calls)
}

const VERBS: &[&str] = &["get", "post", "put", "patch", "delete", "options", "any"];

fn singular(s: &str) -> String {
    if let Some(stem) = s.strip_suffix("ies") {
        format!("{stem}y")
    } else if let Some(stem) = s.strip_suffix('s') {
        stem.to_string()
    } else {
        s.to_string()
    }
}

fn visit(ctx: &mut Ctx<'_>, node: Node<'_>, group: &Group) {
    if node.kind() == "namespace_use_clause" {
        let mut cursor = node.walk();
        if let Some(target) = node.named_children(&mut cursor).find(|n| matches!(n.kind(), "name" | "qualified_name")) {
            let fqn = text(target, ctx.src).trim_start_matches('\\').to_string();
            let alias = node
                .child_by_field_name("alias")
                .map(|a| text(a, ctx.src).to_string())
                .unwrap_or_else(|| fqn.rsplit('\\').next().unwrap_or(&fqn).to_string());
            ctx.uses.insert(alias.to_ascii_lowercase(), fqn);
        }
        return;
    }
    if node.kind() == "expression_statement"
        && let Some(expr) = node.named_child(0)
        && let Some(chain) = route_chain(expr, ctx.src)
    {
        handle_chain(ctx, &chain, group, node.start_position().row as u32);
        return;
    }
    let mut cursor = node.walk();
    let children: Vec<_> = node.named_children(&mut cursor).collect();
    for c in children {
        visit(ctx, c, group);
    }
}

fn handle_chain(ctx: &mut Ctx<'_>, chain: &[(String, Node<'_>)], group: &Group, line: u32) {
    let src = ctx.src;
    let str_arg = |n: Node<'_>, i: usize| arg_nodes(n).get(i).and_then(|a| unquote(text(*a, src)));
    let raw_arg = |n: Node<'_>, i: usize| arg_nodes(n).get(i).map(|a| text(*a, src).to_string());

    // Collect group-ish attributes and route modifiers from the chain.
    let mut g = group.clone();
    let mut name = None;
    let mut middleware = Vec::new();
    let mut only: Option<Vec<String>> = None;
    let mut except: Vec<String> = Vec::new();
    let mut group_body = None;
    for (method, call) in chain {
        match method.as_str() {
            "prefix" => g.prefix.extend(str_arg(*call, 0)),
            "name" | "as" => {
                if let Some(n) = str_arg(*call, 0) {
                    name = Some(n.clone());
                    g.name.push_str(&n);
                }
            }
            "middleware" => {
                let m = raw_arg(*call, 0).map(|r| list_or_str(&r)).unwrap_or_default();
                middleware.extend(m.clone());
                g.middleware.extend(m);
            }
            "controller" => g.controller = raw_arg(*call, 0).map(|c| ctx.resolve(&c)),
            "only" => only = raw_arg(*call, 0).map(|r| list_or_str(&r)),
            "except" => except = raw_arg(*call, 0).map(|r| list_or_str(&r)).unwrap_or_default(),
            "group" => {
                let args = arg_nodes(*call);
                // Route::group(['prefix' => 'x', 'middleware' => [...]], fn)
                if let Some(attrs) = args.first().filter(|a| a.kind() == "array_creation_expression") {
                    let mut cursor = attrs.walk();
                    for item in attrs.named_children(&mut cursor) {
                        let (Some(k), Some(v)) = (item.named_child(0), item.named_child(1)) else { continue };
                        let (k, v) = (unquote(text(k, src)).unwrap_or_default(), text(v, src));
                        match k.as_str() {
                            "prefix" => g.prefix.extend(unquote(v)),
                            "middleware" => g.middleware.extend(list_or_str(v)),
                            "as" => g.name.push_str(&unquote(v).unwrap_or_default()),
                            _ => {}
                        }
                    }
                }
                group_body = args
                    .iter()
                    .find(|a| matches!(a.kind(), "anonymous_function" | "arrow_function"))
                    .and_then(|f| f.child_by_field_name("body"));
            }
            _ => {}
        }
    }
    if let Some(body) = group_body {
        visit(ctx, body, &g);
        return;
    }

    let (verb, call) = (&chain[0].0, chain[0].1);
    let full_name = name.map(|n| format!("{}{n}", group.name));
    let file = ctx.file.to_string();
    let mut found: Vec<RouteDef> = Vec::new();
    let mut push = |methods: Vec<String>, uri: String, action: RouteAction, name: Option<String>| {
        let mut mw = group.middleware.clone();
        mw.extend(middleware.iter().cloned());
        found.push(RouteDef { methods, uri: join_uri(&group.prefix, &uri), name, action, middleware: mw, file: file.clone(), line });
    };
    let args = arg_nodes(call);
    match verb.as_str() {
        v if VERBS.contains(&v) => {
            let Some(uri) = str_arg(call, 0) else { return };
            let methods = if v == "any" { vec!["ANY".into()] } else { vec![v.to_uppercase()] };
            let action = ctx.action(args.get(1).copied(), group);
            push(methods, uri, action, full_name);
        }
        "match" => {
            let methods: Vec<String> = raw_arg(call, 0).map(|r| list_or_str(&r)).unwrap_or_default().iter().map(|m| m.to_uppercase()).collect();
            let Some(uri) = str_arg(call, 1) else { return };
            let action = ctx.action(args.get(2).copied(), group);
            push(methods, uri, action, full_name);
        }
        "view" => {
            let (Some(uri), Some(view)) = (str_arg(call, 0), str_arg(call, 1)) else { return };
            push(vec!["GET".into()], uri, RouteAction::View(view), full_name);
        }
        "redirect" | "permanentRedirect" => {
            let (Some(uri), Some(to)) = (str_arg(call, 0), str_arg(call, 1)) else { return };
            push(vec!["ANY".into()], uri, RouteAction::Redirect(to), full_name);
        }
        "resource" | "apiResource" => {
            let Some(base) = str_arg(call, 0) else { return };
            let Some(class) = raw_arg(call, 1).map(|c| ctx.resolve(&c)) else { return };
            let param = singular(base.rsplit('/').next().unwrap_or(&base)).replace('-', "_");
            let mut actions: Vec<(&str, &str, String)> = vec![
                ("index", "GET", base.clone()),
                ("create", "GET", format!("{base}/create")),
                ("store", "POST", base.clone()),
                ("show", "GET", format!("{base}/{{{param}}}")),
                ("edit", "GET", format!("{base}/{{{param}}}/edit")),
                ("update", "PUT|PATCH", format!("{base}/{{{param}}}")),
                ("destroy", "DELETE", format!("{base}/{{{param}}}")),
            ];
            if verb == "apiResource" {
                actions.retain(|(a, _, _)| *a != "create" && *a != "edit");
            }
            let route_base = base.replace('/', ".");
            for (action, methods, uri) in actions {
                if only.as_ref().is_some_and(|o| !o.iter().any(|x| x == action)) || except.iter().any(|x| x == action) {
                    continue;
                }
                push(
                    methods.split('|').map(String::from).collect(),
                    uri,
                    RouteAction::Controller { class: class.clone(), method: action.into() },
                    Some(format!("{}{route_base}.{action}", group.name)),
                );
            }
        }
        _ => {}
    }
    ctx.out.extend(found);
}

/// Routes defined in one routes file. Files named `api.php` get the `api` prefix, as Laravel does.
pub fn parse_routes_file(path: &str, text: &str) -> Vec<RouteDef> {
    let mut parser = Parser::new();
    if parser.set_language(&tree_sitter_php::LANGUAGE_PHP.into()).is_err() {
        return Vec::new();
    }
    let Some(tree) = parser.parse(text, None) else { return Vec::new() };
    let mut ctx = Ctx { src: text.as_bytes(), file: path, uses: HashMap::new(), out: Vec::new() };
    let mut group = Group::default();
    if path.rsplit('/').next() == Some("api.php") {
        group.prefix.push("api".into());
        group.middleware.push("api".into());
    } else if path.rsplit('/').next() == Some("web.php") {
        group.middleware.push("web".into());
    }
    visit(&mut ctx, tree.root_node(), &group);
    ctx.out
}

/// All statically readable routes at `commit`.
pub fn routes_at(repo: &Repo, commit: ObjectId) -> Result<Vec<RouteDef>> {
    let mut out = Vec::new();
    for f in repo.list_files(commit)? {
        if f.path.starts_with("routes/") && f.path.ends_with(".php") {
            let text = String::from_utf8_lossy(&repo.read_blob(f.id)?).into_owned();
            out.extend(parse_routes_file(&f.path, &text));
        }
    }
    out.sort_by(|a, b| a.uri.cmp(&b.uri).then_with(|| a.methods.cmp(&b.methods)));
    Ok(out)
}

/// Parse `php artisan route:list --json`.
pub fn parse_route_list(json: &str) -> Result<Vec<RouteDef>> {
    let start = json.find('[').context("route:list printed no JSON")?;
    let v: Value = serde_json::from_str(&json[start..]).context("route:list output is not valid JSON")?;
    let mut out = Vec::new();
    for r in v.as_array().into_iter().flatten() {
        let s = |k: &str| r.get(k).and_then(Value::as_str).map(String::from);
        let methods: Vec<String> = s("method").unwrap_or_default().split('|').filter(|m| *m != "HEAD").map(String::from).collect();
        let action = match s("action").as_deref() {
            Some("Closure") | None => RouteAction::Closure,
            Some(a) => match a.split_once('@') {
                Some((c, m)) => RouteAction::Controller { class: c.to_string(), method: m.to_string() },
                None if a.contains('\\') => RouteAction::Controller { class: a.to_string(), method: "__invoke".into() },
                None => RouteAction::Other(a.to_string()),
            },
        };
        let middleware = r.get("middleware").and_then(Value::as_array).into_iter().flatten().filter_map(Value::as_str).map(String::from).collect();
        let uri = s("uri").unwrap_or_default();
        out.push(RouteDef {
            methods,
            uri: if uri.is_empty() { "/".into() } else { uri.trim_start_matches('/').to_string() },
            name: s("name"),
            action,
            middleware,
            file: String::new(),
            line: 0,
        });
    }
    Ok(out)
}

/// `php artisan route:list --json` in a snapshot. Boots the application: trusted repos only.
pub fn artisan_routes(dir: &Path, cancelled: &dyn Fn() -> bool) -> Result<Vec<RouteDef>> {
    let cmd = ["php".to_string()];
    let args: Vec<String> = ["artisan", "route:list", "--json", "--no-interaction"].map(String::from).to_vec();
    let (stdout, stderr) = crate::analysis::tools::run(dir, &cmd, &args, cancelled)?;
    parse_route_list(&stdout).with_context(|| stderr.lines().last().unwrap_or("").to_string())
}

pub fn diff(old: &[RouteDef], new: &[RouteDef]) -> Vec<RouteChange> {
    let index = |routes: &[RouteDef]| -> HashMap<String, RouteDef> { routes.iter().map(|r| (r.key(), r.clone())).collect() };
    let (o, n) = (index(old), index(new));
    let mut out = Vec::new();
    for r in new {
        match o.get(&r.key()) {
            None => out.push(RouteChange::Added(r.clone())),
            Some(prev) if (&prev.action, &prev.name, &prev.middleware) != (&r.action, &r.name, &r.middleware) => {
                out.push(RouteChange::Changed { old: prev.clone(), new: r.clone() })
            }
            _ => {}
        }
    }
    for r in old {
        if !n.contains_key(&r.key()) {
            out.push(RouteChange::Removed(r.clone()));
        }
    }
    out
}

/// Test files that look like they test `class` (by short name), e.g. `tests/**/InvoiceServiceTest.php`.
pub fn tests_for_class<'a>(class: &str, paths: impl IntoIterator<Item = &'a str>) -> Vec<String> {
    let short = class.rsplit('\\').next().unwrap_or(class);
    paths
        .into_iter()
        .filter(|p| p.starts_with("tests/") && p.ends_with(".php"))
        .filter(|p| p.rsplit('/').next().is_some_and(|n| n.starts_with(short) && n.contains("Test")))
        .map(String::from)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const API: &str = r#"<?php
use App\Http\Controllers\InvoiceController;
use App\Http\Controllers\Admin\ReportController as Reports;
use Illuminate\Support\Facades\Route;

Route::get('/user', fn () => auth()->user())->middleware('auth:sanctum');
Route::apiResource('invoices', InvoiceController::class)->only(['index', 'show']);
Route::prefix('admin')->name('admin.')->middleware(['auth', 'can:admin'])->group(function () {
    Route::get('reports/{report}', [Reports::class, 'show'])->name('reports.show');
    Route::controller(InvoiceController::class)->group(function () {
        Route::post('invoices/{invoice}/send', 'send');
    });
});
Route::match(['get', 'post'], 'hook', 'App\Http\Controllers\HookController@handle');
"#;

    #[test]
    fn reads_routes_statically() {
        let routes = parse_routes_file("routes/api.php", API);
        let keys: Vec<String> = routes.iter().map(|r| format!("{} {} {}", r.key(), r.action.label(), r.name.clone().unwrap_or_default())).collect();
        assert_eq!(
            keys,
            vec![
                "GET api/user Closure ",
                "GET api/invoices InvoiceController@index invoices.index",
                "GET api/invoices/{invoice} InvoiceController@show invoices.show",
                "GET api/admin/reports/{report} ReportController@show admin.reports.show",
                "POST api/admin/invoices/{invoice}/send InvoiceController@send ",
                "GET|POST api/hook HookController@handle ",
            ]
        );
        assert_eq!(routes[0].middleware, vec!["api", "auth:sanctum"]);
        assert_eq!(routes[3].middleware, vec!["api", "auth", "can:admin"]);
        assert_eq!(
            routes[3].action,
            RouteAction::Controller { class: "App\\Http\\Controllers\\Admin\\ReportController".into(), method: "show".into() }
        );
        assert_eq!(routes[0].line, 5);
    }

    #[test]
    fn parses_artisan_and_diffs() {
        let json = r#"[{"domain":null,"method":"GET|HEAD","uri":"api/user","name":null,"action":"Closure","middleware":["api"]},
                       {"domain":null,"method":"POST","uri":"login","name":"login","action":"App\\Http\\Controllers\\Auth@store","middleware":["web"]}]"#;
        let routes = parse_route_list(json).unwrap();
        assert_eq!(routes[0].methods, vec!["GET"]);
        assert_eq!(routes[1].action.label(), "Auth@store");

        let mut changed = routes.clone();
        changed[1].name = Some("auth.login".into());
        changed.remove(0);
        changed.push(RouteDef { uri: "logout".into(), ..routes[1].clone() });
        let d = diff(&routes, &changed);
        assert_eq!(d.len(), 3);
        assert!(matches!(&d[0], RouteChange::Changed { new, .. } if new.name.as_deref() == Some("auth.login")));
        assert!(matches!(&d[1], RouteChange::Added(r) if r.uri == "logout"));
        assert!(matches!(&d[2], RouteChange::Removed(r) if r.uri == "api/user"));
    }

    #[test]
    fn finds_tests_for_a_class() {
        let paths = ["tests/Unit/InvoiceServiceTest.php", "tests/Feature/InvoiceServiceSendingTest.php", "tests/Unit/UserTest.php", "app/InvoiceServiceTest.php"];
        assert_eq!(tests_for_class("App\\Services\\InvoiceService", paths).len(), 2);
    }
}
