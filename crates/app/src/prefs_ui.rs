//! Appearance preferences and bookmarks: the Phase 6 parts of the app.

use egui::{FontId, TextStyle, ThemePreference};
use review_core::git::parse_id;
use review_core::review::ThreadSide;
use review_core::review::bookmarks::NewBookmark;
use review_core::settings::GLOBAL;

use super::ReviewApp;
use crate::state::{JobResult, PeekTab, Prefs, PrefsDialog, Side};

pub const DEFAULT_CODE_FONT: f32 = 13.0;
pub const DEFAULT_PEEK_HEIGHT: f32 = 260.0;

impl Prefs {
    pub fn load(store: Option<&review_core::settings::Store>) -> Self {
        let get = |k: &str| store.and_then(|s| s.get(GLOBAL, k).ok().flatten());
        Prefs {
            theme: match get("theme").as_deref() {
                Some("light") => ThemePreference::Light,
                Some("dark") => ThemePreference::Dark,
                _ => ThemePreference::System,
            },
            zoom: get("zoom").and_then(|v| v.parse().ok()).unwrap_or(1.0),
            code_font: get("code_font").and_then(|v| v.parse().ok()).unwrap_or(DEFAULT_CODE_FONT),
            peek_height: get("peek_height").and_then(|v| v.parse().ok()).unwrap_or(DEFAULT_PEEK_HEIGHT),
        }
    }

    pub fn save(&self, store: &review_core::settings::Store) {
        let theme = match self.theme {
            ThemePreference::Light => "light",
            ThemePreference::Dark => "dark",
            ThemePreference::System => "system",
        };
        let _ = store.set(GLOBAL, "theme", theme);
        let _ = store.set(GLOBAL, "zoom", &format!("{:.2}", self.zoom));
        let _ = store.set(GLOBAL, "code_font", &format!("{:.1}", self.code_font));
        let _ = store.set(GLOBAL, "peek_height", &format!("{:.0}", self.peek_height));
    }
}

impl ReviewApp {
    /// Apply theme, zoom and code font size to the egui context.
    pub(super) fn apply_prefs(&self) {
        self.ctx.set_theme(self.prefs.theme);
        self.ctx.set_zoom_factor(self.prefs.zoom.clamp(0.5, 3.0));
        let size = self.prefs.code_font.clamp(8.0, 32.0);
        self.ctx.all_styles_mut(|style| {
            style.text_styles.insert(TextStyle::Monospace, FontId::monospace(size));
        });
    }

    /// Keep the zoom set with Cmd/Ctrl +/− (handled by egui) across restarts.
    pub(super) fn track_zoom(&mut self) {
        let zoom = self.ctx.zoom_factor();
        if (zoom - self.prefs.zoom).abs() > 0.001 {
            self.prefs.zoom = zoom;
            if let Some(store) = &self.store {
                self.prefs.save(store);
            }
        }
    }

    pub(super) fn open_prefs(&mut self) {
        self.prefs_dialog = Some(PrefsDialog {
            prefs: self.prefs.clone(),
            check_updates: self.check_updates,
            unified: self.unified,
            blame_on: self.blame_on,
            coverage_on: self.coverage_on,
        });
    }

    pub(super) fn save_prefs(&mut self) {
        let Some(d) = self.prefs_dialog.take() else { return };
        self.prefs = d.prefs;
        self.check_updates = d.check_updates;
        self.save_pref("check_updates", self.check_updates);
        self.unified = d.unified;
        self.blame_on = d.blame_on;
        self.coverage_on = d.coverage_on;
        if let Some(store) = &self.store {
            self.prefs.save(store);
        }
        self.save_pref("unified", self.unified);
        self.save_pref("blame", self.blame_on);
        self.save_pref("coverage", self.coverage_on);
        self.apply_prefs();
        let unified = self.unified;
        if let Some(sf) = self.current_file_mut() {
            sf.reveal_current_hunk(unified);
        }
    }

    // ---- bookmarks ----

    pub(super) fn load_bookmarks(&mut self) {
        let Some(rs) = self.rs.as_mut() else { return };
        let Some(store) = &self.store else { return };
        rs.bookmarks = store.bookmarks(rs.repo.root()).unwrap_or_default();
    }

    /// Bookmark the cursor line.
    pub(super) fn add_bookmark(&mut self) {
        let Some(sf) = self.rs.as_ref().and_then(|rs| rs.commit.as_ref()).and_then(|c| c.file.as_ref()) else { return };
        let Some(cursor) = sf.cursor else {
            self.toast("Click a line first, then press Cmd/Ctrl+D.");
            return;
        };
        let browse = sf.browse.is_some();
        let Some((version, path)) = self.version_of(cursor.side) else { return };
        let line_text = match &sf.view {
            crate::state::ViewState::Ready(v) => match v.as_ref() {
                review_core::file_view::FileView::Text { diff, .. } => {
                    let lines = if cursor.side == Side::Old { &diff.old.lines } else { &diff.new.lines };
                    lines.get(cursor.line as usize).map(|l| l.trim().chars().take(80).collect::<String>()).unwrap_or_default()
                }
                _ => String::new(),
            },
            _ => String::new(),
        };
        let Some(rs) = self.rs.as_ref() else { return };
        let Some(sc) = rs.commit.as_ref() else { return };
        let new = NewBookmark {
            commit_id: sc.info.id.to_string(),
            version_id: version.commit.to_string(),
            path: path.clone(),
            side: if browse || cursor.side == Side::New { ThreadSide::New } else { ThreadSide::Old },
            line: cursor.line,
            note: if line_text.is_empty() { format!("{path}:{}", cursor.line + 1) } else { line_text },
        };
        let Some(store) = &self.store else { return };
        match store.add_bookmark(rs.repo.root(), &new) {
            Ok(_) => {
                self.load_bookmarks();
                self.toast(format!("Bookmarked {path}:{}", cursor.line + 1));
            }
            Err(e) => self.error = Some(format!("{e:#}")),
        }
    }

    pub(super) fn open_bookmark(&mut self, id: i64) {
        let Some(b) = self.rs.as_ref().and_then(|rs| rs.bookmarks.iter().find(|b| b.id == id)).cloned() else { return };
        let (Ok(commit), Ok(version)) = (parse_id(&b.commit_id), parse_id(&b.version_id)) else { return };
        let current = self.rs.as_ref().and_then(|rs| rs.commit.as_ref()).map(|c| c.info.id);
        if current != Some(commit) {
            self.select_commit_by_id(commit, 0, None, true);
            if let Some(sc) = self.rs.as_mut().and_then(|rs| rs.commit.as_mut()) {
                if b.side == ThreadSide::New && version == commit {
                    sc.pending_path = Some(b.path.clone());
                    sc.pending_line = Some((Side::New, b.line));
                } else {
                    sc.pending_browse = Some((version, b.path.clone()));
                    sc.pending_line = Some((Side::New, b.line));
                }
            }
            return;
        }
        self.open_location(version, &b.path, b.line);
    }

    pub(super) fn rename_bookmark(&mut self, id: i64, note: &str) {
        if let Some(store) = &self.store {
            let _ = store.rename_bookmark(id, note.trim());
        }
        self.load_bookmarks();
    }

    pub(super) fn delete_bookmark(&mut self, id: i64) {
        if let Some(store) = &self.store {
            let _ = store.delete_bookmark(id);
        }
        self.load_bookmarks();
    }

    pub(super) fn show_bookmarks(&mut self) {
        self.peek.open = true;
        self.peek.tab = PeekTab::Bookmarks;
    }
}

impl ReviewApp {
    /// Ask the update server whether a newer version exists (only when configured and enabled).
    pub(super) fn check_for_updates(&mut self, manual: bool) {
        if !crate::updater::available() {
            if manual {
                self.toast("This build has no update channel configured.");
            }
            return;
        }
        self.pool.spawn("Checking for updates", move |_, send| {
            send(JobResult::UpdateCheck { manual, result: crate::updater::check().map_err(|e| format!("{e:#}")) });
        });
    }

    pub(super) fn install_update(&mut self) {
        self.toast("Downloading the update; the app restarts when it is installed.");
        self.pool.spawn("Installing update", move |_, send| {
            if let Err(e) = crate::updater::install_and_restart() {
                send(JobResult::UpdateFailed(format!("{e:#}")));
            }
        });
    }
}
