//! In-process protocol test: drive [`super::serve`] over a duplex pipe
//! and assert on the real framed JSON-RPC it writes back.
//!
//! This exercises the codec and the transport, not just the handlers, so
//! it is what catches a regression like writing logs to stdout.

use std::time::Duration;

use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt, DuplexStream};

/// Wrap a JSON-RPC message in the `Content-Length` framing LSP uses.
fn frame(v: &Value) -> Vec<u8> {
    let body = serde_json::to_string(v).unwrap();
    format!("Content-Length: {}\r\n\r\n{body}", body.len()).into_bytes()
}

/// Read one framed message. Every read is bounded so a hang fails the
/// test instead of the suite.
async fn next_message(r: &mut DuplexStream) -> Value {
    let mut header = Vec::new();
    loop {
        let mut byte = [0u8; 1];
        let n = tokio::time::timeout(Duration::from_secs(5), r.read(&mut byte))
            .await
            .expect("timed out reading a header")
            .expect("stream closed mid-header");
        assert_eq!(n, 1, "stream closed mid-header");
        header.push(byte[0]);
        if header.ends_with(b"\r\n\r\n") {
            break;
        }
    }
    let header = String::from_utf8(header).unwrap();
    let len: usize = header
        .lines()
        .find_map(|l| l.strip_prefix("Content-Length: "))
        .expect("no Content-Length")
        .trim()
        .parse()
        .unwrap();
    let mut body = vec![0u8; len];
    tokio::time::timeout(Duration::from_secs(5), r.read_exact(&mut body))
        .await
        .expect("timed out reading a body")
        .expect("stream closed mid-body");
    serde_json::from_slice(&body).unwrap()
}

/// Read messages until one matches `method`, returning it.
async fn wait_for(r: &mut DuplexStream, method: &str) -> Value {
    for _ in 0..8 {
        let msg = next_message(r).await;
        if msg.get("method").and_then(Value::as_str) == Some(method) {
            return msg;
        }
    }
    panic!("never saw a {method} message");
}

/// Start a server on a duplex pair and return the client's ends.
fn start() -> (DuplexStream, DuplexStream) {
    let (client_to_server, server_in) = tokio::io::duplex(64 * 1024);
    let (server_out, server_from) = tokio::io::duplex(64 * 1024);
    tokio::spawn(async move { super::serve(server_in, server_out).await });
    (client_to_server, server_from)
}

fn initialize() -> Value {
    json!({
        "jsonrpc": "2.0", "id": 1, "method": "initialize",
        "params": { "capabilities": {} }
    })
}

fn did_open(uri: &str, text: &str) -> Value {
    json!({
        "jsonrpc": "2.0", "method": "textDocument/didOpen",
        "params": { "textDocument": {
            "uri": uri, "languageId": "nll", "version": 1, "text": text
        }}
    })
}

#[tokio::test]
async fn initialize_advertises_the_capabilities_we_implement() {
    let (mut tx, mut rx) = start();
    tx.write_all(&frame(&initialize())).await.unwrap();
    let reply = next_message(&mut rx).await;
    assert_eq!(reply["id"], 1, "{reply}");
    let caps = &reply["result"]["capabilities"];
    assert!(caps["documentSymbolProvider"].as_bool().unwrap());
    assert!(caps["definitionProvider"].as_bool().unwrap());
    assert!(caps["hoverProvider"].as_bool().unwrap());
    assert!(caps["documentFormattingProvider"].as_bool().unwrap());
    assert!(caps["completionProvider"].is_object());
    assert_eq!(caps["textDocumentSync"]["change"], 1, "full sync");
    assert_eq!(reply["result"]["serverInfo"]["name"], "nlink-lab lsp");
}

#[tokio::test]
async fn did_open_publishes_diagnostics_for_a_bad_buffer() {
    let (mut tx, mut rx) = start();
    tx.write_all(&frame(&initialize())).await.unwrap();
    let _ = next_message(&mut rx).await;
    tx.write_all(&frame(&did_open(
        "file:///tmp/bad.nll",
        "lab \"t\"\nnode a\nnode a\n",
    )))
    .await
    .unwrap();

    let msg = wait_for(&mut rx, "textDocument/publishDiagnostics").await;
    let diags = msg["params"]["diagnostics"].as_array().unwrap();
    assert!(!diags.is_empty(), "{msg}");
    assert_eq!(diags[0]["severity"], 1, "an error");
    assert_eq!(diags[0]["source"], "nlink-lab");
    assert!(diags[0]["code"].is_string(), "{}", diags[0]);
}

#[tokio::test]
async fn did_change_clears_diagnostics_once_the_buffer_is_valid() {
    let (mut tx, mut rx) = start();
    tx.write_all(&frame(&initialize())).await.unwrap();
    let _ = next_message(&mut rx).await;
    tx.write_all(&frame(&did_open(
        "file:///tmp/x.nll",
        "lab \"t\"\nnode a\nnode a\n",
    )))
    .await
    .unwrap();
    let first = wait_for(&mut rx, "textDocument/publishDiagnostics").await;
    assert!(
        !first["params"]["diagnostics"]
            .as_array()
            .unwrap()
            .is_empty()
    );

    let good = concat!(
        "lab \"t\" {\n  description \"clean\"\n}\n",
        "node a\nnode b\n",
        "link a:eth0 -- b:eth0 { 10.0.0.1/24 -- 10.0.0.2/24 }\n",
        "validate { reach a b }\n",
    );
    tx.write_all(&frame(&json!({
        "jsonrpc": "2.0", "method": "textDocument/didChange",
        "params": {
            "textDocument": { "uri": "file:///tmp/x.nll", "version": 2 },
            "contentChanges": [{ "text": good }]
        }
    })))
    .await
    .unwrap();
    let second = wait_for(&mut rx, "textDocument/publishDiagnostics").await;
    assert_eq!(
        second["params"]["diagnostics"].as_array().unwrap().len(),
        0,
        "{second}"
    );
}

#[tokio::test]
async fn did_close_publishes_an_empty_list() {
    let (mut tx, mut rx) = start();
    tx.write_all(&frame(&initialize())).await.unwrap();
    let _ = next_message(&mut rx).await;
    tx.write_all(&frame(&did_open(
        "file:///tmp/c.nll",
        "lab \"t\"\nnode a\nnode a\n",
    )))
    .await
    .unwrap();
    let _ = wait_for(&mut rx, "textDocument/publishDiagnostics").await;
    tx.write_all(&frame(&json!({
        "jsonrpc": "2.0", "method": "textDocument/didClose",
        "params": { "textDocument": { "uri": "file:///tmp/c.nll" } }
    })))
    .await
    .unwrap();
    let msg = wait_for(&mut rx, "textDocument/publishDiagnostics").await;
    assert_eq!(msg["params"]["diagnostics"].as_array().unwrap().len(), 0);
}

#[tokio::test]
async fn document_symbol_returns_the_declarations() {
    let (mut tx, mut rx) = start();
    tx.write_all(&frame(&initialize())).await.unwrap();
    let _ = next_message(&mut rx).await;
    tx.write_all(&frame(&did_open(
        "file:///tmp/s.nll",
        "lab \"t\"\nnode a\nnode b\n",
    )))
    .await
    .unwrap();
    let _ = wait_for(&mut rx, "textDocument/publishDiagnostics").await;
    tx.write_all(&frame(&json!({
        "jsonrpc": "2.0", "id": 2, "method": "textDocument/documentSymbol",
        "params": { "textDocument": { "uri": "file:///tmp/s.nll" } }
    })))
    .await
    .unwrap();
    for _ in 0..8 {
        let msg = next_message(&mut rx).await;
        if msg.get("id").and_then(Value::as_i64) == Some(2) {
            let names: Vec<_> = msg["result"]
                .as_array()
                .unwrap()
                .iter()
                .map(|s| s["name"].as_str().unwrap().to_string())
                .collect();
            assert_eq!(names, vec!["t", "a", "b"]);
            return;
        }
    }
    panic!("no reply to the documentSymbol request");
}

#[tokio::test]
async fn stdin_eof_ends_the_server() {
    let (client_to_server, server_in) = tokio::io::duplex(1024);
    let (server_out, _rx) = tokio::io::duplex(1024);
    let task = tokio::spawn(async move { super::serve(server_in, server_out).await });
    drop(client_to_server);
    tokio::time::timeout(Duration::from_secs(5), task)
        .await
        .expect("serve did not return on EOF")
        .unwrap();
}
