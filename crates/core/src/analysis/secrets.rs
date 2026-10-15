//! Flag added lines that look like credentials: known token formats, private keys, and
//! high-entropy values assigned to secret-sounding names.

use std::ops::Range;
use std::sync::OnceLock;

use regex::Regex;

use crate::diff::{FileDiff, LineKind};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SecretFinding {
    /// 0-based line in the new version.
    pub line: u32,
    /// Byte range of the suspicious value.
    pub range: Range<usize>,
    pub kind: &'static str,
    /// The value with most of it hidden, safe to display.
    pub masked: String,
}

struct Pattern {
    kind: &'static str,
    re: Regex,
}

fn patterns() -> &'static [Pattern] {
    static P: OnceLock<Vec<Pattern>> = OnceLock::new();
    P.get_or_init(|| {
        [
            ("AWS access key", r"\b(?:AKIA|ASIA)[0-9A-Z]{16}\b"),
            ("GitHub token", r"\b(?:gh[pousr]_[A-Za-z0-9]{36,}|github_pat_[A-Za-z0-9_]{22,})\b"),
            ("Stripe live key", r"\b(?:sk|rk)_live_[A-Za-z0-9]{16,}\b"),
            ("Slack token", r"\bxox[abposr]-[A-Za-z0-9-]{10,}"),
            ("Google API key", r"\bAIza[0-9A-Za-z_\-]{35}\b"),
            ("Private key", r"-----BEGIN (?:[A-Z]+ )*PRIVATE KEY-----"),
            ("JSON Web Token", r"\beyJ[A-Za-z0-9_\-]{10,}\.[A-Za-z0-9_\-]{10,}\.[A-Za-z0-9_\-]{10,}"),
            ("Laravel APP_KEY", r"base64:[A-Za-z0-9+/]{40,}={0,2}"),
        ]
        .into_iter()
        .map(|(kind, re)| Pattern { kind, re: Regex::new(re).expect("valid pattern") })
        .collect()
    })
}

/// `NAME = value`, `'name' => 'value'`, `name: value`, where the name sounds secret.
fn assignment() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| {
        Regex::new(
            r#"(?i)["']?\b[A-Z0-9_.\-]*(?:SECRET|PASSWORD|PASSWD|PWD|TOKEN|API_?KEY|PRIVATE_?KEY|ACCESS_?KEY|AUTH_?KEY|CLIENT_?SECRET)[A-Z0-9_\-]*["']?\s*(?:=>|=|:)\s*["']?([^"'\s,;)]{8,})"#,
        )
        .expect("valid pattern")
    })
}

/// Values that are clearly placeholders or code, not secrets.
fn is_placeholder(v: &str) -> bool {
    let lower = v.to_ascii_lowercase();
    v.starts_with('$')
        || v.starts_with("env(")
        || v.starts_with("config(")
        || v.starts_with("${")
        || v.contains("(")
        || v.contains("->")
        || v.contains("::")
        || ["null", "true", "false", "changeme", "secret", "password", "example", "your_", "your-", "xxxx", "****", "<", "placeholder", "dummy", "test"]
            .iter()
            .any(|p| lower.contains(p))
}

/// Shannon entropy in bits per character.
pub fn entropy(s: &str) -> f64 {
    let mut counts = [0usize; 256];
    for b in s.bytes() {
        counts[b as usize] += 1;
    }
    let n = s.len() as f64;
    counts
        .iter()
        .filter(|&&c| c > 0)
        .map(|&c| {
            let p = c as f64 / n;
            -p * p.log2()
        })
        .sum()
}

fn mask(v: &str) -> String {
    let shown: String = v.chars().take(4).collect();
    format!("{shown}… ({} chars)", v.chars().count())
}

/// Suspicious values in one line.
pub fn scan_line(line: &str) -> Vec<(Range<usize>, &'static str)> {
    let mut out: Vec<(Range<usize>, &'static str)> = Vec::new();
    for p in patterns() {
        for m in p.re.find_iter(line) {
            out.push((m.range(), p.kind));
        }
    }
    for c in assignment().captures_iter(line) {
        let value = c.get(1).expect("group 1");
        let v = value.as_str();
        if out.iter().any(|(r, _)| r.start < value.end() && value.start() < r.end) {
            continue;
        }
        if !is_placeholder(v) && entropy(v) >= 3.0 {
            out.push((value.range(), "Possible secret"));
        }
    }
    out.sort_by_key(|(r, _)| r.start);
    out
}

/// Findings on lines a diff adds (moved lines are not new).
pub fn scan_diff(diff: &FileDiff) -> Vec<SecretFinding> {
    let mut out = Vec::new();
    for row in &diff.rows {
        let Some(cell) = &row.right else { continue };
        if cell.kind != LineKind::Added {
            continue;
        }
        let Some(text) = diff.new.lines.get(cell.line as usize) else { continue };
        for (range, kind) in scan_line(text) {
            let masked = mask(&text[range.clone()]);
            out.push(SecretFinding { line: cell.line, range, kind, masked });
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::diff::{DiffOptions, diff_texts};

    #[test]
    fn known_formats() {
        let kinds = |l: &str| scan_line(l).into_iter().map(|(_, k)| k).collect::<Vec<_>>();
        assert_eq!(kinds("AWS_ACCESS_KEY_ID=AKIAIOSFODNN7EXAMPLQ"), vec!["AWS access key"]);
        assert_eq!(kinds("$t = 'ghp_aBcDeFgHiJkLmNoPqRsTuVwXyZ0123456789';"), vec!["GitHub token"]);
        assert_eq!(kinds("STRIPE_SECRET=sk_live_4eC39HqLyjWDarjtT1zdp7dc"), vec!["Stripe live key"]);
        assert_eq!(kinds("-----BEGIN RSA PRIVATE KEY-----"), vec!["Private key"]);
        assert_eq!(kinds("APP_KEY=base64:2fl+Ktvkfl+Fuz4Qp/A75G2RTiWVA/ZoKZvp6fiiM10="), vec!["Laravel APP_KEY"]);
    }

    #[test]
    fn assignments_need_entropy_and_no_placeholder() {
        assert_eq!(scan_line("'api_key' => 'q8Zr2LmX0pTe7vNc4WbY',").len(), 1);
        assert_eq!(scan_line("DB_PASSWORD=Hk2#pL9!xQ7z").len(), 1);
        assert!(scan_line("DB_PASSWORD=").is_empty());
        assert!(scan_line("DB_PASSWORD=secret").is_empty());
        assert!(scan_line("'password' => env('DB_PASSWORD'),").is_empty());
        assert!(scan_line("'token' => $request->bearerToken(),").is_empty());
        assert!(scan_line("MAIL_PASSWORD=aaaaaaaaaaaa").is_empty(), "low entropy");
    }

    #[test]
    fn only_added_lines_count() {
        let old = "APP_NAME=Shop\nSTRIPE_KEY=sk_live_4eC39HqLyjWDarjtT1zdp7dc\n";
        let new = "APP_NAME=Shop\nSTRIPE_KEY=sk_live_4eC39HqLyjWDarjtT1zdp7dc\nAWS_KEY=AKIAIOSFODNN7EXAMPLQ\n";
        let found = scan_diff(&diff_texts(old, new, DiffOptions::default()));
        assert_eq!(found.len(), 1);
        assert_eq!((found[0].line, found[0].kind), (2, "AWS access key"));
        assert_eq!(found[0].masked, "AKIA… (20 chars)");
    }
}
