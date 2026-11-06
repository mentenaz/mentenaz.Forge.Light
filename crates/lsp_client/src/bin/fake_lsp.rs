//! Scripted language server for manager tests. `fake_lsp crash` exits right
//! after `initialize`; otherwise it answers until `exit`.

use serde_json::{Value, json};
use std::io::{BufRead, Read, Write};

fn send(v: Value) {
    let body = serde_json::to_vec(&v).unwrap();
    let mut out = std::io::stdout().lock();
    write!(out, "Content-Length: {}\r\n\r\n", body.len()).unwrap();
    out.write_all(&body).unwrap();
    out.flush().unwrap();
}

fn main() {
    let crash = std::env::args().any(|a| a == "crash");
    let stdin = std::io::stdin();
    let mut input = stdin.lock();
    loop {
        let mut length = 0usize;
        loop {
            let mut line = String::new();
            if input.read_line(&mut line).unwrap() == 0 {
                return;
            }
            let line = line.trim();
            if line.is_empty() {
                break;
            }
            if let Some(v) = line.strip_prefix("Content-Length:") {
                length = v.trim().parse().unwrap();
            }
        }
        let mut body = vec![0; length];
        input.read_exact(&mut body).unwrap();
        let msg: Value = serde_json::from_slice(&body).unwrap();
        let id = msg.get("id").cloned();
        match msg["method"].as_str() {
            Some("initialize") => {
                send(json!({"jsonrpc":"2.0","id":id,"result":{
                    "capabilities":{}, "serverInfo":{"name":"fake","version":"1.0"}}}));
                if crash {
                    eprintln!("about to crash");
                    std::process::exit(1);
                }
                eprintln!("fake server up");
                send(json!({"jsonrpc":"2.0","method":"window/logMessage",
                    "params":{"type":3,"message":"hello from fake"}}));
                send(json!({"jsonrpc":"2.0","method":"$/progress","params":{
                    "token":"idx","value":{"kind":"begin","title":"Indexing","percentage":42}}}));
            }
            Some("shutdown") => send(json!({"jsonrpc":"2.0","id":id,"result":null})),
            Some("exit") => return,
            Some("textDocument/didOpen") => {
                eprintln!("recv textDocument/didOpen {}", msg["params"]);
                let doc = &msg["params"]["textDocument"];
                send(json!({"jsonrpc":"2.0","method":"textDocument/publishDiagnostics","params":{
                    "uri": doc["uri"], "version": doc["version"],
                    "diagnostics":[{"range":{"start":{"line":0,"character":1},"end":{"line":0,"character":3}},
                        "severity":1,"message":"bad thing","source":"fake","code":"E1"},
                        {"range":{"start":{"line":0,"character":0},"end":{"line":0,"character":1}},
                        "severity":2,"message":"meh"}]}}));
            }
            Some(m) if m.starts_with("textDocument/did") => {
                // Echo document traffic so tests can see what arrived.
                eprintln!("recv {m} {}", msg["params"]);
            }
            _ => {
                if id.is_some() {
                    send(json!({"jsonrpc":"2.0","id":id,"result":null}));
                }
            }
        }
    }
}
