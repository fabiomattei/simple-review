//! Run PHPStan and Pint on a snapshot and parse their JSON output.
//!
//! These tools execute code from the reviewed project (configuration, extensions, bootstrap
//! files), so callers must only use them on repositories the user marked as trusted.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use serde_json::Value;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Tool {
    PhpStan,
    Pint,
}

impl Tool {
    pub fn label(self) -> &'static str {
        match self {
            Tool::PhpStan => "PHPStan",
            Tool::Pint => "Pint",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Warning {
    pub tool: Tool,
    /// Repository-relative path; empty for project-wide errors.
    pub path: String,
    /// 0-based line, when the tool reports one.
    pub line: Option<u32>,
    pub message: String,
    pub identifier: Option<String>,
}

const TIMEOUT: Duration = Duration::from_secs(600);

/// The command for a tool: the configured one, else the project's `vendor/bin` binary.
pub fn command(tool: Tool, configured: &str, dir: &Path) -> Option<Vec<String>> {
    let configured: Vec<String> = configured.split_whitespace().map(String::from).collect();
    if !configured.is_empty() {
        return Some(configured);
    }
    let bin = match tool {
        Tool::PhpStan => "vendor/bin/phpstan",
        Tool::Pint => "vendor/bin/pint",
    };
    dir.join(bin).exists().then(|| vec![bin.to_string()])
}

/// Run `cmd args` in `dir`, killing it on cancellation or timeout. Returns stdout and stderr.
pub fn run(dir: &Path, cmd: &[String], args: &[String], cancelled: &dyn Fn() -> bool) -> Result<(String, String)> {
    let program = {
        let p = PathBuf::from(&cmd[0]);
        if p.is_relative() && cmd[0].contains('/') { dir.join(p) } else { p }
    };
    let mut child = Command::new(&program)
        .args(&cmd[1..])
        .args(args)
        .current_dir(dir)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .with_context(|| format!("starting {}", program.display()))?;
    // Drain output on threads so a chatty tool can't block on a full pipe.
    let mut stdout = child.stdout.take().expect("piped");
    let mut stderr = child.stderr.take().expect("piped");
    let out = std::thread::spawn(move || {
        let mut s = String::new();
        let _ = std::io::Read::read_to_string(&mut stdout, &mut s);
        s
    });
    let err = std::thread::spawn(move || {
        let mut s = String::new();
        let _ = std::io::Read::read_to_string(&mut stderr, &mut s);
        s
    });
    let started = Instant::now();
    loop {
        if child.try_wait()?.is_some() {
            break;
        }
        if cancelled() || started.elapsed() > TIMEOUT {
            let _ = child.kill();
            let _ = child.wait();
            bail!(if cancelled() { "cancelled" } else { "timed out" });
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    Ok((out.join().unwrap_or_default(), err.join().unwrap_or_default()))
}

/// Strip the snapshot directory (in any of its spellings) from a path the tool reported.
fn relative(path: &str, dirs: &[String]) -> String {
    let path = path.split(" (in context of").next().unwrap_or(path).replace('\\', "/");
    for d in dirs {
        if let Some(rest) = path.strip_prefix(d.as_str()) {
            return rest.trim_start_matches('/').to_string();
        }
    }
    path
}

fn dir_spellings(dir: &Path) -> Vec<String> {
    let mut v = vec![dir.to_string_lossy().replace('\\', "/")];
    if let Ok(c) = dir.canonicalize() {
        v.push(c.to_string_lossy().replace('\\', "/"));
    }
    v
}

/// Parse `phpstan analyse --error-format=json` output.
pub fn parse_phpstan(json: &str, dir: &Path) -> Result<Vec<Warning>> {
    let start = json.find('{').context("PHPStan printed no JSON")?;
    let v: Value = serde_json::from_str(&json[start..]).context("PHPStan output is not valid JSON")?;
    let dirs = dir_spellings(dir);
    let mut out = Vec::new();
    if let Some(files) = v.get("files").and_then(Value::as_object) {
        for (file, data) in files {
            for m in data.get("messages").and_then(Value::as_array).into_iter().flatten() {
                out.push(Warning {
                    tool: Tool::PhpStan,
                    path: relative(file, &dirs),
                    line: m.get("line").and_then(Value::as_u64).map(|l| (l as u32).saturating_sub(1)),
                    message: m.get("message").and_then(Value::as_str).unwrap_or("").to_string(),
                    identifier: m.get("identifier").and_then(Value::as_str).map(String::from),
                });
            }
        }
    }
    for e in v.get("errors").and_then(Value::as_array).into_iter().flatten() {
        if let Some(message) = e.as_str() {
            out.push(Warning { tool: Tool::PhpStan, path: String::new(), line: None, message: message.to_string(), identifier: None });
        }
    }
    Ok(out)
}

/// Parse `pint --test --format=json` output: one warning per changed hunk.
pub fn parse_pint(json: &str, dir: &Path) -> Result<Vec<Warning>> {
    let Some(start) = json.find('{') else { return Ok(Vec::new()) };
    let v: Value = serde_json::from_str(&json[start..]).context("Pint output is not valid JSON")?;
    let dirs = dir_spellings(dir);
    let mut out = Vec::new();
    for f in v.get("files").and_then(Value::as_array).into_iter().flatten() {
        let path = f.get("name").or(f.get("path")).and_then(Value::as_str).unwrap_or("");
        let fixers: Vec<&str> = f.get("appliedFixers").and_then(Value::as_array).into_iter().flatten().filter_map(Value::as_str).collect();
        let message = format!("Style: {}", if fixers.is_empty() { "formatting".to_string() } else { fixers.join(", ") });
        let lines = f.get("diff").and_then(Value::as_str).map(changed_lines).unwrap_or_default();
        let path = relative(path, &dirs);
        if lines.is_empty() {
            out.push(Warning { tool: Tool::Pint, path, line: None, message, identifier: None });
        } else {
            for line in lines {
                out.push(Warning { tool: Tool::Pint, path: path.clone(), line: Some(line), message: message.clone(), identifier: None });
            }
        }
    }
    Ok(out)
}

/// First changed old-side line (0-based) of each hunk in a unified diff.
fn changed_lines(diff: &str) -> Vec<u32> {
    let mut out = Vec::new();
    let mut old_line: Option<u32> = None;
    let mut found = true;
    for l in diff.lines() {
        if let Some(rest) = l.strip_prefix("@@ -") {
            let start: u32 = rest.split([',', ' ']).next().and_then(|n| n.parse().ok()).unwrap_or(1);
            old_line = Some(start.saturating_sub(1));
            found = false;
        } else if let Some(n) = old_line.as_mut() {
            if l.starts_with("---") || l.starts_with("+++") {
                continue;
            }
            if !found && (l.starts_with('-') || l.starts_with('+')) {
                out.push(*n);
                found = true;
            }
            if !l.starts_with('+') {
                *n += 1;
            }
        }
    }
    out
}

/// Run a tool on `paths` inside a snapshot.
pub fn analyse(tool: Tool, cmd: &[String], dir: &Path, paths: &[String], cancelled: &dyn Fn() -> bool) -> Result<Vec<Warning>> {
    if paths.is_empty() {
        return Ok(Vec::new());
    }
    let mut args: Vec<String> = match tool {
        Tool::PhpStan => ["analyse", "--error-format=json", "--no-progress", "--no-interaction", "--memory-limit=2G"]
            .map(String::from)
            .to_vec(),
        Tool::Pint => ["--test", "--format=json"].map(String::from).to_vec(),
    };
    args.extend(paths.iter().cloned());
    let (stdout, stderr) = run(dir, cmd, &args, cancelled)?;
    let parsed = match tool {
        Tool::PhpStan => parse_phpstan(&stdout, dir),
        Tool::Pint => parse_pint(&stdout, dir),
    };
    parsed.with_context(|| {
        let tail: String = stderr.lines().rev().take(5).collect::<Vec<_>>().into_iter().rev().collect::<Vec<_>>().join("\n");
        format!("{} failed: {tail}", tool.label())
    })
}

/// Group warnings by path.
pub fn by_path(warnings: &[Warning]) -> HashMap<&str, Vec<&Warning>> {
    let mut m: HashMap<&str, Vec<&Warning>> = HashMap::new();
    for w in warnings {
        m.entry(w.path.as_str()).or_default().push(w);
    }
    m
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_phpstan_json() {
        let dir = Path::new("/tmp/snap");
        let json = r#"Note: something
{"totals":{"errors":0,"file_errors":2},"files":{"/tmp/snap/app/User.php":{"errors":2,"messages":[
 {"message":"Method App\\User::name() should return string but returns int.","line":12,"ignorable":true,"identifier":"return.type"},
 {"message":"Undefined variable: $x","line":20,"ignorable":true}]}},"errors":["Child process error"]}"#;
        let w = parse_phpstan(json, dir).unwrap();
        assert_eq!(w.len(), 3);
        assert_eq!((w[0].path.as_str(), w[0].line), ("app/User.php", Some(11)));
        assert_eq!(w[0].identifier.as_deref(), Some("return.type"));
        assert_eq!(w[2].path, "");
        assert!(parse_phpstan("Fatal error", dir).is_err());
    }

    #[test]
    fn parses_pint_json_with_hunks() {
        let json = r#"{"result":"fail","files":[{"name":"app/User.php","appliedFixers":["braces","no_unused_imports"],
            "diff":"--- Original\n+++ New\n@@ -3,7 +3,6 @@\n namespace App;\n \n-use Foo;\n use Bar;\n@@ -20,3 +19,3 @@\n a\n-b {\n+b\n+{\n"}]}"#;
        let w = parse_pint(json, Path::new("/x")).unwrap();
        assert_eq!(w.iter().map(|w| w.line).collect::<Vec<_>>(), vec![Some(4), Some(20)]);
        assert_eq!(w[0].message, "Style: braces, no_unused_imports");
        assert!(parse_pint("", Path::new("/x")).unwrap().is_empty());
    }

    #[test]
    fn runs_and_times_out_on_cancel() {
        let dir = std::env::temp_dir();
        let (out, _) = run(&dir, &["sh".into(), "-c".into(), "echo hi".into()], &[], &|| false).unwrap();
        assert_eq!(out.trim(), "hi");
        let started = Instant::now();
        assert!(run(&dir, &["sleep".into(), "5".into()], &[], &|| started.elapsed() > Duration::from_millis(100)).is_err());
        assert!(started.elapsed() < Duration::from_secs(3));
    }
}
