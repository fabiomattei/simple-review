//! A minimal Language Server Protocol client (stdio, JSON-RPC) for precise go-to-definition and
//! find-references, run against a commit's snapshot.
//!
//! Language servers load project code (autoloaders, plugins), so only use this for trusted
//! repositories.

use std::collections::HashSet;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use crossbeam_channel::{Receiver, RecvTimeoutError};
use serde_json::{Value, json};

/// A location returned by the server, as a filesystem path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LspLocation {
    pub path: PathBuf,
    /// 0-based line and byte column.
    pub line: u32,
    pub col: u32,
}

pub struct LspClient {
    child: Child,
    stdin: ChildStdin,
    messages: Receiver<Value>,
    next_id: i64,
    root: PathBuf,
    opened: HashSet<PathBuf>,
}

const REQUEST_TIMEOUT: Duration = Duration::from_secs(60);

/// Frame a JSON-RPC message with its `Content-Length` header.
pub fn encode(message: &Value) -> Vec<u8> {
    let body = message.to_string();
    let mut out = format!("Content-Length: {}\r\n\r\n", body.len()).into_bytes();
    out.extend(body.into_bytes());
    out
}

/// Read one framed message; `None` at end of stream.
pub fn decode(reader: &mut impl BufRead) -> Result<Option<Value>> {
    let mut length = None;
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line)? == 0 {
            return Ok(None);
        }
        let line = line.trim_end();
        if line.is_empty() {
            break;
        }
        if let Some(v) = line.strip_prefix("Content-Length:") {
            length = Some(v.trim().parse::<usize>()?);
        }
    }
    let length = length.context("message without Content-Length")?;
    let mut body = vec![0; length];
    reader.read_exact(&mut body)?;
    Ok(Some(serde_json::from_slice(&body)?))
}

pub fn path_to_uri(path: &Path) -> String {
    let mut uri = String::from("file://");
    let s = path.to_string_lossy().replace('\\', "/");
    if !s.starts_with('/') {
        uri.push('/');
    }
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || b"/-_.~:".contains(&b) {
            uri.push(b as char);
        } else {
            uri.push_str(&format!("%{b:02X}"));
        }
    }
    uri
}

pub fn uri_to_path(uri: &str) -> Option<PathBuf> {
    let rest = uri.strip_prefix("file://")?;
    let bytes = rest.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%'
            && i + 2 < bytes.len()
            && let Ok(b) = u8::from_str_radix(std::str::from_utf8(&bytes[i + 1..i + 3]).ok()?, 16)
        {
            out.push(b);
            i += 3;
            continue;
        }
        out.push(bytes[i]);
        i += 1;
    }
    let s = String::from_utf8(out).ok()?;
    // Windows: file:///C:/x
    let s = if s.len() > 2 && s.as_bytes()[2] == b':' { s[1..].to_string() } else { s };
    Some(PathBuf::from(s))
}

/// UTF-16 offset of a byte column (LSP's default position encoding).
pub fn byte_to_utf16(line: &str, byte: usize) -> u32 {
    line[..byte.min(line.len())].encode_utf16().count() as u32
}

pub fn utf16_to_byte(line: &str, units: u32) -> u32 {
    let mut count = 0;
    for (i, c) in line.char_indices() {
        if count >= units {
            return i as u32;
        }
        count += c.len_utf16() as u32;
    }
    line.len() as u32
}

impl LspClient {
    /// Start `cmd` in `root` and complete the initialize handshake.
    pub fn start(cmd: &[String], root: &Path) -> Result<Self> {
        if cmd.is_empty() {
            bail!("no language server command configured");
        }
        let mut child = Command::new(&cmd[0])
            .args(&cmd[1..])
            .current_dir(root)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .with_context(|| format!("starting language server `{}`", cmd.join(" ")))?;
        let stdin = child.stdin.take().expect("piped");
        let stdout = child.stdout.take().expect("piped");
        let (tx, messages) = crossbeam_channel::unbounded();
        std::thread::Builder::new().name("lsp-reader".into()).spawn(move || {
            let mut reader = BufReader::new(stdout);
            while let Ok(Some(msg)) = decode(&mut reader) {
                if tx.send(msg).is_err() {
                    break;
                }
            }
        })?;
        let mut client = LspClient { child, stdin, messages, next_id: 1, root: root.to_path_buf(), opened: HashSet::new() };
        let root_uri = path_to_uri(root);
        client.request(
            "initialize",
            json!({
                "processId": std::process::id(),
                "rootUri": root_uri,
                "workspaceFolders": [{ "uri": root_uri, "name": "snapshot" }],
                "capabilities": {
                    "textDocument": {
                        "definition": { "linkSupport": false },
                        "references": {},
                        "synchronization": { "didSave": false }
                    },
                    "workspace": { "configuration": true, "workspaceFolders": true }
                },
                "initializationOptions": {}
            }),
            Duration::from_secs(120),
        )?;
        client.notify("initialized", json!({}))?;
        Ok(client)
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    fn send(&mut self, message: &Value) -> Result<()> {
        self.stdin.write_all(&encode(message))?;
        self.stdin.flush()?;
        Ok(())
    }

    fn notify(&mut self, method: &str, params: Value) -> Result<()> {
        self.send(&json!({ "jsonrpc": "2.0", "method": method, "params": params }))
    }

    /// Send a request and wait for its response, answering the server's own requests meanwhile.
    pub fn request(&mut self, method: &str, params: Value, timeout: Duration) -> Result<Value> {
        let id = self.next_id;
        self.next_id += 1;
        self.send(&json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params }))?;
        let deadline = Instant::now() + timeout;
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            let msg = match self.messages.recv_timeout(left) {
                Ok(m) => m,
                Err(RecvTimeoutError::Timeout) => bail!("language server did not answer {method} in time"),
                Err(RecvTimeoutError::Disconnected) => bail!("language server exited"),
            };
            if msg.get("id").and_then(Value::as_i64) == Some(id) && msg.get("method").is_none() {
                if let Some(err) = msg.get("error") {
                    bail!("{method} failed: {}", err.get("message").and_then(Value::as_str).unwrap_or("error"));
                }
                return Ok(msg.get("result").cloned().unwrap_or(Value::Null));
            }
            // A request from the server (configuration, progress tokens…): answer with defaults.
            if let (Some(server_id), Some(m)) = (msg.get("id").cloned(), msg.get("method").and_then(Value::as_str)) {
                let result = if m == "workspace/configuration" {
                    let n = msg.pointer("/params/items").and_then(Value::as_array).map_or(0, Vec::len);
                    Value::Array(vec![Value::Null; n])
                } else {
                    Value::Null
                };
                self.send(&json!({ "jsonrpc": "2.0", "id": server_id, "result": result }))?;
            }
        }
    }

    fn open(&mut self, path: &Path) -> Result<()> {
        if self.opened.contains(path) {
            return Ok(());
        }
        let text = std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
        self.notify(
            "textDocument/didOpen",
            json!({ "textDocument": { "uri": path_to_uri(path), "languageId": "php", "version": 1, "text": text } }),
        )?;
        self.opened.insert(path.to_path_buf());
        Ok(())
    }

    fn position_params(&mut self, rel: &str, line: u32, byte_col: u32) -> Result<Value> {
        let path = self.root.join(rel);
        self.open(&path)?;
        let text = std::fs::read_to_string(&path)?;
        let line_text = text.lines().nth(line as usize).unwrap_or("");
        Ok(json!({
            "textDocument": { "uri": path_to_uri(&path) },
            "position": { "line": line, "character": byte_to_utf16(line_text, byte_col as usize) }
        }))
    }

    pub fn definition(&mut self, rel: &str, line: u32, byte_col: u32) -> Result<Vec<LspLocation>> {
        let params = self.position_params(rel, line, byte_col)?;
        let result = self.request("textDocument/definition", params, REQUEST_TIMEOUT)?;
        Ok(locations(&result))
    }

    pub fn references(&mut self, rel: &str, line: u32, byte_col: u32) -> Result<Vec<LspLocation>> {
        let mut params = self.position_params(rel, line, byte_col)?;
        params["context"] = json!({ "includeDeclaration": false });
        let result = self.request("textDocument/references", params, REQUEST_TIMEOUT)?;
        Ok(locations(&result))
    }

    pub fn shutdown(mut self) {
        let _ = self.request("shutdown", Value::Null, Duration::from_secs(5));
        let _ = self.notify("exit", Value::Null);
    }
}

impl Drop for LspClient {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Parse `Location | Location[] | LocationLink[] | null`, converting columns to bytes.
pub fn locations(result: &Value) -> Vec<LspLocation> {
    let items: Vec<&Value> = match result {
        Value::Array(a) => a.iter().collect(),
        Value::Null => Vec::new(),
        v => vec![v],
    };
    items
        .into_iter()
        .filter_map(|item| {
            let uri = item.get("uri").or(item.get("targetUri"))?.as_str()?;
            let range = item.get("range").or(item.get("targetSelectionRange"))?;
            let line = range.pointer("/start/line")?.as_u64()? as u32;
            let character = range.pointer("/start/character")?.as_u64()? as u32;
            let path = uri_to_path(uri)?;
            let col = std::fs::read_to_string(&path)
                .ok()
                .and_then(|t| t.lines().nth(line as usize).map(|l| utf16_to_byte(l, character)))
                .unwrap_or(character);
            Some(LspLocation { path, line, col })
        })
        .collect()
}

/// Generate Laravel IDE helper stubs in a snapshot when the package is installed (boots the app).
pub fn generate_ide_helper(dir: &Path) -> Result<bool> {
    if !dir.join("vendor/barryvdh/laravel-ide-helper").exists() || !dir.join("artisan").exists() {
        return Ok(false);
    }
    let status = Command::new("php")
        .args(["artisan", "ide-helper:generate", "--no-interaction"])
        .current_dir(dir)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()?;
    Ok(status.success())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frames_round_trip() {
        let msg = json!({ "jsonrpc": "2.0", "id": 1, "result": { "ok": "é" } });
        let bytes = encode(&msg);
        let mut reader = std::io::Cursor::new([bytes.clone(), bytes].concat());
        assert_eq!(decode(&mut reader).unwrap(), Some(msg.clone()));
        assert_eq!(decode(&mut reader).unwrap(), Some(msg));
        assert_eq!(decode(&mut reader).unwrap(), None);
    }

    #[test]
    fn uris_and_columns() {
        let p = Path::new("/tmp/my dir/a#b.php");
        let uri = path_to_uri(p);
        assert_eq!(uri, "file:///tmp/my%20dir/a%23b.php");
        assert_eq!(uri_to_path(&uri).unwrap(), p);
        let line = "$é = '😀'; foo();";
        let byte = line.find("foo").unwrap();
        let units = byte_to_utf16(line, byte);
        assert_eq!(utf16_to_byte(line, units) as usize, byte);
        assert!(units < byte as u32);
    }

    #[test]
    fn parses_location_shapes() {
        let single = json!({ "uri": "file:///nope/a.php", "range": { "start": { "line": 3, "character": 4 }, "end": { "line": 3, "character": 9 } } });
        assert_eq!(locations(&single)[0], LspLocation { path: "/nope/a.php".into(), line: 3, col: 4 });
        let links = json!([{ "targetUri": "file:///nope/b.php", "targetRange": {}, "targetSelectionRange": { "start": { "line": 1, "character": 0 } } }]);
        assert_eq!(locations(&links)[0].line, 1);
        assert!(locations(&Value::Null).is_empty());
    }
}
