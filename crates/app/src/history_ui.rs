//! History, bisect and Laravel views: the Phase 5 parts of the app.

use std::collections::HashSet;

use review_core::ObjectId;
use review_core::analysis::{complexity, snapshot};
use review_core::bisect::Verdict;
use review_core::history;
use review_core::laravel::{composer, risk, routes, schema};

use super::ReviewApp;
use crate::state::{JobResult, LaravelData, PeekTab, Side, ViewState};
use crate::ui::BisectMark;

impl ReviewApp {
    // ---- risk badges ----

    /// Work out risk flags for commits the list is showing.
    pub(super) fn compute_risks(&mut self, ids: Vec<ObjectId>) {
        let ids: Vec<ObjectId> = ids.into_iter().filter(|id| !self.risks.contains_key(id) && !self.risk_pending.contains(id)).collect();
        if ids.is_empty() {
            return;
        }
        let Some(rs) = self.rs.as_ref() else { return };
        let repo = rs.repo.clone();
        self.risk_pending.extend(ids.iter().copied());
        self.pool.spawn(format!("Checking {} commits for risky files", ids.len()), move |ctx, send| {
            let mut out = Vec::new();
            for id in ids {
                if ctx.is_cancelled() {
                    break;
                }
                let parent = repo.commit(id).ok().and_then(|c| c.parents.first().copied());
                let risks = repo
                    .changed_files(parent, id)
                    .map(|files| risk::risks(files.iter().map(|f| f.path())))
                    .unwrap_or_default();
                out.push((id, risks));
            }
            send(JobResult::Risks(out));
        });
    }

    // ---- pickaxe and line history ----

    pub(super) fn pickaxe_search(&mut self) {
        let query = self.history_panel.query.trim().to_string();
        if query.is_empty() {
            return;
        }
        let Some(rs) = self.rs.as_ref() else { return };
        let Some(tip) = rs.range.as_ref().map(|r| r.tip).or(rs.head) else { return };
        let path = if self.history_panel.this_file {
            rs.commit.as_ref().and_then(|c| c.file.as_ref()).map(|f| f.change.path().to_string())
        } else {
            None
        };
        let regex = self.history_panel.regex;
        let repo = rs.repo.clone();
        let label = match &path {
            Some(p) => format!("“{query}” in {p}"),
            None => format!("“{query}”"),
        };
        self.history_panel.pickaxe = Some((label.clone(), Err(None)));
        self.pool.spawn(format!("Searching history for {label}"), move |_, send| {
            let result = history::pickaxe(&repo, tip, &query, regex, path.as_deref(), 200).map_err(|e| format!("{e:#}"));
            send(JobResult::Pickaxe { query: label, result });
        });
    }

    /// History of the selected lines, or of the function (or line) at the cursor.
    pub(super) fn line_history(&mut self) {
        let Some((side, start, end)) = self.history_target() else {
            self.toast("Click a line (or select lines) first.");
            return;
        };
        let Some((version, path)) = self.version_of(side) else { return };
        let Some(rs) = self.rs.as_ref() else { return };
        let repo = rs.repo.clone();
        let label = format!("{path}:{}", review_core::review::model::line_label(start, end));
        self.history_panel.lines = Some((label.clone(), Err(None)));
        self.peek.open = true;
        self.peek.tab = PeekTab::History;
        self.pool.spawn(format!("History of {label}"), move |_, send| {
            let result = (|| -> anyhow::Result<crate::state::LineHistoryResult> {
                let steps = history::line_history(&repo, version.commit, &path, start, end)?
                    .into_iter()
                    .map(|s| Ok((s.clone(), repo.commit(s.commit)?)))
                    .collect::<anyhow::Result<Vec<_>>>()?;
                // Whole ranges reach back through shared lines like `}`; the first line (a
                // function's declaration) says who wrote it.
                let origin = history::original_author(&repo, version.commit, &path, start, start)?;
                Ok(crate::state::LineHistoryResult { steps, origin })
            })()
            .map_err(|e| format!("{e:#}"));
            send(JobResult::LineHistory { label, result });
        });
    }

    /// Selected lines, else the enclosing function, else the cursor line.
    fn history_target(&self) -> Option<(Side, u32, u32)> {
        let sf = self.rs.as_ref()?.commit.as_ref()?.file.as_ref()?;
        if let Some(sel) = sf.selection {
            return Some(sel);
        }
        let cursor = sf.cursor?;
        let ViewState::Ready(view) = &sf.view else { return None };
        if let review_core::file_view::FileView::Text { diff, .. } = view.as_ref()
            && sf.change.path().ends_with(".php")
        {
            let lines = if cursor.side == Side::Old { &diff.old.lines } else { &diff.new.lines };
            let functions = complexity::functions(&lines.join("\n"));
            if let Some(m) = functions
                .values()
                .filter(|m| m.line <= cursor.line && cursor.line < m.line + m.lines)
                .min_by_key(|m| m.lines)
            {
                return Some((cursor.side, m.line, m.line + m.lines - 1));
            }
        }
        Some((cursor.side, cursor.line, cursor.line))
    }

    pub(super) fn open_history_step(&mut self, i: usize) {
        let Some((_, Ok(result))) = &self.history_panel.lines else { return };
        let Some((step, _)) = result.steps.get(i).cloned() else { return };
        self.select_commit_by_id(step.commit, 0, Some(step.path.clone()), true);
        if let Some((start, _)) = step.lines
            && let Some(sc) = self.rs.as_mut().and_then(|rs| rs.commit.as_mut())
        {
            sc.pending_line = Some((Side::New, start));
        }
    }

    // ---- bisect ----

    pub(super) fn bisect_mark(&mut self, mark: BisectMark) {
        let Some(id) = self.rs.as_ref().and_then(|rs| rs.commit.as_ref()).filter(|c| c.base.is_none()).map(|c| c.info.id) else {
            return;
        };
        let b = &mut self.bisect;
        b.good.retain(|g| *g != id);
        b.skip.remove(&id);
        if b.bad == Some(id) {
            b.bad = None;
        }
        match mark {
            BisectMark::Good => b.good.push(id),
            BisectMark::Bad => b.bad = Some(id),
            BisectMark::Skip => {
                b.skip.insert(id);
            }
        }
        self.update_bisect();
    }

    pub(super) fn update_bisect(&mut self) {
        let Some(rs) = self.rs.as_ref() else { return };
        self.bisect_verdict = if self.bisect.is_active() {
            Some(self.bisect.verdict(&rs.repo).map_err(|e| format!("{e:#}")))
        } else {
            None
        };
        if let Some(Ok(Verdict::Found(id))) = &self.bisect_verdict {
            let short = id.to_hex_with_len(8).to_string();
            self.toast(format!("Bisect: {short} is the first bad commit"));
        }
    }

    pub(super) fn bisect_go(&mut self) {
        let target = match &self.bisect_verdict {
            Some(Ok(Verdict::Next { commit, .. } | Verdict::Found(commit))) => *commit,
            _ => return,
        };
        self.select_commit_by_id(target, 0, None, true);
    }

    pub(super) fn bisect_marks(&self) -> (HashSet<ObjectId>, Option<ObjectId>, HashSet<ObjectId>) {
        (self.bisect.good.iter().copied().collect(), self.bisect.bad, self.bisect.skip.clone())
    }

    // ---- Laravel views ----

    /// Compute schema, package and route changes for the selected diff, once.
    pub(super) fn ensure_laravel(&mut self) {
        if !(self.peek.open && self.peek.tab == PeekTab::Laravel) {
            return;
        }
        let Some(rs) = self.rs.as_ref() else { return };
        let Some(sc) = rs.commit.as_ref() else { return };
        let key = (sc.parent(), sc.info.id);
        if self.laravel.data.contains_key(&key) {
            return;
        }
        self.laravel.data.insert(key, Err(None));
        let repo = rs.repo.clone();
        self.pool.spawn(format!("Reading migrations, packages and routes at {}", sc.info.short_id()), move |_, send| {
            let result = (|| -> anyhow::Result<LaravelData> {
                let (old, new) = key;
                let new_schema = schema::schema_at(&repo, new)?;
                let old_schema = old.map(|o| schema::schema_at(&repo, o)).transpose()?.unwrap_or_default();
                let lock = |id: Option<ObjectId>| -> Option<String> {
                    let f = repo.find_file(id?, "composer.lock").ok()??;
                    Some(String::from_utf8_lossy(&repo.read_blob(f.id).ok()?).into_owned())
                };
                let (old_lock, new_lock) = (lock(old), lock(Some(new)));
                let packages = if old_lock != new_lock {
                    let parse = |t: Option<String>| t.map(|t| composer::parse_lock(&t)).transpose().map(Option::unwrap_or_default);
                    Some(composer::diff_lock(&parse(old_lock)?, &parse(new_lock)?))
                } else {
                    None
                };
                let new_routes = routes::routes_at(&repo, new)?;
                let old_routes = old.map(|o| routes::routes_at(&repo, o)).transpose()?.unwrap_or_default();
                Ok(LaravelData {
                    schema_changes: schema::diff(&old_schema, &new_schema),
                    schema: new_schema,
                    packages,
                    route_changes: routes::diff(&old_routes, &new_routes),
                    routes: new_routes,
                })
            })()
            .map_err(|e| format!("{e:#}"));
            send(JobResult::Laravel { key, result });
        });
    }

    /// `composer audit` on the commit's lock file. Contacts Packagist; runs no project code.
    pub(super) fn check_advisories(&mut self) {
        let Some(rs) = self.rs.as_ref() else { return };
        let Some(sc) = rs.commit.as_ref() else { return };
        let key = (sc.parent(), sc.info.id);
        let repo = rs.repo.clone();
        self.laravel.advisories.insert(key, Err(None));
        self.pool.spawn("Checking security advisories (Packagist)", move |ctx, send| {
            let result = (|| -> anyhow::Result<Vec<composer::Advisory>> {
                let dir = snapshot::default_cache_dir()?.join("audit").join(key.1.to_string());
                std::fs::create_dir_all(&dir)?;
                for name in ["composer.json", "composer.lock"] {
                    let f = repo.find_file(key.1, name)?.ok_or_else(|| anyhow::anyhow!("{name} is not in this commit"))?;
                    std::fs::write(dir.join(name), repo.read_blob(f.id)?)?;
                }
                composer::audit(&dir, &|| ctx.is_cancelled())
            })()
            .map_err(|e| format!("{e:#}"));
            send(JobResult::Advisories { key, result });
        });
    }

    /// Exact routes from `php artisan route:list` on both versions (trusted repositories).
    pub(super) fn artisan_routes(&mut self) {
        if !self.trusted() {
            self.toast("Mark this repository as trusted in Settings to run artisan.");
            return;
        }
        let Some(rs) = self.rs.as_ref() else { return };
        let Some(sc) = rs.commit.as_ref() else { return };
        let key = (sc.parent(), sc.info.id);
        let repo = rs.repo.clone();
        self.laravel.artisan.insert(key, Err(None));
        self.pool.spawn("Running artisan route:list", move |ctx, send| {
            let result = (|| -> anyhow::Result<Vec<routes::RouteChange>> {
                let cache = snapshot::default_cache_dir()?;
                let list = |id: ObjectId| -> anyhow::Result<Vec<routes::RouteDef>> {
                    let snap = snapshot::ensure(&repo, id, &cache, &|| ctx.is_cancelled())?;
                    routes::artisan_routes(&snap.dir, &|| ctx.is_cancelled())
                };
                let new = list(key.1)?;
                let old = key.0.map(list).transpose()?.unwrap_or_default();
                Ok(routes::diff(&old, &new))
            })()
            .map_err(|e| format!("{e:#}"));
            send(JobResult::ArtisanRoutes { key, result });
        });
    }

    /// Jump to the controller method of a route in the current Laravel data.
    pub(super) fn open_route_action(&mut self, i: usize) {
        let Some(sc) = self.rs.as_ref().and_then(|rs| rs.commit.as_ref()) else { return };
        let key = (sc.parent(), sc.info.id);
        let Some(Ok(data)) = self.laravel.data.get(&key) else { return };
        let Some(route) = data.routes.get(i) else { return };
        let routes::RouteAction::Controller { class, method } = route.action.clone() else {
            let (file, line) = (route.file.clone(), route.line);
            if !file.is_empty() {
                self.open_location(key.1, &file, line);
            }
            return;
        };
        self.request_index(key.1);
        let Some(index) = self.indexes.get(&key.1) else {
            self.toast("Indexing symbols… try again in a moment.");
            return;
        };
        let short = class.rsplit('\\').next().unwrap_or(&class).to_string();
        let path = format!("{}.php", short);
        let target = index
            .files()
            .find(|(p, f)| p.ends_with(&path) && f.symbols.iter().any(|s| s.fqn.eq_ignore_ascii_case(&format!("{class}::{method}"))))
            .and_then(|(p, f)| f.symbols.iter().find(|s| s.name.eq_ignore_ascii_case(&method)).map(|s| (p.clone(), s.span.line)));
        match target {
            Some((p, line)) => self.open_location(key.1, &p, line),
            None => self.toast(format!("{class}::{method} is not in this commit's code")),
        }
    }

    pub(super) fn open_tests_for(&mut self, class: &str) {
        let Some(rs) = self.rs.as_ref() else { return };
        let Some(sc) = rs.commit.as_ref() else { return };
        let commit = sc.info.id;
        let tree: Vec<String> = rs.repo.list_files(commit).map(|f| f.into_iter().map(|f| f.path).collect()).unwrap_or_default();
        match routes::tests_for_class(class, tree.iter().map(String::as_str)).first() {
            Some(t) => {
                let t = t.clone();
                self.open_tree_file(&t);
            }
            None => self.toast(format!("No tests named after {}", class.rsplit('\\').next().unwrap_or(class))),
        }
    }
}
