//! Line blame via `git blame --porcelain`.
//!
//! The git CLI is used because it is the only implementation that honours
//! `.git-blame-ignore-revs`, `.mailmap` and move/copy detection together. It only reads.

use std::collections::HashMap;
use std::process::Command;

use anyhow::{Context, Result, bail};
use gix::ObjectId;

use crate::git::{Repo, Signature, parse_id};

pub const IGNORE_REVS_FILE: &str = ".git-blame-ignore-revs";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BlameCommit {
    pub id: ObjectId,
    pub author: Signature,
    pub committer: Signature,
    pub summary: String,
    pub co_authors: Vec<String>,
    pub ai_assisted: bool,
    /// The commit is the root of the blamed range (`boundary` in porcelain output).
    pub boundary: bool,
    /// Path of the file in that commit (differs for renamed, moved or copied lines).
    pub path: String,
}

impl BlameCommit {
    pub fn committer_differs(&self) -> bool {
        self.author.email != self.committer.email || self.author.name != self.committer.name
    }
}

#[derive(Debug, Clone, Default)]
pub struct Blame {
    pub commits: Vec<BlameCommit>,
    /// For each line of the file (0-based), an index into `commits`.
    pub lines: Vec<u32>,
    /// For each line, its 0-based line number in the blamed commit.
    pub orig_lines: Vec<u32>,
}

impl Blame {
    pub fn commit_for_line(&self, line: usize) -> Option<&BlameCommit> {
        self.lines.get(line).map(|&i| &self.commits[i as usize])
    }

    /// Lines per person, most first. By author or committer name.
    pub fn share(&self, by_committer: bool) -> Vec<(String, usize)> {
        let mut counts: HashMap<&str, usize> = HashMap::new();
        for &i in &self.lines {
            let c = &self.commits[i as usize];
            let who = if by_committer { &c.committer.name } else { &c.author.name };
            *counts.entry(who).or_default() += 1;
        }
        let mut out: Vec<(String, usize)> = counts.into_iter().map(|(k, v)| (k.to_string(), v)).collect();
        out.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
        out
    }
}

/// Revisions listed in `.git-blame-ignore-revs` at `head`, keeping only ones that exist
/// (git refuses to run if asked to ignore an unknown revision).
pub fn ignore_revs(repo: &Repo, head: ObjectId) -> Vec<ObjectId> {
    let Ok(Some(file)) = repo.find_file(head, IGNORE_REVS_FILE) else { return Vec::new() };
    let Ok(bytes) = repo.read_blob(file.id) else { return Vec::new() };
    String::from_utf8_lossy(&bytes)
        .lines()
        .map(|l| l.split('#').next().unwrap_or("").trim())
        .filter(|l| l.len() >= 7 && l.chars().all(|c| c.is_ascii_hexdigit()))
        .filter_map(|l| repo.resolve(l).ok())
        .collect()
}

/// Blame `path` as of `commit`, with move/copy detection and the given ignored revisions.
pub fn blame_file(repo: &Repo, commit: ObjectId, path: &str, ignore: &[ObjectId]) -> Result<Blame> {
    let mut cmd = Command::new("git");
    cmd.current_dir(repo.root())
        .env("GIT_DIR", repo.git_dir())
        .args(["blame", "--porcelain", "-M", "-C"]);
    for rev in ignore {
        cmd.arg("--ignore-rev").arg(rev.to_string());
    }
    cmd.arg(commit.to_string()).arg("--").arg(path);
    let out = cmd.output().context("running git blame (is git installed?)")?;
    if !out.status.success() {
        bail!("git blame failed: {}", String::from_utf8_lossy(&out.stderr).trim());
    }
    let mut blame = parse_porcelain(&String::from_utf8_lossy(&out.stdout))?;
    for c in &mut blame.commits {
        if let Ok(info) = repo.commit(c.id) {
            c.co_authors = info.co_authors;
            c.ai_assisted = info.ai_assisted;
        }
    }
    Ok(blame)
}

fn parse_tz(tz: &str) -> i32 {
    let sign = if tz.starts_with('-') { -1 } else { 1 };
    let digits = tz.trim_start_matches(['+', '-']);
    let (h, m) = digits.split_at(digits.len().min(2));
    sign * (h.parse::<i32>().unwrap_or(0) * 3600 + m.parse::<i32>().unwrap_or(0) * 60)
}

pub fn parse_porcelain(text: &str) -> Result<Blame> {
    let mut blame = Blame::default();
    let mut index: HashMap<ObjectId, u32> = HashMap::new();
    let mut current: Option<u32> = None;
    for line in text.lines() {
        if let Some(_content) = line.strip_prefix('\t') {
            continue;
        }
        let mut parts = line.split(' ');
        let first = parts.next().unwrap_or("");
        if first.len() >= 40 && first.chars().all(|c| c.is_ascii_hexdigit()) {
            let id = parse_id(first)?;
            let orig: u32 = parts.next().and_then(|s| s.parse().ok()).unwrap_or(1);
            let final_line: u32 = parts.next().and_then(|s| s.parse().ok()).context("malformed blame header")?;
            let idx = *index.entry(id).or_insert_with(|| {
                blame.commits.push(BlameCommit {
                    id,
                    author: empty_sig(),
                    committer: empty_sig(),
                    summary: String::new(),
                    co_authors: Vec::new(),
                    ai_assisted: false,
                    boundary: false,
                    path: String::new(),
                });
                (blame.commits.len() - 1) as u32
            });
            let at = (final_line.saturating_sub(1)) as usize;
            if blame.lines.len() <= at {
                blame.lines.resize(at + 1, idx);
                blame.orig_lines.resize(at + 1, 0);
            }
            blame.lines[at] = idx;
            blame.orig_lines[at] = orig.saturating_sub(1);
            current = Some(idx);
            continue;
        }
        let Some(idx) = current else { continue };
        let c = &mut blame.commits[idx as usize];
        let value = line.split_once(' ').map_or("", |(_, v)| v);
        let email = || value.trim_start_matches('<').trim_end_matches('>').to_string();
        match first {
            "author" => c.author.name = value.to_string(),
            "author-mail" => c.author.email = email(),
            "author-time" => c.author.time = value.parse().unwrap_or(0),
            "author-tz" => c.author.offset = parse_tz(value),
            "committer" => c.committer.name = value.to_string(),
            "committer-mail" => c.committer.email = email(),
            "committer-time" => c.committer.time = value.parse().unwrap_or(0),
            "committer-tz" => c.committer.offset = parse_tz(value),
            "summary" => c.summary = value.to_string(),
            "boundary" => c.boundary = true,
            "filename" => c.path = value.to_string(),
            _ => {}
        }
    }
    Ok(blame)
}

fn empty_sig() -> Signature {
    Signature { name: String::new(), email: String::new(), time: 0, offset: 0 }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_porcelain() {
        let text = "\
aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa 1 1 2
author Ann
author-mail <ann@x>
author-time 1700000000
author-tz -0130
committer Bob
committer-mail <bob@x>
committer-time 1700000100
committer-tz +0000
summary First
boundary
filename a.php
\tline one
aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa 2 2
\tline two
bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb 7 3 1
author Cy
author-mail <cy@x>
author-time 1700000200
author-tz +0100
committer Cy
committer-mail <cy@x>
committer-time 1700000200
committer-tz +0100
summary Second
previous aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa old.php
filename old.php
\tline three
";
        let b = parse_porcelain(text).unwrap();
        assert_eq!(b.lines, vec![0, 0, 1]);
        assert_eq!(b.orig_lines, vec![0, 1, 6]);
        assert_eq!(b.commits[0].author.offset, -5400);
        assert!(b.commits[0].committer_differs());
        assert!(b.commits[0].boundary);
        assert_eq!(b.commits[1].path, "old.php");
        assert_eq!(b.share(false), vec![("Ann".to_string(), 2), ("Cy".to_string(), 1)]);
    }
}
