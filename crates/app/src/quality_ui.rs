//! Quality signals and precise navigation: the Phase 4 parts of the app.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use review_core::ObjectId;
use review_core::analysis::coverage::{self, Coverage};
use review_core::analysis::{self, complexity, secrets, snapshot};
use review_core::diff::DiffOptions;
use review_core::file_view::{self, FileView};
use review_core::lsp::{self, LspClient, LspLocation};
use review_core::settings::Store;

use super::ReviewApp;
use crate::state::{
    AnalysisKey, AnalysisState, DefinitionState, FileQuality, JobResult, LspTarget, PeekTab, ReferencesState, Side,
    Snippet, ViewState,
};
use crate::ui::quality::QualityView;

/// Language servers kept running (one per commit snapshot).
const MAX_SERVERS: usize = 2;

impl ReviewApp {
    pub(super) fn trusted(&self) -> bool {
        self.rs.as_ref().is_some_and(|rs| rs.settings.trusted)
    }

    pub(super) fn use_lsp(&self) -> bool {
        self.rs.as_ref().is_some_and(|rs| rs.settings.trusted && rs.settings.use_lsp && !rs.settings.lsp_cmd.trim().is_empty())
    }

    fn analysis_key(&self) -> Option<AnalysisKey> {
        let sc = self.rs.as_ref()?.commit.as_ref()?;
        Some((sc.parent(), sc.info.id))
    }

    // ---- coverage ----

    pub(super) fn load_coverage(&mut self) {
        self.coverage = None;
        self.coverage_note = None;
        let Some(rs) = self.rs.as_ref() else { return };
        let configured = rs.settings.coverage_path.trim();
        if configured.is_empty() {
            return;
        }
        let path = {
            let p = PathBuf::from(configured);
            if p.is_absolute() { p } else { rs.repo.root().join(p) }
        };
        if !path.exists() {
            self.coverage_note = Some(format!("No coverage report at {} (set it in Settings).", path.display()));
            return;
        }
        match Coverage::load(&path) {
            Ok(c) => {
                let modified = std::fs::metadata(&path).and_then(|m| m.modified()).ok();
                let age = modified.and_then(|m| m.elapsed().ok()).map(|d| d.as_secs() / 3600);
                self.coverage_note = Some(format!(
                    "{} files from {}{}",
                    c.file_count(),
                    path.display(),
                    age.map(|h| format!(", {h} h old")).unwrap_or_default()
                ));
                self.coverage = Some(Arc::new(c));
            }
            Err(e) => self.coverage_note = Some(format!("Could not read coverage: {e:#}")),
        }
        if let Some(sf) = self.current_file_mut() {
            sf.quality = None;
        }
    }

    // ---- per-file signals ----

    /// Complexity, secrets and coverage for the open file, computed once its diff is ready.
    pub(super) fn ensure_file_quality(&mut self) {
        let coverage = self.coverage.clone();
        let Some(sf) = self.current_file_mut() else { return };
        if sf.quality.is_some() || sf.browse.is_some() {
            return;
        }
        let ViewState::Ready(view) = &sf.view else { return };
        let FileView::Text { diff, .. } = view.as_ref() else { return };
        let path = sf.change.path().to_string();
        let complexity = if path.ends_with(".php") {
            complexity::compare(&diff.old.lines.join("\n"), &diff.new.lines.join("\n"))
        } else {
            Vec::new()
        };
        let uncovered = coverage.as_ref().and_then(|c| c.for_file(&path)).map(|fc| coverage::uncovered_added(diff, fc));
        sf.quality = Some(FileQuality { complexity, secrets: secrets::scan_diff(diff), uncovered });
    }

    /// Scan every changed file of the selected commit for secrets, in the background.
    pub(super) fn ensure_secret_scan(&mut self) {
        let Some(key) = self.analysis_key() else { return };
        if self.secret_scans.contains_key(&key) || self.secret_pending.contains(&key) {
            return;
        }
        let Some(rs) = self.rs.as_ref() else { return };
        let Some(sc) = rs.commit.as_ref() else { return };
        let crate::state::FilesState::Ready(files) = &sc.files else { return };
        let files = files.clone();
        let repo = rs.repo.clone();
        self.secret_pending.insert(key);
        self.pool.spawn(format!("Scanning {} for secrets", sc.info.short_id()), move |ctx, send| {
            let mut findings = Vec::new();
            for f in &files {
                if ctx.is_cancelled() {
                    return;
                }
                if let Ok(FileView::Text { diff, .. }) = file_view::load(&repo, f, DiffOptions::default(), false) {
                    findings.extend(secrets::scan_diff(&diff).into_iter().map(|s| (f.path().to_string(), s)));
                }
            }
            send(JobResult::Secrets { key, findings });
        });
    }

    // ---- PHPStan / Pint ----

    pub(super) fn run_analysis(&mut self) {
        if !self.trusted() {
            self.toast("Mark this repository as trusted in Settings to run its tools.");
            return;
        }
        let Some(key) = self.analysis_key() else { return };
        if matches!(self.analyses.get(&key), Some(AnalysisState::Running)) {
            return;
        }
        let Some(rs) = self.rs.as_ref() else { return };
        let Some(sc) = rs.commit.as_ref() else { return };
        let files = sc.files().to_vec();
        let repo = rs.repo.clone();
        let commands = Store::tool_commands(&rs.settings);
        let cache = match snapshot::default_cache_dir() {
            Ok(c) => c,
            Err(e) => {
                self.error = Some(format!("{e:#}"));
                return;
            }
        };
        self.analyses.insert(key, AnalysisState::Running);
        self.pool.spawn(format!("Analysing {}", sc.info.short_id()), move |ctx, send| {
            let result = analysis::analyse_change(&repo, &cache, key.0, key.1, &files, &commands, &|p| ctx.set_progress(p), &|| ctx.is_cancelled())
                .map(Arc::new)
                .map_err(|e| format!("{e:#}"));
            send(JobResult::Analysis { key, result });
        });
    }

    /// New warnings and possible secrets on the open file, for the gutter.
    pub(super) fn file_warnings(&self) -> Vec<(Side, u32, String)> {
        let Some(sf) = self.rs.as_ref().and_then(|rs| rs.commit.as_ref()).and_then(|c| c.file.as_ref()) else {
            return Vec::new();
        };
        if sf.browse.is_some() {
            return Vec::new();
        }
        let path = sf.change.path();
        let mut out: Vec<(Side, u32, String)> = sf
            .quality
            .iter()
            .flat_map(|q| &q.secrets)
            .map(|s| (Side::New, s.line, format!("Possible secret: {} ({})", s.kind, s.masked)))
            .collect();
        if let Some(key) = self.analysis_key()
            && let Some(AnalysisState::Done(result)) = self.analyses.get(&key)
        {
            out.extend(
                result
                    .introduced
                    .iter()
                    .filter(|w| w.path == path)
                    .filter_map(|w| Some((Side::New, w.line?, format!("{}: {}", w.tool.label(), w.message)))),
            );
        }
        out
    }

    pub(super) fn quality_view(&self) -> QualityView {
        let key = self.analysis_key();
        let sf = self.rs.as_ref().and_then(|rs| rs.commit.as_ref()).and_then(|c| c.file.as_ref());
        QualityView {
            trusted: self.trusted(),
            analysis: key.and_then(|k| match self.analyses.get(&k) {
                Some(AnalysisState::Running) => Some(Err(None)),
                Some(AnalysisState::Failed(e)) => Some(Err(Some(e.clone()))),
                Some(AnalysisState::Done(r)) => Some(Ok(r.clone())),
                None => None,
            }),
            secrets: key.and_then(|k| self.secret_scans.get(&k).cloned()),
            file: sf.map(|f| f.change.path().to_string()),
            quality: sf.and_then(|f| f.quality.clone()),
            coverage_note: self.coverage_note.clone(),
            coverage_on: self.coverage_on,
        }
    }

    /// Show `line` (new side) of a changed file in the selected commit.
    pub(super) fn go_to_changed_line(&mut self, path: &str, line: u32) {
        let Some(commit) = self.rs.as_ref().and_then(|rs| rs.commit.as_ref()).map(|c| c.info.id) else { return };
        self.open_location(commit, path, line);
    }

    // ---- language server ----

    /// Map a server location to the repository (when it is in the snapshot's tree).
    fn lsp_target(dir: &Path, loc: LspLocation) -> LspTarget {
        let canonical = dir.canonicalize().unwrap_or_else(|_| dir.to_path_buf());
        let rel = loc
            .path
            .strip_prefix(dir)
            .or_else(|_| loc.path.strip_prefix(&canonical))
            .ok()
            .map(|p| p.to_string_lossy().replace('\\', "/"))
            .filter(|p| !p.starts_with("vendor/") && !p.starts_with("node_modules/"));
        let context = std::fs::read_to_string(&loc.path)
            .ok()
            .and_then(|t| t.lines().nth(loc.line as usize).map(|l| l.trim().chars().take(140).collect()))
            .unwrap_or_default();
        LspTarget { rel, abs: loc.path, line: loc.line, context }
    }

    /// Ask the language server (in a snapshot of `commit`) for definitions or references.
    pub(super) fn lsp_request(&mut self, commit: ObjectId, path: String, line: u32, col: u32, references: bool) -> u64 {
        self.lsp_counter += 1;
        let request = self.lsp_counter;
        let Some(rs) = self.rs.as_ref() else { return request };
        let repo = rs.repo.clone();
        let cmd: Vec<String> = rs.settings.lsp_cmd.split_whitespace().map(String::from).collect();
        let servers = self.lsp_servers.clone();
        let label = format!("Asking {} ({})", cmd.first().cloned().unwrap_or_default(), if references { "references" } else { "definition" });
        self.pool.spawn(label, move |ctx, send| {
            let result = (|| -> anyhow::Result<Vec<LspTarget>> {
                let cache = snapshot::default_cache_dir()?;
                let snap = snapshot::ensure(&repo, commit, &cache, &|| ctx.is_cancelled())?;
                let mut servers = servers.lock().unwrap();
                if !servers.contains_key(&commit) {
                    while servers.len() >= MAX_SERVERS {
                        let Some(old) = servers.keys().next().copied() else { break };
                        if let Some(c) = servers.remove(&old) {
                            c.shutdown();
                        }
                    }
                    let _ = lsp::generate_ide_helper(&snap.dir);
                    servers.insert(commit, LspClient::start(&cmd, &snap.dir)?);
                }
                let client = servers.get_mut(&commit).expect("inserted above");
                let found = if references { client.references(&path, line, col) } else { client.definition(&path, line, col) };
                let found = match found {
                    Ok(f) => f,
                    Err(e) => {
                        // A dead server is restarted on the next request.
                        servers.remove(&commit);
                        return Err(e);
                    }
                };
                Ok(found.into_iter().map(|l| Self::lsp_target(&snap.dir, l)).collect())
            })()
            .map_err(|e| format!("{e:#}"));
            send(if references {
                JobResult::LspReferences { request, result }
            } else {
                JobResult::LspDefinition { request, result }
            });
        });
        request
    }

    pub(super) fn on_lsp_definition(&mut self, request: u64, result: Result<Vec<LspTarget>, String>) {
        let Some(def) = &self.peek.definition else { return };
        let DefinitionState::LspPending { request: r, path, line, col } = &def.state else { return };
        if *r != request {
            return;
        }
        let (path, line, col, commit) = (path.clone(), *line, *col, def.version.commit);
        match result {
            Ok(targets) if !targets.is_empty() => {
                let name = word_at(&self.view_text_line(&path, line), col as usize);
                let snippet = self.lsp_snippet(&targets[0]);
                if let Some(def) = self.peek.definition.as_mut() {
                    def.state = DefinitionState::Lsp { name, targets, selected: 0 };
                    def.snippet = snippet;
                }
            }
            other => {
                // Fall back to the tree-sitter index.
                if let Err(e) = &other {
                    self.toast(format!("Language server: {e}. Using the symbol index."));
                }
                if let Some(def) = self.peek.definition.as_mut() {
                    def.state = DefinitionState::Pending { path, line, col };
                }
                self.request_index(commit);
                self.resolve_pending_lookups();
            }
        }
    }

    pub(super) fn on_lsp_references(&mut self, request: u64, result: Result<Vec<LspTarget>, String>) {
        let Some(refs) = &self.peek.references else { return };
        let ReferencesState::LspPending { request: r } = &refs.state else { return };
        if *r != request {
            return;
        }
        let ((path, line, col), commit) = (refs.origin.clone(), refs.version.commit);
        match result {
            Ok(targets) => {
                let name = word_at(&self.view_text_line(&path, line), col as usize);
                if let Some(refs) = self.peek.references.as_mut() {
                    refs.state = ReferencesState::Lsp { name, targets };
                }
            }
            Err(e) => {
                self.toast(format!("Language server: {e}. Using the symbol index."));
                if let Some(refs) = self.peek.references.as_mut() {
                    refs.state = ReferencesState::Pending;
                }
                self.request_index(commit);
                self.resolve_pending_lookups();
            }
        }
    }

    /// A line of the open file on whichever side shows `path`.
    fn view_text_line(&self, path: &str, line: u32) -> String {
        let Some(sf) = self.rs.as_ref().and_then(|rs| rs.commit.as_ref()).and_then(|c| c.file.as_ref()) else {
            return String::new();
        };
        let ViewState::Ready(view) = &sf.view else { return String::new() };
        let FileView::Text { diff, .. } = view.as_ref() else { return String::new() };
        let lines = if sf.change.new.as_ref().is_some_and(|s| s.path == path) { &diff.new.lines } else { &diff.old.lines };
        lines.get(line as usize).cloned().unwrap_or_default()
    }

    /// Source around a language-server target, for the peek panel.
    pub(super) fn lsp_snippet(&self, target: &LspTarget) -> Option<Snippet> {
        let text = std::fs::read_to_string(&target.abs).ok()?;
        let first = target.line.saturating_sub(2);
        // The server only says where the definition starts; find its end by parsing the file,
        // else show the rest of it.
        let last = review_core::symbols::parse::parse_php(&text)
            .symbols
            .iter()
            .filter(|s| s.first_line <= target.line && target.line <= s.last_line)
            .min_by_key(|s| s.last_line - s.first_line)
            .map(|s| s.last_line as usize);
        let lines: Vec<String> = text
            .lines()
            .enumerate()
            .skip(first as usize)
            .take_while(|(i, _)| last.is_none_or(|l| *i <= l))
            .map(|(_, l)| l.trim_end_matches('\r').to_string())
            .collect();
        let path = target.abs.to_string_lossy().into_owned();
        let mut with_tag = vec!["<?php".to_string()];
        with_tag.extend(lines.iter().cloned());
        let highlighted = self.highlighter.highlight(&path, &with_tag, self.theme(), &|| false).map(|mut h| {
            h.remove(0);
            h
        });
        Some(Snippet { key: (path, target.line), first_line: first, lines, highlighted })
    }

    pub(super) fn open_lsp_target(&mut self, commit: ObjectId, target: &LspTarget) {
        match &target.rel {
            Some(rel) => self.open_location(commit, rel, target.line),
            None => self.toast(format!("{} is outside the repository (shown in the peek panel only).", target.abs.display())),
        }
    }

    pub(super) fn shutdown_language_servers(&mut self) {
        if let Ok(mut servers) = self.lsp_servers.lock() {
            for (_, c) in servers.drain() {
                c.shutdown();
            }
        }
    }

    pub(super) fn show_quality(&mut self) {
        self.peek.open = true;
        self.peek.tab = PeekTab::Quality;
    }
}

/// The identifier around a byte column.
fn word_at(line: &str, col: usize) -> String {
    let is_word = |c: char| c.is_alphanumeric() || c == '_' || c == '$';
    let col = col.min(line.len());
    let start = line[..col].rfind(|c: char| !is_word(c)).map_or(0, |i| i + 1);
    let end = line[col..].find(|c: char| !is_word(c)).map_or(line.len(), |i| col + i);
    line.get(start..end).unwrap_or("").to_string()
}
