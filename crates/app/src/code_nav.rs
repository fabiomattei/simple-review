//! Go to definition, find references, whole-file browsing and blame: the Phase 2 parts of the app.

use std::sync::Arc;

use review_core::ObjectId;
use review_core::blame::{self, Blame};
use review_core::symbols::{SymbolIndex, is_php_path};

use super::{ReviewApp, request_view};
use crate::nav::Location;
use crate::state::{
    BlameKey, Browse, DefinitionPeek, DefinitionState, JobResult, PeekTab, ReferencesPeek, ReferencesState, SelectedFile,
    Side, Snippet, Version, ViewState,
};


impl ReviewApp {
    /// The version (commit) and path shown on `side` of the current file.
    pub(super) fn version_of(&self, side: Side) -> Option<(Version, String)> {
        let sc = self.rs.as_ref()?.commit.as_ref()?;
        let sf = sc.file.as_ref()?;
        if let Some(b) = &sf.browse {
            return Some((Version { commit: b.commit, label: b.label.clone() }, sf.change.path().to_string()));
        }
        match side {
            Side::Old => {
                let parent = sc.parent()?;
                let path = sf.change.old.as_ref()?.path.clone();
                Some((Version { commit: parent, label: format!("parent {}", parent.to_hex_with_len(8)) }, path))
            }
            Side::New => {
                let path = sf.change.new.as_ref()?.path.clone();
                Some((Version { commit: sc.info.id, label: format!("commit {}", sc.info.short_id()) }, path))
            }
        }
    }

    /// Build (or reuse) the symbol index for `commit` in the background.
    pub(super) fn request_index(&mut self, commit: ObjectId) {
        if self.indexes.get(&commit).is_some() || self.index_jobs.contains_key(&commit) {
            return;
        }
        let Some(rs) = self.rs.as_ref() else { return };
        let repo = rs.repo.clone();
        let settings = rs.settings.clone();
        let cache = self.parse_cache.clone();
        let label = format!("Indexing symbols at {}", commit.to_hex_with_len(8));
        let token = self.pool.spawn(label, move |ctx, send| {
            let include = |path: &str| !settings.is_ignored(path);
            let result = SymbolIndex::build(&repo, commit, &include, &cache, &|p| ctx.set_progress(p), &|| ctx.is_cancelled())
                .map(|o| o.map(Arc::new))
                .map_err(|e| format!("{e:#}"));
            send(JobResult::Index { commit, result });
        });
        self.index_jobs.insert(commit, token);
    }

    /// Index both sides of the selected commit ahead of the first lookup, cancelling stale builds.
    pub(super) fn prefetch_indexes(&mut self) {
        let Some(sc) = self.rs.as_ref().and_then(|rs| rs.commit.as_ref()) else { return };
        let wanted: Vec<ObjectId> = std::iter::once(sc.info.id).chain(sc.parent()).collect();
        self.index_jobs.retain(|commit, token| {
            let keep = wanted.contains(commit);
            if !keep {
                token.cancel();
            }
            keep
        });
        for commit in wanted {
            self.request_index(commit);
        }
    }

    pub(super) fn on_index(&mut self, commit: ObjectId, result: Result<Option<Arc<SymbolIndex>>, String>) {
        self.index_jobs.remove(&commit);
        match result {
            Ok(Some(index)) => {
                self.indexes.insert(commit, index);
                self.resolve_pending_lookups();
            }
            Ok(None) => {} // cancelled
            Err(e) => self.error = Some(format!("Indexing failed: {e}")),
        }
    }

    pub(super) fn go_to_definition(&mut self) {
        let Some(cursor) = self.current_cursor() else { return };
        let Some((version, path)) = self.version_of(cursor.side) else { return };
        self.peek.open = true;
        self.peek.tab = PeekTab::Definition;
        if !is_php_path(&path) {
            self.peek.definition = Some(DefinitionPeek {
                version,
                state: DefinitionState::NotFound("Go to definition works in PHP files.".into()),
                snippet: None,
            });
            return;
        }
        let commit = version.commit;
        if self.use_lsp() {
            let request = self.lsp_request(commit, path.clone(), cursor.line, cursor.col, false);
            self.peek.definition = Some(DefinitionPeek {
                version,
                state: DefinitionState::LspPending { request, path, line: cursor.line, col: cursor.col },
                snippet: None,
            });
            return;
        }
        self.peek.definition = Some(DefinitionPeek {
            version,
            state: DefinitionState::Pending { path, line: cursor.line, col: cursor.col },
            snippet: None,
        });
        self.request_index(commit);
        self.resolve_pending_lookups();
    }

    pub(super) fn find_references(&mut self) {
        let Some(cursor) = self.current_cursor() else { return };
        let Some((version, path)) = self.version_of(cursor.side) else { return };
        self.peek.open = true;
        self.peek.tab = PeekTab::References;
        let commit = version.commit;
        if self.use_lsp() && is_php_path(&path) {
            let request = self.lsp_request(commit, path.clone(), cursor.line, cursor.col, true);
            self.peek.references = Some(ReferencesPeek {
                version,
                origin: (path, cursor.line, cursor.col),
                state: ReferencesState::LspPending { request },
            });
            return;
        }
        self.peek.references =
            Some(ReferencesPeek { version, origin: (path, cursor.line, cursor.col), state: ReferencesState::Pending });
        self.request_index(commit);
        self.resolve_pending_lookups();
    }

    fn current_cursor(&self) -> Option<crate::state::Cursor> {
        self.rs.as_ref()?.commit.as_ref()?.file.as_ref()?.cursor
    }

    /// Finish definition/reference lookups whose index has arrived.
    pub(super) fn resolve_pending_lookups(&mut self) {
        if let Some(def) = &self.peek.definition
            && let DefinitionState::Pending { path, line, col } = &def.state
            && let Some(index) = self.indexes.get(&def.version.commit)
        {
            let state = match index.definition_at(path, *line, *col) {
                Some(lookup) if !lookup.candidates.is_empty() => DefinitionState::Ready { lookup, selected: 0 },
                Some(lookup) => {
                    DefinitionState::NotFound(format!("No definition of `{}` found at {}.", lookup.name, def.version.label))
                }
                None => DefinitionState::NotFound("No class, function, method or property name at the cursor.".into()),
            };
            if let Some(def) = self.peek.definition.as_mut() {
                def.state = state;
            }
            self.load_snippet();
        }
        if let Some(refs) = &self.peek.references
            && matches!(refs.state, ReferencesState::Pending)
            && let Some(index) = self.indexes.get(&refs.version.commit)
        {
            let (path, line, col) = &refs.origin;
            let state = match index.definition_at(path, *line, *col).and_then(|l| l.candidates.into_iter().next()) {
                Some(target) => {
                    let hits = index.references(&target);
                    ReferencesState::Ready { target, hits }
                }
                None => ReferencesState::NotFound(format!("Nothing to find references for at {}.", refs.version.label)),
            };
            if let Some(refs) = self.peek.references.as_mut() {
                refs.state = state;
            }
        }
    }

    /// Read the selected definition's source for the peek panel.
    pub(super) fn load_snippet(&mut self) {
        if let Some(def) = &self.peek.definition
            && let DefinitionState::Lsp { targets, selected, .. } = &def.state
        {
            let snippet = targets.get(*selected).and_then(|t| self.lsp_snippet(t));
            if let Some(def) = self.peek.definition.as_mut() {
                def.snippet = snippet;
            }
            return;
        }
        let theme = self.theme();
        let Some(rs) = self.rs.as_ref() else { return };
        let Some(def) = self.peek.definition.as_mut() else { return };
        let DefinitionState::Ready { lookup, selected } = &def.state else {
            def.snippet = None;
            return;
        };
        let Some(c) = lookup.candidates.get(*selected) else { return };
        let key = (c.path.clone(), c.symbol.first_line);
        if def.snippet.as_ref().is_some_and(|s| s.key == key) {
            return;
        }
        let Ok(Some(file)) = rs.repo.find_file(def.version.commit, &c.path) else { return };
        let Ok(bytes) = rs.repo.read_blob(file.id) else { return };
        let text = String::from_utf8_lossy(&bytes);
        let first = c.symbol.first_line;
        let last = c.symbol.last_line.max(first);
        let lines: Vec<String> = text
            .lines()
            .skip(first as usize)
            .take((last - first + 1) as usize)
            .map(|l| l.trim_end_matches('\r').to_string())
            .collect();
        // Highlight from an opening tag so the PHP grammar is active for a mid-file excerpt.
        let mut with_tag = vec!["<?php".to_string()];
        with_tag.extend(lines.iter().cloned());
        let highlighted = self.highlighter.highlight(&c.path, &with_tag, theme, &|| false).map(|mut h| {
            h.remove(0);
            h
        });
        def.snippet = Some(Snippet { key, first_line: first, lines, highlighted });
    }

    pub(super) fn open_definition(&mut self) {
        let Some(def) = &self.peek.definition else { return };
        if let DefinitionState::Lsp { targets, selected, .. } = &def.state {
            let (commit, target) = (def.version.commit, targets.get(*selected).cloned());
            if let Some(t) = target {
                self.open_lsp_target(commit, &t);
            }
            return;
        }
        let DefinitionState::Ready { lookup, selected } = &def.state else { return };
        let Some(c) = lookup.candidates.get(*selected) else { return };
        let (commit, path, line) = (def.version.commit, c.path.clone(), c.symbol.span.line);
        self.open_location(commit, &path, line);
    }

    pub(super) fn open_reference(&mut self, i: usize) {
        let Some(refs) = &self.peek.references else { return };
        if let ReferencesState::Lsp { targets, .. } = &refs.state {
            let (commit, target) = (refs.version.commit, targets.get(i).cloned());
            if let Some(t) = target {
                self.open_lsp_target(commit, &t);
            }
            return;
        }
        let ReferencesState::Ready { hits, .. } = &refs.state else { return };
        let Some(hit) = hits.get(i) else { return };
        let (commit, path, line) = (refs.version.commit, hit.path.clone(), hit.reference.span.line);
        self.open_location(commit, &path, line);
    }

    /// Show `line` of `path` at `commit`: in the diff when the file changed in the selected commit
    /// on that side, otherwise as a whole file.
    pub(super) fn open_location(&mut self, commit: ObjectId, path: &str, line: u32) {
        let Some(sc) = self.rs.as_ref().and_then(|rs| rs.commit.as_ref()) else { return };
        let side_match = if commit == sc.info.id {
            sc.files().iter().position(|f| f.new.as_ref().is_some_and(|s| s.path == path)).map(|i| (i, Side::New))
        } else if Some(commit) == sc.parent() {
            sc.files().iter().position(|f| f.old.as_ref().is_some_and(|s| s.path == path)).map(|i| (i, Side::Old))
        } else {
            None
        };
        let current = sc.file.as_ref();
        match side_match {
            Some((i, side)) => {
                if current.is_none_or(|f| f.browse.is_some() || f.index != Some(i)) {
                    self.select_file(i, crate::state::HunkTarget::First, true);
                }
                self.reveal_line(side, line);
            }
            None => {
                let open = current.is_some_and(|f| f.browse.as_ref().is_some_and(|b| b.commit == commit) && f.change.path() == path);
                if !open {
                    self.open_browse(commit, path, true);
                }
                self.reveal_line(Side::New, line);
            }
        }
    }

    fn reveal_line(&mut self, side: Side, line: u32) {
        let unified = self.unified;
        if let Some(sf) = self.current_file_mut() {
            sf.pending_line = Some((side, line));
            sf.apply_pending_line(unified);
        }
    }

    /// Open `path` as a whole file at `commit` (the selected commit or its parent).
    pub(super) fn open_browse(&mut self, commit: ObjectId, path: &str, push: bool) {
        let Some(rs) = self.rs.as_mut() else { return };
        let Some(sc) = rs.commit.as_mut() else { return };
        let file = match rs.repo.find_file(commit, path) {
            Ok(Some(f)) => f,
            Ok(None) => {
                self.error = Some(format!("{path} does not exist at {}", commit.to_hex_with_len(8)));
                return;
            }
            Err(e) => {
                self.error = Some(format!("{e:#}"));
                return;
            }
        };
        let label = if commit == sc.info.id {
            format!("commit {}", sc.info.short_id())
        } else if Some(commit) == sc.parent() {
            format!("parent {}", commit.to_hex_with_len(8))
        } else {
            commit.to_hex_with_len(8).to_string()
        };
        if let Some(token) = sc.file.as_ref().and_then(|f| f.token.as_ref()) {
            token.cancel();
        }
        let mut sf = SelectedFile::browse(file, Browse { commit, label });
        request_view(&self.pool, &rs.repo, &mut sf, self.opts, &mut self.view_cache);
        if push {
            rs.history.push(Location {
                commit: sc.info.id,
                parent_idx: sc.parent_idx,
                path: Some(path.to_string()),
                browse: Some(commit),
                base: sc.base,
            });
        }
        sc.file = Some(sf);
    }

    pub(super) fn open_tree_file(&mut self, path: &str) {
        let Some(sc) = self.rs.as_ref().and_then(|rs| rs.commit.as_ref()) else { return };
        match sc.files().iter().position(|f| f.path() == path) {
            Some(i) => self.select_file(i, crate::state::HunkTarget::First, true),
            None => {
                let commit = sc.info.id;
                self.open_browse(commit, path, true);
            }
        }
    }

    /// Load the full file list of the selected commit for "All files" mode.
    pub(super) fn request_tree(&mut self) {
        let Some(rs) = self.rs.as_ref() else { return };
        let Some(sc) = rs.commit.as_ref() else { return };
        if sc.tree.is_some() {
            return;
        }
        let repo = rs.repo.clone();
        let commit = sc.info.id;
        self.pool.spawn(format!("Listing all files at {}", sc.info.short_id()), move |_, send| {
            send(JobResult::Tree { commit, result: repo.list_files(commit).map_err(|e| format!("{e:#}")) });
        });
    }

    /// Blame keys needed for the current file: one per visible side.
    pub(super) fn blame_keys(&self) -> (Option<BlameKey>, Option<BlameKey>) {
        let key = |side| self.version_of(side).map(|(v, path)| (v.commit, path));
        let Some(sf) = self.rs.as_ref().and_then(|rs| rs.commit.as_ref()).and_then(|c| c.file.as_ref()) else {
            return (None, None);
        };
        if sf.browse.is_some() {
            return (None, key(Side::New));
        }
        (key(Side::Old), key(Side::New))
    }

    /// Start `git blame` for the current file's sides when the gutter is on.
    pub(super) fn ensure_blame(&mut self) {
        if !self.blame_on {
            return;
        }
        let ready = self
            .rs
            .as_ref()
            .and_then(|rs| rs.commit.as_ref())
            .and_then(|c| c.file.as_ref())
            .is_some_and(|f| matches!(f.view, ViewState::Ready(_)));
        if !ready {
            return;
        }
        let (old, new) = self.blame_keys();
        for key in [old, new].into_iter().flatten() {
            if self.blame_pending.contains(&key) || self.blames.get(&key).is_some() {
                continue;
            }
            let Some(rs) = self.rs.as_ref() else { return };
            self.blame_pending.insert(key.clone());
            let repo = rs.repo.clone();
            let ignore = self.ignore_revs.clone();
            let label = format!("Blaming {} at {}", key.1, key.0.to_hex_with_len(8));
            self.pool.spawn(label, move |_, send| {
                let result = blame::blame_file(&repo, key.0, &key.1, &ignore).map_err(|e| format!("{e:#}"));
                send(JobResult::Blame { key, result: Arc::new(result) });
            });
        }
    }

    pub(super) fn blame_for(&mut self, key: &Option<BlameKey>) -> Option<Arc<Result<Blame, String>>> {
        key.as_ref().and_then(|k| self.blames.get(k))
    }
}
