//! Quality signals: new static-analysis warnings, coverage, complexity, secrets.

pub mod complexity;
pub mod coverage;
pub mod secrets;
pub mod snapshot;
pub mod tools;

use std::collections::HashMap;
use std::path::Path;

use anyhow::Result;
use gix::ObjectId;

use crate::git::{ChangeStatus, FileChange, Repo};
use tools::{Tool, Warning};

/// Commands configured per repository; empty means "use the project's vendor/bin".
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ToolCommands {
    pub phpstan: String,
    pub pint: String,
}

#[derive(Debug, Clone, Default)]
pub struct AnalysisResult {
    /// Warnings in the new version that have no counterpart in the old one.
    pub introduced: Vec<Warning>,
    /// Total warnings reported for the changed files in each version.
    pub old_total: usize,
    pub new_total: usize,
    pub ran: Vec<Tool>,
    /// Things the user should know: missing tools, failures, a vendor/ mismatch.
    pub notes: Vec<String>,
}

/// Identity of a warning across versions: tool, path (in the new version), message and the text
/// of the line it points at, so moved code doesn't produce "new" warnings.
fn key(w: &Warning, path: &str, line_text: &str) -> (Tool, String, String, String) {
    (w.tool, path.to_string(), w.message.trim().to_string(), line_text.trim().to_string())
}

/// Warnings in `new` not present in `old`. `old_path` maps a new path to its path in the old
/// version (renames); line texts come from the respective versions.
pub fn introduced(
    old: &[Warning],
    new: &[Warning],
    old_path: &dyn Fn(&str) -> String,
    old_line: &dyn Fn(&str, u32) -> String,
    new_line: &dyn Fn(&str, u32) -> String,
) -> Vec<Warning> {
    let reverse: HashMap<String, String> = new.iter().map(|w| (old_path(&w.path), w.path.clone())).collect();
    let mut counts: HashMap<(Tool, String, String, String), usize> = HashMap::new();
    for w in old {
        let path = reverse.get(&w.path).cloned().unwrap_or_else(|| w.path.clone());
        let text = w.line.map(|l| old_line(&w.path, l)).unwrap_or_default();
        *counts.entry(key(w, &path, &text)).or_default() += 1;
    }
    new.iter()
        .filter(|w| {
            let text = w.line.map(|l| new_line(&w.path, l)).unwrap_or_default();
            match counts.get_mut(&key(w, &w.path, &text)) {
                Some(n) if *n > 0 => {
                    *n -= 1;
                    false
                }
                _ => true,
            }
        })
        .cloned()
        .collect()
}

fn read_line(dir: &Path, path: &str, line: u32) -> String {
    std::fs::read_to_string(dir.join(path))
        .ok()
        .and_then(|t| t.lines().nth(line as usize).map(String::from))
        .unwrap_or_default()
}

/// Run PHPStan and Pint on the changed PHP files of `old..new` and keep only new warnings.
/// Executes project code: only call this for trusted repositories.
#[allow(clippy::too_many_arguments)]
pub fn analyse_change(
    repo: &Repo,
    cache: &Path,
    old: Option<ObjectId>,
    new: ObjectId,
    changes: &[FileChange],
    commands: &ToolCommands,
    progress: &dyn Fn(f32),
    cancelled: &dyn Fn() -> bool,
) -> Result<AnalysisResult> {
    let mut result = AnalysisResult::default();
    let php = |p: &str| p.ends_with(".php");
    let new_paths: Vec<String> = changes
        .iter()
        .filter(|c| c.status != ChangeStatus::Deleted)
        .filter_map(|c| c.new.as_ref().map(|s| s.path.clone()))
        .filter(|p| php(p))
        .collect();
    let old_paths: Vec<String> = changes
        .iter()
        .filter(|c| c.status != ChangeStatus::Added && c.new.as_ref().is_some_and(|s| php(&s.path)))
        .filter_map(|c| c.old.as_ref().map(|s| s.path.clone()))
        .collect();
    if new_paths.is_empty() {
        result.notes.push("No changed PHP files to analyse.".into());
        return Ok(result);
    }
    let renames: HashMap<String, String> = changes
        .iter()
        .filter_map(|c| Some((c.new.as_ref()?.path.clone(), c.old.as_ref()?.path.clone())))
        .collect();

    progress(0.05);
    let new_snap = snapshot::ensure(repo, new, cache, cancelled)?;
    let old_snap = old.map(|o| snapshot::ensure(repo, o, cache, cancelled)).transpose()?;
    if new_snap.vendor_mismatch {
        result.notes.push(
            "composer.lock at this commit differs from your working copy; vendor/ is linked from the working copy, so results may be off."
                .into(),
        );
    } else if !new_snap.vendor_linked {
        result.notes.push("No vendor/ in the working copy: run composer install there first.".into());
    }
    progress(0.2);

    let tools = [(Tool::PhpStan, &commands.phpstan), (Tool::Pint, &commands.pint)];
    for (i, (tool, configured)) in tools.iter().enumerate() {
        let Some(cmd) = tools::command(*tool, configured, &new_snap.dir) else {
            result.notes.push(format!("{} not found (vendor/bin or Settings).", tool.label()));
            continue;
        };
        let new_w = match tools::analyse(*tool, &cmd, &new_snap.dir, &new_paths, cancelled) {
            Ok(w) => w,
            Err(e) => {
                result.notes.push(format!("{e:#}"));
                continue;
            }
        };
        progress(0.2 + 0.4 * (i as f32 + 0.5));
        let old_w = match &old_snap {
            Some(snap) => tools::command(*tool, configured, &snap.dir)
                .map(|cmd| tools::analyse(*tool, &cmd, &snap.dir, &old_paths, cancelled))
                .transpose()
                .unwrap_or_else(|e| {
                    result.notes.push(format!("{} on the old version: {e:#}", tool.label()));
                    Some(Vec::new())
                })
                .unwrap_or_default(),
            None => Vec::new(),
        };
        result.old_total += old_w.len();
        result.new_total += new_w.len();
        let old_dir = old_snap.as_ref().map(|s| s.dir.clone());
        let fresh = introduced(
            &old_w,
            &new_w,
            &|p| renames.get(p).cloned().unwrap_or_else(|| p.to_string()),
            &|p, l| old_dir.as_ref().map(|d| read_line(d, p, l)).unwrap_or_default(),
            &|p, l| read_line(&new_snap.dir, p, l),
        );
        result.introduced.extend(fresh);
        result.ran.push(*tool);
        progress(0.2 + 0.4 * (i as f32 + 1.0));
    }
    let _ = snapshot::prune(cache, repo, 6);
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn w(path: &str, line: u32, message: &str) -> Warning {
        Warning { tool: Tool::PhpStan, path: path.into(), line: Some(line), message: message.into(), identifier: None }
    }

    #[test]
    fn only_new_warnings_survive() {
        let old = vec![w("old/A.php", 3, "Undefined variable $x"), w("old/A.php", 9, "Bad return")];
        let new = vec![
            w("new/A.php", 5, "Undefined variable $x"), // moved down two lines, same code: not new
            w("new/A.php", 9, "Bad return"),           // same message, different code: new
            w("new/A.php", 12, "Unused method"),       // new
        ];
        let old_text = |_: &str, l: u32| match l {
            3 => "echo $x;".to_string(),
            9 => "return 1;".to_string(),
            _ => String::new(),
        };
        let new_text = |_: &str, l: u32| match l {
            5 => "  echo $x;".to_string(),
            9 => "return 'a';".to_string(),
            _ => String::new(),
        };
        let fresh = introduced(&old, &new, &|p| p.replace("new/", "old/"), &old_text, &new_text);
        let lines: Vec<_> = fresh.iter().map(|w| w.line.unwrap()).collect();
        assert_eq!(lines, vec![9, 12]);
    }

    #[test]
    fn duplicates_are_counted() {
        let old = vec![w("A.php", 1, "m")];
        let new = vec![w("A.php", 1, "m"), w("A.php", 2, "m")];
        let same = |_: &str, _: u32| "x".to_string();
        assert_eq!(introduced(&old, &new, &|p| p.to_string(), &same, &same).len(), 1);
    }
}
