//! The Laravel tab: schema from migrations, Composer package changes, routes.

use egui::{RichText, Ui};
use review_core::laravel::composer::{Advisory, PackageChange};
use review_core::laravel::routes::{RouteAction, RouteChange, RouteDef};
use review_core::laravel::schema::{Column, TableChange};

use super::{Action, Palette, palette};
use crate::state::{LaravelData, LaravelTab};

pub struct LaravelView<'a> {
    pub data: Option<&'a Result<LaravelData, Option<String>>>,
    pub advisories: Option<&'a Result<Vec<Advisory>, Option<String>>>,
    pub artisan: Option<&'a Result<Vec<RouteChange>, Option<String>>>,
    pub trusted: bool,
}

pub fn show(ui: &mut Ui, tab: LaravelTab, filter: &mut String, v: &LaravelView<'_>, actions: &mut Vec<Action>) {
    let tabs = [(LaravelTab::Schema, "Schema", ""), (LaravelTab::Packages, "Packages", ""), (LaravelTab::Routes, "Routes", "")];
    if let Some(t) = super::theme::segmented(ui, tab, &tabs, false) {
        actions.push(Action::LaravelTab(t));
    }
    ui.separator();
    let data = match v.data {
        None | Some(Err(None)) => {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label("Reading migrations, composer.lock and routes…");
            });
            return;
        }
        Some(Err(Some(e))) => {
            ui.colored_label(ui.visuals().error_fg_color, e);
            return;
        }
        Some(Ok(d)) => d,
    };
    let pal = palette(ui);
    egui::ScrollArea::vertical().id_salt("laravel").auto_shrink([false, true]).show(ui, |ui| match tab {
        LaravelTab::Schema => schema(ui, &pal, data),
        LaravelTab::Packages => packages(ui, &pal, data, v.advisories, actions),
        LaravelTab::Routes => routes(ui, &pal, data, v, filter, actions),
    });
}

fn column_text(c: &Column) -> String {
    let mods = if c.modifiers.is_empty() { String::new() } else { format!("  {}", c.modifiers.join(" ")) };
    format!("{} {}{mods}", c.name, c.kind)
}

fn schema(ui: &mut Ui, pal: &Palette, d: &LaravelData) {
    let added = pal.status(review_core::git::ChangeStatus::Added);
    let removed = pal.status(review_core::git::ChangeStatus::Deleted);
    if d.schema_changes.is_empty() {
        ui.weak("This commit doesn't change the schema.");
    } else {
        ui.strong("Changed by this commit");
        for change in &d.schema_changes {
            match change {
                TableChange::Created(t) => {
                    ui.label(RichText::new(format!("+ table {}", t.name)).color(added));
                    for c in &t.columns {
                        ui.label(RichText::new(format!("    + {}", column_text(c))).monospace().color(added));
                    }
                }
                TableChange::Dropped(t) => {
                    ui.label(RichText::new(format!("- table {}", t.name)).color(removed));
                }
                TableChange::Altered { table, added: a, removed: r, changed, indexes_added, indexes_removed } => {
                    ui.label(RichText::new(format!("~ table {table}")).color(pal.warn));
                    for c in a {
                        ui.label(RichText::new(format!("    + {}", column_text(c))).monospace().color(added));
                    }
                    for c in r {
                        ui.label(RichText::new(format!("    - {}", column_text(c))).monospace().color(removed));
                    }
                    for (o, n) in changed {
                        ui.label(RichText::new(format!("    ~ {}  (was {})", column_text(n), column_text(o))).monospace().color(pal.warn));
                    }
                    for i in indexes_added {
                        ui.label(RichText::new(format!("    + {i}")).monospace().color(added));
                    }
                    for i in indexes_removed {
                        ui.label(RichText::new(format!("    - {i}")).monospace().color(removed));
                    }
                }
            }
        }
    }
    ui.separator();
    ui.strong(format!("Schema at this commit ({} tables, from {} migrations)", d.schema.tables.len(), d.schema.migrations.len()));
    ui.weak("Read from database/migrations without running them; raw SQL and conditions are not interpreted.");
    for t in d.schema.tables.values() {
        egui::CollapsingHeader::new(RichText::new(format!("{} ({} columns)", t.name, t.columns.len())).monospace())
            .id_salt(("table", &t.name))
            .show(ui, |ui| {
                for c in &t.columns {
                    ui.monospace(column_text(c));
                }
                for i in &t.indexes {
                    ui.label(RichText::new(i).monospace().color(pal.weak));
                }
                ui.weak(format!("migrations: {}", t.migrations.join(", ")));
            });
    }
}

fn packages(ui: &mut Ui, pal: &Palette, d: &LaravelData, advisories: Option<&Result<Vec<Advisory>, Option<String>>>, actions: &mut Vec<Action>) {
    match &d.packages {
        None => {
            ui.weak("composer.lock didn't change in this commit.");
        }
        Some(changes) => {
            ui.strong(format!("{} package change(s)", changes.len()));
            egui::Grid::new("packages").striped(true).num_columns(2).show(ui, |ui| {
                for c in changes {
                    let (text, colour) = match c {
                        PackageChange::Added { version, dev, .. } => (format!("+ {version}{}", if *dev { " (dev)" } else { "" }), pal.status(review_core::git::ChangeStatus::Added)),
                        PackageChange::Removed { version, .. } => (format!("- {version}"), pal.status(review_core::git::ChangeStatus::Deleted)),
                        PackageChange::Changed { from, to, major, downgrade, .. } => (
                            format!("{from} -> {to}{}{}", if *major { "  MAJOR" } else { "" }, if *downgrade { "  downgrade" } else { "" }),
                            if *major || *downgrade { pal.warn } else { pal.text },
                        ),
                    };
                    ui.monospace(c.name());
                    ui.label(RichText::new(text).color(colour));
                    ui.end_row();
                }
            });
        }
    }
    ui.separator();
    ui.horizontal(|ui| {
        let running = matches!(advisories, Some(Err(None)));
        if ui.add_enabled(!running, egui::Button::new("Check security advisories")).clicked() {
            actions.push(Action::CheckAdvisories);
        }
        ui.weak("Runs `composer audit` on this commit's lock file. Contacts Packagist; runs no project code.");
    });
    match advisories {
        None => {}
        Some(Err(None)) => {
            ui.spinner();
        }
        Some(Err(Some(e))) => {
            ui.colored_label(ui.visuals().error_fg_color, e);
        }
        Some(Ok(list)) if list.is_empty() => {
            ui.label("No known advisories for the locked versions.");
        }
        Some(Ok(list)) => {
            ui.label(RichText::new(format!("⚠ {} advisory(ies)", list.len())).color(pal.warn).strong());
            for a in list {
                ui.horizontal_wrapped(|ui| {
                    ui.monospace(&a.package);
                    if let Some(s) = &a.severity {
                        ui.label(RichText::new(s).color(pal.warn));
                    }
                    ui.label(&a.title);
                    if let Some(cve) = &a.cve {
                        ui.weak(cve);
                    }
                    ui.weak(format!("affects {}", a.affected));
                    if let Some(link) = &a.link {
                        ui.hyperlink_to("details", link);
                    }
                });
            }
        }
    }
}

fn route_line(r: &RouteDef) -> String {
    let name = r.name.as_ref().map(|n| format!("  [{n}]")).unwrap_or_default();
    format!("{:<10} /{}  -> {}{name}", r.methods.join("|"), r.uri.trim_start_matches('/'), r.action.label())
}

fn routes(ui: &mut Ui, pal: &Palette, d: &LaravelData, v: &LaravelView<'_>, filter: &mut String, actions: &mut Vec<Action>) {
    let changes = match v.artisan {
        Some(Ok(c)) => {
            ui.weak("Changes from `php artisan route:list` on both versions.");
            c
        }
        _ => &d.route_changes,
    };
    if changes.is_empty() {
        ui.weak("No route changes in this commit.");
    } else {
        ui.strong(format!("{} route change(s)", changes.len()));
        for c in changes {
            let (text, colour) = match c {
                RouteChange::Added(r) => (format!("+ {}", route_line(r)), pal.status(review_core::git::ChangeStatus::Added)),
                RouteChange::Removed(r) => (format!("- {}", route_line(r)), pal.status(review_core::git::ChangeStatus::Deleted)),
                RouteChange::Changed { old, new } => (format!("~ {}   (was {})", route_line(new), route_line(old)), pal.warn),
            };
            ui.label(RichText::new(text).monospace().color(colour));
        }
    }
    ui.horizontal(|ui| {
        if v.trusted {
            let running = matches!(v.artisan, Some(Err(None)));
            if ui.add_enabled(!running, egui::Button::new("Use artisan route:list")).on_hover_text("Exact routes, including packages; boots the app in a snapshot").clicked() {
                actions.push(Action::ArtisanRoutes);
            }
            if let Some(Err(Some(e))) = v.artisan {
                ui.colored_label(ui.visuals().error_fg_color, e);
            }
        } else {
            ui.weak("Read statically from routes/*.php. Trust the repository in Settings to use artisan.");
        }
    });
    ui.separator();
    ui.horizontal(|ui| {
        ui.strong(format!("Routes at this commit ({})", d.routes.len()));
        ui.add(egui::TextEdit::singleline(filter).hint_text("filter").desired_width(160.0));
    });
    let needle = filter.to_lowercase();
    for (i, r) in d.routes.iter().enumerate() {
        let line = route_line(r);
        if !needle.is_empty() && !line.to_lowercase().contains(&needle) {
            continue;
        }
        ui.horizontal(|ui| {
            if ui.link(RichText::new(&line).monospace()).on_hover_text("Open the controller method").clicked() {
                actions.push(Action::OpenRoute(i));
            }
            if let RouteAction::Controller { class, .. } = &r.action
                && ui.small_button("tests").on_hover_text("Open tests named after the controller").clicked()
            {
                actions.push(Action::OpenTestsFor(class.clone()));
            }
            if !r.middleware.is_empty() {
                ui.weak(r.middleware.join(", "));
            }
        });
    }
}
