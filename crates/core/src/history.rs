//! History queries via the git CLI (read-only): pickaxe search and line/function history.

use std::collections::HashMap;
use std::process::Command;

use anyhow::{Context, Result, bail};
use gix::ObjectId;
use regex::Regex;

use crate::git::{CommitInfo, Repo, parse_id};

const MARK: &str = "__SR__";

fn git(repo: &Repo, args: &[&str]) -> Result<String> {
    let out = Command::new("git")
        .current_dir(repo.root())
        .env("GIT_DIR", repo.git_dir())
        .args(args)
        .output()
        .context("running git (is it installed?)")?;
    if !out.status.success() {
        bail!("git {} failed: {}", args.first().unwrap_or(&""), String::from_utf8_lossy(&out.stderr).trim());
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    Added,
    Removed,
    /// Both, or a count change that isn't all one way.
    Changed,
}

#[derive(Debug, Clone)]
pub struct PickaxeHit {
    pub commit: CommitInfo,
    /// Files where the number of matches changed, and in which direction.
    pub files: Vec<(String, Direction)>,
}

/// Commits (newest first, from `tip`) that changed how often `needle` occurs, like `git log -S`
/// (or `-G` with `regex`). Optionally limited to `path`.
pub fn pickaxe(repo: &Repo, tip: ObjectId, needle: &str, regex: bool, path: Option<&str>, max: usize) -> Result<Vec<PickaxeHit>> {
    if needle.is_empty() {
        bail!("enter some text to search for");
    }
    let pattern = if regex { Some(Regex::new(needle).context("invalid regular expression")?) } else { None };
    let count = |text: &str| match &pattern {
        Some(re) => re.find_iter(text).count(),
        None => text.matches(needle).count(),
    };
    let tip = tip.to_string();
    let max = format!("-n{max}");
    let flag = if regex { format!("-G{needle}") } else { format!("-S{needle}") };
    let format = format!("--format={MARK}%H");
    let mut args = vec!["log", tip.as_str(), max.as_str(), flag.as_str(), format.as_str(), "--name-only", "--no-renames"];
    if let Some(p) = path {
        args.extend(["--", p]);
    }
    let out = git(repo, &args)?;
    let mut hits = Vec::new();
    for block in out.split(MARK).filter(|b| !b.trim().is_empty()) {
        let mut lines = block.lines().map(str::trim).filter(|l| !l.is_empty());
        let Some(id) = lines.next().and_then(|h| parse_id(h).ok()) else { continue };
        let commit = repo.commit(id)?;
        let parent = commit.parents.first().copied();
        let read = |c: Option<ObjectId>, p: &str| -> usize {
            c.and_then(|c| repo.find_file(c, p).ok().flatten())
                .and_then(|f| repo.read_blob(f.id).ok())
                .map_or(0, |b| count(&String::from_utf8_lossy(&b)))
        };
        let files = lines
            .map(|p| {
                let (before, after) = (read(parent, p), read(Some(id), p));
                let dir = match (before, after) {
                    (0, _) => Direction::Added,
                    (_, 0) => Direction::Removed,
                    (b, a) if a > b => Direction::Added,
                    (b, a) if a < b => Direction::Removed,
                    _ => Direction::Changed,
                };
                (p.to_string(), dir)
            })
            .collect();
        hits.push(PickaxeHit { commit, files });
    }
    Ok(hits)
}

/// One step of a line range's history.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LineStep {
    pub commit: ObjectId,
    /// Path of the file in that commit (follows renames).
    pub path: String,
    /// Where the tracked lines are after that commit (0-based, inclusive); `None` if the
    /// commit deleted them.
    pub lines: Option<(u32, u32)>,
    /// The commit created the lines from nothing: history ends here.
    pub created: bool,
}

/// Commits that changed lines `start..=end` (0-based) of `path` at `commit`, newest first,
/// following moves and renames (`git log -L`). The last step is where the lines originated.
pub fn line_history(repo: &Repo, commit: ObjectId, path: &str, start: u32, end: u32) -> Result<Vec<LineStep>> {
    let range = format!("-L{},{}:{}", start + 1, end.max(start) + 1, path);
    let format = format!("--format={MARK}%H");
    let commit = commit.to_string();
    let out = git(repo, &["log", commit.as_str(), range.as_str(), format.as_str(), "--no-color"])?;
    let mut steps = Vec::new();
    for block in out.split(MARK).filter(|b| !b.trim().is_empty()) {
        let mut lines = block.lines();
        let Some(id) = lines.next().and_then(|h| parse_id(h.trim()).ok()) else { continue };
        let mut file = path.to_string();
        let mut range = None;
        let mut created = false;
        let (mut removed, mut added): (Vec<&str>, Vec<&str>) = (Vec::new(), Vec::new());
        for l in lines {
            if range.is_some() && !l.starts_with("+++") && !l.starts_with("---") {
                if let Some(r) = l.strip_prefix('-') {
                    removed.push(r);
                } else if let Some(a) = l.strip_prefix('+') {
                    added.push(a);
                }
            }
            if let Some(rest) = l.strip_prefix("+++ ") {
                if rest != "/dev/null" {
                    file = rest.strip_prefix("b/").unwrap_or(rest).to_string();
                }
            } else if l.starts_with("@@ ") && range.is_none() {
                // @@ -a,b +c,d @@ — b == 0 means the lines did not exist before.
                let old = l.split_whitespace().find(|p| p.starts_with('-')).unwrap_or("-0,1");
                created = old.split(',').nth(1).is_some_and(|b| b == "0");
                let new = l.split_whitespace().find(|p| p.starts_with('+')).unwrap_or("+0,0");
                let mut nums = new[1..].split(',');
                let c: u32 = nums.next().and_then(|n| n.parse().ok()).unwrap_or(0);
                let d: u32 = nums.next().and_then(|n| n.parse().ok()).unwrap_or(1);
                range = (d > 0).then(|| (c.saturating_sub(1), c.saturating_sub(1) + d - 1));
            }
        }
        // Git may pair new lines with unrelated old ones ("edit"); that is a creation too.
        if !created && !removed.is_empty() && similarity(&removed.join("\n"), &added.join("\n")) < 0.4 {
            created = true;
        }
        steps.push(LineStep { commit: id, path: file, lines: range, created });
        // `git log -L` keeps listing commits that touched the empty spot the lines were
        // inserted into; they are not part of these lines' history.
        if created {
            break;
        }
    }
    Ok(steps)
}

/// Token overlap (Dice coefficient) of two snippets of code.
fn similarity(a: &str, b: &str) -> f32 {
    let tokens = |s: &str| -> std::collections::HashSet<String> {
        s.split(|c: char| !(c.is_alphanumeric() || c == '_' || c == '$')).filter(|t| !t.is_empty()).map(String::from).collect()
    };
    let (ta, tb) = (tokens(a), tokens(b));
    if ta.is_empty() && tb.is_empty() {
        return 1.0;
    }
    2.0 * ta.intersection(&tb).count() as f32 / (ta.len() + tb.len()) as f32
}

/// The commit that first introduced the lines (the oldest step of their history).
pub fn original_author(repo: &Repo, commit: ObjectId, path: &str, start: u32, end: u32) -> Result<Option<CommitInfo>> {
    let steps = line_history(repo, commit, path, start, end)?;
    steps.last().map(|s| repo.commit(s.commit)).transpose()
}

/// Group pickaxe hits by file, for summaries.
pub fn files_touched(hits: &[PickaxeHit]) -> HashMap<&str, usize> {
    let mut m = HashMap::new();
    for h in hits {
        for (f, _) in &h.files {
            *m.entry(f.as_str()).or_default() += 1;
        }
    }
    m
}
