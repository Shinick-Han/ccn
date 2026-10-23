//! Minimal stdio Language Server exposing per-function CCN.
//!
//! No external protocol crate: the base protocol is just
//! `Content-Length: N\r\n\r\n<json>` and the handful of methods below. The
//! server is intentionally stateless beyond open documents; every request
//! re-analyses the current text with the same `crate::ccn` dispatcher the CLI
//! uses, so editor numbers can never drift from report numbers.

use std::collections::HashMap;
use std::io::{self, BufRead, BufReader, Write};
use std::path::PathBuf;

use serde_json::{json, Value};

use crate::ccn::{analyze_file, FunctionCcn, Lang};

/// The project CCN budget. Diagnostics start above this value.
const CCN_LIMIT: u32 = 10;
/// Above this value the diagnostic is an error instead of a warning.
const CCN_ERROR: u32 = 20;
/// Refuse absurd bodies rather than trying to allocate them.
const MAX_CONTENT_LENGTH: usize = 64 * 1024 * 1024;

/// Encode one JSON-RPC value with LSP base-protocol framing.
pub fn encode_message(value: &Value) -> Vec<u8> {
    let body = serde_json::to_vec(value).unwrap_or_else(|_| b"{}".to_vec());
    let mut out = format!("Content-Length: {}\r\n\r\n", body.len()).into_bytes();
    out.extend_from_slice(&body);
    out
}

/// Read one framed message.
///
/// Returns `Ok(None)` on EOF (including a stream that ends mid-header).
/// Missing or invalid framing, oversized bodies and invalid JSON are
/// reported as `InvalidData` errors; no input can panic.
pub fn read_message<R: BufRead>(reader: &mut R) -> io::Result<Option<Value>> {
    let mut content_length: Option<usize> = None;

    loop {
        let mut line = String::new();
        let read = reader.read_line(&mut line)?;
        if read == 0 {
            return Ok(None);
        }
        if line.len() > 64 * 1024 {
            return Err(invalid_data("header line too long"));
        }
        let header = line.trim_end_matches(['\r', '\n']);
        if header.is_empty() {
            break;
        }
        let Some((name, value)) = header.split_once(':') else {
            // Unknown or malformed header: ignore it, the framing still
            // depends only on Content-Length.
            continue;
        };
        if name.trim().eq_ignore_ascii_case("content-length") {
            let parsed = value
                .trim()
                .parse::<usize>()
                .map_err(|_| invalid_data("invalid Content-Length"))?;
            if parsed > MAX_CONTENT_LENGTH {
                return Err(invalid_data("Content-Length too large"));
            }
            content_length = Some(parsed);
        }
    }

    let length = content_length.ok_or_else(|| invalid_data("missing Content-Length"))?;
    let mut body = vec![0u8; length];
    reader.read_exact(&mut body)?;
    let value = serde_json::from_slice(&body).map_err(|_| invalid_data("invalid JSON body"))?;
    Ok(Some(value))
}

fn invalid_data(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

/// One open document, always full-sync.
struct Document {
    text: String,
    version: Option<i64>,
    path: PathBuf,
    lang: Option<Lang>,
}

/// The protocol state. Public so tests (and embedders) can drive it without
/// touching stdio.
pub struct Server {
    documents: HashMap<String, Document>,
    exited: bool,
}

impl Default for Server {
    fn default() -> Self {
        Self::new()
    }
}

impl Server {
    pub fn new() -> Self {
        Self {
            documents: HashMap::new(),
            exited: false,
        }
    }

    /// True after an `exit` notification. `run` polls this after each message.
    pub fn exited(&self) -> bool {
        self.exited
    }

    /// Handle one incoming message, returning every outgoing message in order.
    pub fn handle(&mut self, msg: Value) -> Vec<Value> {
        let Some(method) = msg.get("method").and_then(Value::as_str) else {
            // A response from the client (or junk): nothing to do.
            return Vec::new();
        };
        let id = msg.get("id").cloned().filter(|id| !id.is_null());
        let params = msg.get("params").cloned().unwrap_or(Value::Null);

        match method {
            "initialize" => respond(id, initialize_result()),
            "initialized" => Vec::new(),
            "shutdown" => respond(id, Value::Null),
            "exit" => {
                self.exited = true;
                Vec::new()
            }
            "textDocument/didOpen" => self.did_open(&params),
            "textDocument/didChange" => self.did_change(&params),
            "textDocument/didClose" => self.did_close(&params),
            "textDocument/hover" => respond(id, self.hover(&params)),
            "textDocument/documentSymbol" => respond(id, self.document_symbols(&params)),
            // Unknown notifications are ignored. Unknown requests get a null
            // result, which is safer for clients than an error reply here.
            _ => respond(id, Value::Null),
        }
    }

    fn did_open(&mut self, params: &Value) -> Vec<Value> {
        let Some(document) = params.get("textDocument") else {
            return Vec::new();
        };
        let Some(uri) = document.get("uri").and_then(Value::as_str) else {
            return Vec::new();
        };
        let Some(text) = document.get("text").and_then(Value::as_str) else {
            return Vec::new();
        };
        let version = document.get("version").and_then(Value::as_i64);
        let path = uri_to_path(uri).unwrap_or_else(|| PathBuf::from(uri));
        let path_text = path.to_string_lossy();
        let lang = Lang::from_path(path_text.as_ref());

        self.documents.insert(
            uri.to_string(),
            Document {
                text: text.to_string(),
                version,
                path,
                lang,
            },
        );

        let diagnostics = self.diagnostics(uri);
        vec![publish_diagnostics(uri, version, diagnostics)]
    }

    fn did_change(&mut self, params: &Value) -> Vec<Value> {
        let Some(uri) = params
            .pointer("/textDocument/uri")
            .and_then(Value::as_str)
            .map(str::to_string)
        else {
            return Vec::new();
        };
        let version = params
            .pointer("/textDocument/version")
            .and_then(Value::as_i64);
        // Full sync: take the last change's complete text.
        let text = params
            .pointer("/contentChanges")
            .and_then(Value::as_array)
            .and_then(|changes| changes.last())
            .and_then(|change| change.get("text"))
            .and_then(Value::as_str)
            .map(str::to_string);

        let Some(document) = self.documents.get_mut(&uri) else {
            return Vec::new();
        };
        if let Some(text) = text {
            document.text = text;
        }
        if version.is_some() {
            document.version = version;
        }

        let published_version = self.documents.get(&uri).and_then(|doc| doc.version);
        let diagnostics = self.diagnostics(&uri);
        vec![publish_diagnostics(&uri, published_version, diagnostics)]
    }

    fn did_close(&mut self, params: &Value) -> Vec<Value> {
        let Some(uri) = params
            .pointer("/textDocument/uri")
            .and_then(Value::as_str)
            .map(str::to_string)
        else {
            return Vec::new();
        };
        let version = self
            .documents
            .remove(&uri)
            .and_then(|document| document.version);
        vec![publish_diagnostics(&uri, version, Vec::new())]
    }

    /// Diagnostics for one open document; empty when the language is unknown.
    fn diagnostics(&self, uri: &str) -> Vec<Value> {
        let Some(document) = self.documents.get(uri) else {
            return Vec::new();
        };
        let Some(functions) = functions_for(document) else {
            return Vec::new();
        };

        functions
            .iter()
            .enumerate()
            .filter(|(_, function)| function.ccn > CCN_LIMIT)
            .map(|(index, function)| {
                let range = function_range(&document.text, &functions, index);
                let severity = if function.ccn > CCN_ERROR { 1 } else { 2 };
                json!({
                    "range": range,
                    "severity": severity,
                    "source": "ccn",
                    "message": format!(
                        "CCN {} (limit {}) \u{00b7} nesting {} \u{00b7} {} LOC",
                        function.ccn, CCN_LIMIT, function.max_nesting, function.nloc
                    ),
                })
            })
            .collect()
    }

    fn hover(&self, params: &Value) -> Value {
        let Some(uri) = params.pointer("/textDocument/uri").and_then(Value::as_str) else {
            return Value::Null;
        };
        let line = params
            .pointer("/position/line")
            .and_then(Value::as_u64)
            .unwrap_or(0) as usize;
        let Some(document) = self.documents.get(uri) else {
            return Value::Null;
        };
        let Some(functions) = functions_for(document) else {
            return Value::Null;
        };

        // Start line is 1-based in the analysis, 0-based in LSP.
        let Some(index) = functions
            .iter()
            .rposition(|function| function.line.saturating_sub(1) <= line)
        else {
            return Value::Null;
        };
        let function = &functions[index];
        let value = format!(
            "**{}** \u{2014} CCN {} \u{00b7} nesting {} \u{00b7} LOC {}",
            function.name, function.ccn, function.max_nesting, function.nloc
        );

        json!({
            "contents": { "kind": "markdown", "value": value },
            "range": function_range(&document.text, &functions, index),
        })
    }

    fn document_symbols(&self, params: &Value) -> Value {
        let Some(uri) = params.pointer("/textDocument/uri").and_then(Value::as_str) else {
            return Value::Array(Vec::new());
        };
        let Some(document) = self.documents.get(uri) else {
            return Value::Array(Vec::new());
        };
        let Some(functions) = functions_for(document) else {
            return Value::Array(Vec::new());
        };

        let symbols: Vec<Value> = functions
            .iter()
            .enumerate()
            .map(|(index, function)| {
                let range = function_range(&document.text, &functions, index);
                let selection = range.clone();
                let identity = if function.identity.is_empty() {
                    function.name.as_str()
                } else {
                    function.identity.as_str()
                };
                json!({
                    "name": function.name,
                    "detail": format!("{} \u{00b7} CCN {}", identity, function.ccn),
                    "kind": 12,
                    "range": range,
                    "selectionRange": selection,
                })
            })
            .collect();
        Value::Array(symbols)
    }
}

fn initialize_result() -> Value {
    json!({
        "capabilities": {
            "textDocumentSync": 1,
            "hoverProvider": true,
            "documentSymbolProvider": true,
        },
        "serverInfo": {
            "name": "ccn",
            "version": env!("CARGO_PKG_VERSION"),
        },
    })
}

fn respond(id: Option<Value>, result: Value) -> Vec<Value> {
    match id {
        Some(id) => vec![json!({ "jsonrpc": "2.0", "id": id, "result": result })],
        None => Vec::new(),
    }
}

fn publish_diagnostics(uri: &str, version: Option<i64>, diagnostics: Vec<Value>) -> Value {
    json!({
        "jsonrpc": "2.0",
        "method": "textDocument/publishDiagnostics",
        "params": {
            "uri": uri,
            "version": version,
            "diagnostics": diagnostics,
        },
    })
}

/// Analyse one document, returning its functions sorted by source position.
/// `None` means the extension is not a language ccn can measure.
fn functions_for(document: &Document) -> Option<Vec<FunctionCcn>> {
    let lang = document.lang?;
    let summary = analyze_file(&document.path, document.text.as_bytes(), lang.name());
    let mut functions = summary.detail;
    functions.sort_by(|a, b| a.line.cmp(&b.line).then(a.end_line.cmp(&b.end_line)));
    Some(functions)
}

/// A function's range: from its declaration line to the line before the next
/// function, or the end of the document for the last one.
fn function_range(text: &str, functions: &[FunctionCcn], index: usize) -> Value {
    let start = functions[index].line.saturating_sub(1);
    let last = last_line(text);
    let end = match functions.get(index + 1) {
        Some(next) => next.line.saturating_sub(2),
        None => last,
    }
    .max(start)
    .min(last.max(start));

    json!({
        "start": { "line": start, "character": 0 },
        "end": { "line": end, "character": line_utf16_len(text, end) },
    })
}

fn line_utf16_len(text: &str, line: usize) -> usize {
    text.split('\n')
        .nth(line)
        .map(|content| content.trim_end_matches('\r').encode_utf16().count())
        .unwrap_or(0)
}

fn last_line(text: &str) -> usize {
    text.split('\n').count().saturating_sub(1)
}

/// Convert a `file://` URI to a local path, percent-decoding escapes and
/// dropping the leading slash before a Windows drive letter.
fn uri_to_path(uri: &str) -> Option<PathBuf> {
    let rest = uri.strip_prefix("file://")?;
    let path_part = rest.split_once('?').map_or(rest, |(path, _)| path);
    let path_part = path_part
        .split_once('#')
        .map_or(path_part, |(path, _)| path);
    let decoded = percent_decode(path_part);
    let normalized = if cfg!(windows)
        && decoded.len() >= 3
        && decoded.as_bytes()[0] == b'/'
        && decoded.as_bytes()[1].is_ascii_alphabetic()
        && decoded.as_bytes()[2] == b':'
    {
        decoded[1..].to_string()
    } else {
        decoded
    };
    Some(PathBuf::from(normalized))
}

fn percent_decode(input: &str) -> String {
    let bytes = input.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' && index + 2 < bytes.len() {
            if let (Some(high), Some(low)) =
                (hex_value(bytes[index + 1]), hex_value(bytes[index + 2]))
            {
                out.push(high * 16 + low);
                index += 3;
                continue;
            }
        }
        out.push(bytes[index]);
        index += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn hex_value(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

/// Blocking stdio server. Returns 0 on `exit` or EOF.
pub fn run() -> i32 {
    let stdin = io::stdin();
    let stdout = io::stdout();
    let mut reader = BufReader::new(stdin.lock());
    let mut writer = stdout.lock();
    let mut server = Server::new();

    loop {
        match read_message(&mut reader) {
            Ok(Some(message)) => {
                for response in server.handle(message) {
                    if writer.write_all(&encode_message(&response)).is_err() {
                        return 1;
                    }
                }
                if writer.flush().is_err() {
                    return 1;
                }
                if server.exited() {
                    return 0;
                }
            }
            Ok(None) => return 0,
            Err(_) => return 1,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    const URI: &str = "file:///C:/tmp/sample.c";

    fn branchy_c() -> &'static str {
        "int complex(int a, int b) {\n\
         \x20   int r = 0;\n\
         \x20   if (a > 0) { r += 1; }\n\
         \x20   if (b > 0) { r += 1; }\n\
         \x20   if (a > 1) { r += 1; }\n\
         \x20   if (b > 1) { r += 1; }\n\
         \x20   if (a > 2) { r += 1; }\n\
         \x20   if (b > 2) { r += 1; }\n\
         \x20   if (a > 3) { r += 1; }\n\
         \x20   if (b > 3) { r += 1; }\n\
         \x20   if (a > 4) { r += 1; }\n\
         \x20   if (b > 4) { r += 1; }\n\
         \x20   if (a > 5) { r += 1; }\n\
         \x20   if (b > 5) { r += 1; }\n\
         \x20   return r;\n\
         }\n\
         \n\
         int simple(void) {\n\
         \x20   return 1;\n\
         }\n"
    }

    fn open(server: &mut Server, uri: &str, text: &str) -> Vec<Value> {
        server.handle(json!({
            "jsonrpc": "2.0",
            "method": "textDocument/didOpen",
            "params": {
                "textDocument": {
                    "uri": uri,
                    "languageId": "c",
                    "version": 1,
                    "text": text,
                }
            }
        }))
    }

    #[test]
    fn framing_round_trip() {
        let value = json!({ "jsonrpc": "2.0", "id": 1, "method": "shutdown" });
        let encoded = encode_message(&value);
        let mut reader = Cursor::new(encoded);
        let decoded = read_message(&mut reader).unwrap().unwrap();
        assert_eq!(decoded, value);
        assert_eq!(read_message(&mut reader).unwrap(), None);
    }

    #[test]
    fn framing_reads_multiple_messages() {
        let first = json!({ "jsonrpc": "2.0", "method": "initialized" });
        let second = json!({ "jsonrpc": "2.0", "id": 2, "method": "shutdown" });
        let mut bytes = encode_message(&first);
        bytes.extend(encode_message(&second));
        let mut reader = Cursor::new(bytes);
        assert_eq!(read_message(&mut reader).unwrap(), Some(first));
        assert_eq!(read_message(&mut reader).unwrap(), Some(second));
        assert_eq!(read_message(&mut reader).unwrap(), None);
    }

    #[test]
    fn framing_ignores_unknown_headers_and_is_case_insensitive() {
        let body = br#"{"jsonrpc":"2.0","id":7,"method":"shutdown"}"#;
        let framed = format!(
            "content-length: {}\r\nContent-Type: application/vscode-jsonrpc\r\n\r\n",
            body.len()
        );
        let mut bytes = framed.into_bytes();
        bytes.extend_from_slice(body);
        let mut reader = Cursor::new(bytes);
        let value = read_message(&mut reader).unwrap().unwrap();
        assert_eq!(value["id"], 7);
    }

    #[test]
    fn malformed_framing_errors_without_panicking() {
        let cases: &[&[u8]] = &[
            b"Content-Length: nope\r\n\r\n{}",
            b"Bad-Header\r\n\r\n{}",
            b"\r\n{}",
            b"Content-Length: 4\r\n\r\nnope",
        ];
        for case in cases {
            let mut reader = Cursor::new(*case);
            assert!(read_message(&mut reader).is_err(), "case: {case:?}");
        }
    }

    #[test]
    fn framing_eof_is_none() {
        let mut empty = Cursor::new(Vec::new());
        assert_eq!(read_message(&mut empty).unwrap(), None);
        let mut partial = Cursor::new(b"Content-Length: 10\r\n".to_vec());
        assert_eq!(read_message(&mut partial).unwrap(), None);
    }

    #[test]
    fn initialize_advertises_capabilities() {
        let mut server = Server::new();
        let out = server.handle(json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": { "rootUri": "file:///C:/tmp" }
        }));
        assert_eq!(out.len(), 1);
        let result = &out[0]["result"];
        assert_eq!(result["capabilities"]["textDocumentSync"], 1);
        assert_eq!(result["capabilities"]["hoverProvider"], true);
        assert_eq!(result["capabilities"]["documentSymbolProvider"], true);
        assert_eq!(result["serverInfo"]["name"], "ccn");
        assert_eq!(result["serverInfo"]["version"], env!("CARGO_PKG_VERSION"));
    }

    #[test]
    fn did_open_publishes_high_ccn_diagnostic() {
        let mut server = Server::new();
        let out = open(&mut server, URI, branchy_c());
        assert_eq!(out.len(), 1);
        let params = &out[0]["params"];
        assert_eq!(params["uri"], URI);
        assert_eq!(params["version"], 1);
        let diagnostics = params["diagnostics"].as_array().unwrap();
        assert_eq!(diagnostics.len(), 1, "{diagnostics:?}");
        assert_eq!(diagnostics[0]["severity"], 2);
        assert_eq!(diagnostics[0]["source"], "ccn");
        let message = diagnostics[0]["message"].as_str().unwrap();
        assert!(message.starts_with("CCN "), "{message}");
        assert!(message.contains("(limit 10)"), "{message}");
        assert_eq!(diagnostics[0]["range"]["start"]["line"], 0);
    }

    #[test]
    fn did_close_publishes_empty_diagnostics() {
        let mut server = Server::new();
        open(&mut server, URI, branchy_c());
        let out = server.handle(json!({
            "jsonrpc": "2.0",
            "method": "textDocument/didClose",
            "params": { "textDocument": { "uri": URI } }
        }));
        assert_eq!(out.len(), 1);
        assert_eq!(out[0]["params"]["diagnostics"], json!([]));
    }

    #[test]
    fn hover_reports_nearest_function() {
        let mut server = Server::new();
        open(&mut server, URI, branchy_c());
        let out = server.handle(json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "textDocument/hover",
            "params": {
                "textDocument": { "uri": URI },
                "position": { "line": 1, "character": 4 }
            }
        }));
        let result = &out[0]["result"];
        assert_eq!(result["contents"]["kind"], "markdown");
        let value = result["contents"]["value"].as_str().unwrap();
        assert!(value.starts_with("**complex**"), "{value}");
        assert!(value.contains("CCN "), "{value}");
        assert_eq!(result["range"]["start"]["line"], 0);
    }

    #[test]
    fn hover_before_any_function_is_null() {
        let mut server = Server::new();
        open(&mut server, "file:///C:/tmp/empty.c", "int x;\n");
        let out = server.handle(json!({
            "jsonrpc": "2.0",
            "id": 3,
            "method": "textDocument/hover",
            "params": {
                "textDocument": { "uri": "file:///C:/tmp/empty.c" },
                "position": { "line": 0, "character": 0 }
            }
        }));
        assert_eq!(out[0]["result"], Value::Null);
    }

    #[test]
    fn document_symbols_include_functions_and_lines() {
        let mut server = Server::new();
        open(&mut server, URI, branchy_c());
        let out = server.handle(json!({
            "jsonrpc": "2.0",
            "id": 4,
            "method": "textDocument/documentSymbol",
            "params": { "textDocument": { "uri": URI } }
        }));
        let symbols = out[0]["result"].as_array().unwrap();
        assert_eq!(symbols.len(), 2, "{symbols:?}");
        assert_eq!(symbols[0]["name"], "complex");
        assert_eq!(symbols[0]["kind"], 12);
        assert_eq!(symbols[0]["range"]["start"]["line"], 0);
        assert!(symbols[0]["detail"].as_str().unwrap().contains("CCN "));
        assert_eq!(symbols[1]["name"], "simple");
        assert!(symbols[1]["range"]["start"]["line"].as_u64().unwrap() > 0);
    }

    #[test]
    fn unknown_language_is_silent_and_does_not_panic() {
        let mut server = Server::new();
        let uri = "file:///C:/tmp/script.kt";
        let out = open(&mut server, uri, "fun main() { if (true) {} }\n");
        assert_eq!(out.len(), 1);
        assert_eq!(out[0]["params"]["diagnostics"], json!([]));

        let symbols = server.handle(json!({
            "jsonrpc": "2.0",
            "id": 5,
            "method": "textDocument/documentSymbol",
            "params": { "textDocument": { "uri": uri } }
        }));
        assert_eq!(symbols[0]["result"], json!([]));

        let hover = server.handle(json!({
            "jsonrpc": "2.0",
            "id": 6,
            "method": "textDocument/hover",
            "params": {
                "textDocument": { "uri": uri },
                "position": { "line": 0, "character": 0 }
            }
        }));
        assert_eq!(hover[0]["result"], Value::Null);
    }

    #[test]
    fn did_change_replaces_text_and_republishes() {
        let mut server = Server::new();
        open(&mut server, URI, "int simple(void) { return 1; }\n");
        let out = server.handle(json!({
            "jsonrpc": "2.0",
            "method": "textDocument/didChange",
            "params": {
                "textDocument": { "uri": URI, "version": 2 },
                "contentChanges": [ { "text": branchy_c() } ]
            }
        }));
        assert_eq!(out[0]["params"]["version"], 2);
        assert_eq!(out[0]["params"]["diagnostics"].as_array().unwrap().len(), 1);
    }

    #[test]
    fn unknown_request_gets_null_result_and_unknown_notification_is_ignored() {
        let mut server = Server::new();
        let request = server.handle(json!({
            "jsonrpc": "2.0",
            "id": 9,
            "method": "workspace/unknown",
            "params": {}
        }));
        assert_eq!(request.len(), 1);
        assert_eq!(request[0]["result"], Value::Null);

        let notification = server.handle(json!({
            "jsonrpc": "2.0",
            "method": "workspace/unknown",
            "params": {}
        }));
        assert!(notification.is_empty());
    }

    #[test]
    fn exit_sets_exited() {
        let mut server = Server::new();
        assert!(!server.exited());
        server.handle(json!({ "jsonrpc": "2.0", "method": "exit" }));
        assert!(server.exited());
    }

    #[test]
    fn uri_to_path_decodes_and_normalizes_windows_drives() {
        let path = uri_to_path("file:///C:/Program%20Files/a%2Bb.c").unwrap();
        #[cfg(windows)]
        assert_eq!(path.to_string_lossy(), "C:/Program Files/a+b.c");
        #[cfg(not(windows))]
        assert_eq!(path.to_string_lossy(), "/C:/Program Files/a+b.c");
        assert_eq!(uri_to_path("untitled:Untitled-1"), None);
    }
}
