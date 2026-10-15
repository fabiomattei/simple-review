//! Clover XML coverage (PHPUnit `--coverage-clover`, Pest `--coverage-clover`).

use std::collections::HashMap;
use std::path::Path;

use anyhow::{Context, Result};
use quick_xml::{Reader, XmlVersion};
use quick_xml::events::Event;

use crate::diff::{FileDiff, LineKind};

/// Hit counts of executable lines in one file (1-based line numbers, as in Clover).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FileCoverage {
    pub lines: HashMap<u32, u32>,
}

impl FileCoverage {
    /// `Some(true)` covered, `Some(false)` executable but never run, `None` not executable.
    pub fn is_covered(&self, line0: u32) -> Option<bool> {
        self.lines.get(&(line0 + 1)).map(|&count| count > 0)
    }
}

#[derive(Debug, Clone, Default)]
pub struct Coverage {
    /// Paths as written in the report (usually absolute, from the machine that ran the tests).
    files: Vec<(String, FileCoverage)>,
}

impl Coverage {
    pub fn load(path: &Path) -> Result<Self> {
        let xml = std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
        Self::parse(&xml)
    }

    pub fn parse(xml: &str) -> Result<Self> {
        let mut reader = Reader::from_str(xml);
        let mut files = Vec::new();
        let mut current: Option<(String, FileCoverage)> = None;
        loop {
            match reader.read_event().context("invalid Clover XML")? {
                Event::Start(e) | Event::Empty(e) if e.name().as_ref() == "file" => {
                    if let Some(done) = current.take() {
                        files.push(done);
                    }
                    let name = e
                        .try_get_attribute("path")?
                        .or(e.try_get_attribute("name")?)
                        .map(|a| a.normalized_value(XmlVersion::Implicit1_0).map(|v| v.into_owned()))
                        .transpose()?
                        .unwrap_or_default();
                    current = Some((name.replace('\\', "/"), FileCoverage::default()));
                }
                Event::Start(e) | Event::Empty(e) if e.name().as_ref() == "line" => {
                    let attr = |n: &str| -> Option<String> {
                        e.try_get_attribute(n).ok().flatten().and_then(|a| a.normalized_value(XmlVersion::Implicit1_0).ok()).map(|v| v.into_owned())
                    };
                    if let (Some((_, fc)), Some(num), Some(count)) = (current.as_mut(), attr("num"), attr("count"))
                        && attr("type").as_deref() != Some("method")
                        && let (Ok(num), Ok(count)) = (num.parse::<u32>(), count.parse::<u32>())
                    {
                        let slot = fc.lines.entry(num).or_insert(0);
                        *slot = (*slot).max(count);
                    }
                }
                Event::End(e) if e.name().as_ref() == "file" => {
                    if let Some(done) = current.take() {
                        files.push(done);
                    }
                }
                Event::Eof => {
                    if let Some(done) = current.take() {
                        files.push(done);
                    }
                    break;
                }
                _ => {}
            }
        }
        Ok(Coverage { files })
    }

    pub fn file_count(&self) -> usize {
        self.files.len()
    }

    /// Coverage for a repository-relative path, matched as a path suffix.
    pub fn for_file(&self, repo_path: &str) -> Option<&FileCoverage> {
        let suffix = format!("/{repo_path}");
        self.files
            .iter()
            .find(|(p, _)| p == repo_path || p.ends_with(&suffix))
            .map(|(_, fc)| fc)
    }
}

/// Added lines of `diff` that are executable but not covered (0-based, new side).
pub fn uncovered_added(diff: &FileDiff, cov: &FileCoverage) -> Vec<u32> {
    diff.rows
        .iter()
        .filter_map(|r| r.right.as_ref())
        .filter(|c| matches!(c.kind, LineKind::Added | LineKind::Moved))
        .filter(|c| cov.is_covered(c.line) == Some(false))
        .map(|c| c.line)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::diff::{DiffOptions, diff_texts};

    pub const SAMPLE: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<coverage generated="1700000000">
  <project timestamp="1700000000">
    <file name="/home/ci/shop/app/Services/InvoiceService.php">
      <class name="App\Services\InvoiceService" namespace="App\Services"/>
      <line num="9" type="method" name="total" count="3"/>
      <line num="11" type="stmt" count="3"/>
      <line num="12" type="stmt" count="0"/>
      <line num="13" type="stmt" count="0"/>
    </file>
    <file name="/home/ci/shop/app/Models/User.php"/>
  </project>
</coverage>"#;

    #[test]
    fn parses_and_matches_by_suffix() {
        let cov = Coverage::parse(SAMPLE).unwrap();
        assert_eq!(cov.file_count(), 2);
        let fc = cov.for_file("app/Services/InvoiceService.php").unwrap();
        assert_eq!(fc.is_covered(10), Some(true));
        assert_eq!(fc.is_covered(11), Some(false));
        assert_eq!(fc.is_covered(8), None, "method lines are not statements");
        assert!(cov.for_file("Services/InvoiceService.php").is_some());
        assert!(cov.for_file("app/Other.php").is_none());
    }

    #[test]
    fn finds_uncovered_added_lines() {
        let cov = Coverage::parse(SAMPLE).unwrap();
        let fc = cov.for_file("app/Services/InvoiceService.php").unwrap();
        let old: String = (1..=13).map(|i| format!("l{i}\n")).collect();
        let new = old.replace("l12\n", "x12\n").replace("l11\n", "x11\n");
        let diff = diff_texts(&old, &new, DiffOptions::default());
        assert_eq!(uncovered_added(&diff, fc), vec![11]);
    }
}
