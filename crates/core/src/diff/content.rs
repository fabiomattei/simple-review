//! Decide whether a blob can be shown as text, and decode it.

/// Files above either limit get a placeholder until the user asks to render them.
pub const MAX_TEXT_BYTES: usize = 2 * 1024 * 1024;
pub const MAX_TEXT_LINES: usize = 30_000;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Content {
    Text {
        text: String,
        /// The bytes were not valid UTF-8 and were decoded as Latin-1.
        non_utf8: bool,
    },
    Binary,
    Image,
    LfsPointer {
        oid: String,
        size: u64,
    },
    TooLarge {
        bytes: usize,
        lines: usize,
    },
}

impl Content {
    pub fn text(&self) -> Option<&str> {
        match self {
            Content::Text { text, .. } => Some(text),
            _ => None,
        }
    }
}

const IMAGE_EXTENSIONS: &[&str] = &["png", "jpg", "jpeg", "gif", "webp", "bmp", "ico", "tiff", "avif"];

pub fn is_image_path(path: &str) -> bool {
    extension(path).is_some_and(|ext| IMAGE_EXTENSIONS.contains(&ext.as_str()))
}

fn extension(path: &str) -> Option<String> {
    let name = path.rsplit('/').next()?;
    let (_, ext) = name.rsplit_once('.')?;
    Some(ext.to_ascii_lowercase())
}

/// Heuristic used by git: a NUL byte in the first 8000 bytes means binary.
pub fn looks_binary(bytes: &[u8]) -> bool {
    bytes[..bytes.len().min(8000)].contains(&0)
}

fn parse_lfs_pointer(bytes: &[u8]) -> Option<(String, u64)> {
    if bytes.len() > 1024 {
        return None;
    }
    let text = std::str::from_utf8(bytes).ok()?;
    if !text.starts_with("version https://git-lfs.github.com/spec/") {
        return None;
    }
    let mut oid = None;
    let mut size = None;
    for line in text.lines() {
        if let Some(v) = line.strip_prefix("oid ") {
            oid = Some(v.trim().to_string());
        } else if let Some(v) = line.strip_prefix("size ") {
            size = v.trim().parse().ok();
        }
    }
    Some((oid?, size?))
}

/// Classify a blob. `force` skips the size limits (the user clicked "show anyway").
pub fn classify(path: &str, bytes: &[u8], force: bool) -> Content {
    if let Some((oid, size)) = parse_lfs_pointer(bytes) {
        return Content::LfsPointer { oid, size };
    }
    if is_image_path(path) && !path.ends_with(".svg") {
        return Content::Image;
    }
    if looks_binary(bytes) {
        return Content::Binary;
    }
    let lines = bytes.iter().filter(|&&b| b == b'\n').count();
    if !force && (bytes.len() > MAX_TEXT_BYTES || lines > MAX_TEXT_LINES) {
        return Content::TooLarge { bytes: bytes.len(), lines };
    }
    match std::str::from_utf8(bytes) {
        Ok(text) => Content::Text { text: text.to_string(), non_utf8: false },
        Err(_) => Content::Text { text: bytes.iter().map(|&b| b as char).collect(), non_utf8: true },
    }
}

/// Paths whose diffs are usually machine-generated and collapsed by default.
pub fn is_generated(path: &str) -> bool {
    let name = path.rsplit('/').next().unwrap_or(path);
    matches!(
        name,
        "composer.lock" | "package-lock.json" | "yarn.lock" | "pnpm-lock.yaml" | "Cargo.lock" | "bun.lockb"
    ) || name.ends_with(".min.js")
        || name.ends_with(".min.css")
        || name.ends_with(".map")
        || path.starts_with("public/build/")
        || path.contains("/_ide_helper")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies() {
        assert!(matches!(classify("a.php", b"<?php\n", false), Content::Text { non_utf8: false, .. }));
        assert_eq!(classify("a.bin", b"ab\0cd", false), Content::Binary);
        assert_eq!(classify("logo.PNG", b"whatever", false), Content::Image);
        assert!(matches!(classify("icon.svg", b"<svg/>", false), Content::Text { .. }));
        let lfs = b"version https://git-lfs.github.com/spec/v1\noid sha256:abc\nsize 42\n";
        assert_eq!(classify("big.psd", lfs, false), Content::LfsPointer { oid: "sha256:abc".into(), size: 42 });
        let latin1 = classify("a.txt", &[b'c', b'a', b'f', 0xe9], false);
        assert_eq!(latin1, Content::Text { text: "café".into(), non_utf8: true });
    }

    #[test]
    fn large_files_are_deferred_unless_forced() {
        let big = "x\n".repeat(MAX_TEXT_LINES + 1);
        assert!(matches!(classify("a.txt", big.as_bytes(), false), Content::TooLarge { .. }));
        assert!(matches!(classify("a.txt", big.as_bytes(), true), Content::Text { .. }));
    }

    #[test]
    fn generated_paths() {
        assert!(is_generated("composer.lock"));
        assert!(is_generated("public/build/app.js"));
        assert!(is_generated("resources/js/app.min.js"));
        assert!(!is_generated("app/Models/User.php"));
    }
}
