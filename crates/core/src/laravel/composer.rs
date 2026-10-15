//! `composer.lock` changes as package version changes, and an opt-in advisory check.

use std::collections::BTreeMap;
use std::path::Path;

use anyhow::{Context, Result};
use serde_json::Value;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Package {
    pub version: String,
    pub dev: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PackageChange {
    Added { name: String, version: String, dev: bool },
    Removed { name: String, version: String, dev: bool },
    Changed { name: String, from: String, to: String, dev: bool, major: bool, downgrade: bool },
}

impl PackageChange {
    pub fn name(&self) -> &str {
        match self {
            PackageChange::Added { name, .. } | PackageChange::Removed { name, .. } | PackageChange::Changed { name, .. } => name,
        }
    }
}

pub fn parse_lock(text: &str) -> Result<BTreeMap<String, Package>> {
    let v: Value = serde_json::from_str(text).context("composer.lock is not valid JSON")?;
    let mut out = BTreeMap::new();
    for (key, dev) in [("packages", false), ("packages-dev", true)] {
        for p in v.get(key).and_then(Value::as_array).into_iter().flatten() {
            if let (Some(name), Some(version)) = (p.get("name").and_then(Value::as_str), p.get("version").and_then(Value::as_str)) {
                out.insert(name.to_string(), Package { version: version.to_string(), dev });
            }
        }
    }
    Ok(out)
}

/// Numeric parts of a version (`v1.2.3-beta` → [1, 2, 3]); empty for branch versions like `dev-main`.
fn numbers(version: &str) -> Vec<u64> {
    let v = version.trim_start_matches('v');
    if v.starts_with("dev-") {
        return Vec::new();
    }
    v.split(|c: char| !c.is_ascii_digit() && c != '.')
        .next()
        .unwrap_or("")
        .split('.')
        .filter_map(|p| p.parse().ok())
        .collect()
}

pub fn diff_lock(old: &BTreeMap<String, Package>, new: &BTreeMap<String, Package>) -> Vec<PackageChange> {
    let mut out = Vec::new();
    for (name, p) in new {
        match old.get(name) {
            None => out.push(PackageChange::Added { name: name.clone(), version: p.version.clone(), dev: p.dev }),
            Some(o) if o.version != p.version => {
                let (a, b) = (numbers(&o.version), numbers(&p.version));
                let major = !a.is_empty() && !b.is_empty() && a[0] != b[0];
                out.push(PackageChange::Changed {
                    name: name.clone(),
                    from: o.version.clone(),
                    to: p.version.clone(),
                    dev: p.dev,
                    major,
                    downgrade: !a.is_empty() && !b.is_empty() && b < a,
                });
            }
            Some(_) => {}
        }
    }
    for (name, p) in old {
        if !new.contains_key(name) {
            out.push(PackageChange::Removed { name: name.clone(), version: p.version.clone(), dev: p.dev });
        }
    }
    out.sort_by(|a, b| a.name().cmp(b.name()));
    out
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Advisory {
    pub package: String,
    pub title: String,
    pub cve: Option<String>,
    pub link: Option<String>,
    pub affected: String,
    pub severity: Option<String>,
}

/// Parse `composer audit --format=json` output.
pub fn parse_audit(json: &str) -> Result<Vec<Advisory>> {
    let start = json.find('{').context("composer audit printed no JSON")?;
    let v: Value = serde_json::from_str(&json[start..]).context("composer audit output is not valid JSON")?;
    let mut out = Vec::new();
    if let Some(map) = v.get("advisories").and_then(Value::as_object) {
        for (package, list) in map {
            // A list, or an object keyed by index in some Composer versions.
            let items: Vec<&Value> = match list {
                Value::Array(a) => a.iter().collect(),
                Value::Object(o) => o.values().collect(),
                _ => Vec::new(),
            };
            for a in items {
                let s = |k: &str| a.get(k).and_then(Value::as_str).map(String::from);
                out.push(Advisory {
                    package: package.clone(),
                    title: s("title").unwrap_or_default(),
                    cve: s("cve"),
                    link: s("link"),
                    affected: s("affectedVersions").unwrap_or_default(),
                    severity: s("severity"),
                });
            }
        }
    }
    Ok(out)
}

/// Run `composer audit` against the lock file in `dir`. Contacts Packagist: only on request.
/// `--no-plugins` keeps project code from running.
pub fn audit(dir: &Path, cancelled: &dyn Fn() -> bool) -> Result<Vec<Advisory>> {
    let cmd = ["composer".to_string()];
    let args: Vec<String> = ["audit", "--locked", "--no-plugins", "--no-interaction", "--format=json"].map(String::from).to_vec();
    let (stdout, stderr) = crate::analysis::tools::run(dir, &cmd, &args, cancelled)?;
    parse_audit(&stdout).with_context(|| stderr.lines().last().unwrap_or("").to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lock(pkgs: &[(&str, &str)], dev: &[(&str, &str)]) -> String {
        let list = |l: &[(&str, &str)]| {
            l.iter().map(|(n, v)| format!(r#"{{"name":"{n}","version":"{v}"}}"#)).collect::<Vec<_>>().join(",")
        };
        format!(r#"{{"packages":[{}],"packages-dev":[{}]}}"#, list(pkgs), list(dev))
    }

    #[test]
    fn diffs_versions() {
        let old = parse_lock(&lock(&[("laravel/framework", "v11.9.0"), ("guzzlehttp/guzzle", "7.8.1"), ("old/pkg", "1.0.0")], &[("phpunit/phpunit", "10.5.0")])).unwrap();
        let new = parse_lock(&lock(&[("laravel/framework", "v12.0.1"), ("guzzlehttp/guzzle", "7.8.0"), ("new/pkg", "dev-main")], &[("phpunit/phpunit", "10.5.0")])).unwrap();
        let d = diff_lock(&old, &new);
        assert_eq!(d.len(), 4);
        assert_eq!(d[0], PackageChange::Changed { name: "guzzlehttp/guzzle".into(), from: "7.8.1".into(), to: "7.8.0".into(), dev: false, major: false, downgrade: true });
        assert!(matches!(&d[1], PackageChange::Changed { major: true, downgrade: false, .. }));
        assert_eq!(d[2], PackageChange::Added { name: "new/pkg".into(), version: "dev-main".into(), dev: false });
        assert!(matches!(&d[3], PackageChange::Removed { name, .. } if name == "old/pkg"));
    }

    #[test]
    fn parses_audit_output() {
        let json = r#"{"advisories":{"guzzlehttp/guzzle":[{"advisoryId":"PKSA-1","packageName":"guzzlehttp/guzzle","title":"Cookie leak","cve":"CVE-2022-29248","link":"https://example.test","affectedVersions":"<7.4.3","severity":"high"}]},"abandoned":[]}"#;
        let a = parse_audit(json).unwrap();
        assert_eq!(a.len(), 1);
        assert_eq!(a[0].cve.as_deref(), Some("CVE-2022-29248"));
        assert!(parse_audit(r#"{"advisories":[]}"#).unwrap().is_empty());
    }
}
