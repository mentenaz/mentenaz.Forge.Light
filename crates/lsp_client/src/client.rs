//! JSON-RPC client: request/response matching, server requests, events, lifecycle.

use crate::transport::{read_message, write_message};
use lsp_types::notification::Notification;
use lsp_types::request::Request;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};
use std::collections::HashMap;
use std::path::Path;
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::io::{AsyncBufRead, AsyncRead, AsyncWrite, BufReader};
use tokio::runtime::Handle;
use tokio::sync::{mpsc, oneshot};

const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
const SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(3);

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("server connection closed")]
    Closed,
    #[error("request timed out")]
    Timeout,
    #[error("server error {code}: {message}")]
    Server { code: i64, message: String },
    #[error("bad payload: {0}")]
    Payload(#[from] serde_json::Error),
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
}

/// Things the server tells us unprompted.
#[derive(Debug)]
pub enum Event {
    Diagnostics(lsp_types::PublishDiagnosticsParams),
    Progress(lsp_types::ProgressParams),
    ShowMessage(lsp_types::ShowMessageParams),
    LogMessage(lsp_types::LogMessageParams),
    Stderr(String),
    /// The process or connection ended; carries the exit code when known.
    Exited(Option<i32>),
}

type Pending = HashMap<i64, oneshot::Sender<Result<Value, Error>>>;

struct Inner {
    next_id: AtomicI64,
    pending: Mutex<Pending>,
    out: mpsc::UnboundedSender<Value>,
    events: mpsc::UnboundedSender<Event>,
    exit_reported: AtomicBool,
    workspace_folders: Mutex<Value>,
    kill: Mutex<Option<oneshot::Sender<()>>>,
}

#[derive(Clone)]
pub struct Client {
    inner: Arc<Inner>,
}

impl Inner {
    fn report_exit(&self, code: Option<i32>) {
        if !self.exit_reported.swap(true, Ordering::SeqCst) {
            let _ = self.events.send(Event::Exited(code));
        }
    }

    fn fail_pending(&self) {
        for (_, tx) in self.pending.lock().unwrap().drain() {
            let _ = tx.send(Err(Error::Closed));
        }
    }
}

/// Cancels the request if the caller drops the future before a reply arrives.
struct Cancel<'a> {
    inner: &'a Inner,
    id: i64,
    armed: bool,
}

impl Drop for Cancel<'_> {
    fn drop(&mut self) {
        if self.armed && self.inner.pending.lock().unwrap().remove(&self.id).is_some() {
            let _ = self.inner.out.send(
                json!({"jsonrpc":"2.0","method":"$/cancelRequest","params":{"id":self.id}}),
            );
        }
    }
}

impl Client {
    /// Spawns a server process and connects to its stdio.
    pub fn spawn(
        handle: &Handle,
        command: &Path,
        args: &[String],
        cwd: Option<&Path>,
    ) -> Result<(Client, mpsc::UnboundedReceiver<Event>), Error> {
        let mut cmd = tokio::process::Command::new(command);
        cmd.args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        if let Some(cwd) = cwd {
            cmd.current_dir(cwd);
        }
        #[cfg(windows)]
        cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
        // Spawning registers with the reactor, which needs a runtime context.
        let _guard = handle.enter();
        let mut child = cmd.spawn()?;
        let stdin = child.stdin.take().expect("piped");
        let stdout = child.stdout.take().expect("piped");
        let stderr = child.stderr.take().expect("piped");

        let (client, events) = Self::connect(handle, BufReader::new(stdout), stdin, false);

        let inner = client.inner.clone();
        handle.spawn(async move {
            use tokio::io::AsyncBufReadExt;
            let mut lines = BufReader::new(stderr).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                let _ = inner.events.send(Event::Stderr(line));
            }
        });

        let (kill_tx, kill_rx) = oneshot::channel();
        *client.inner.kill.lock().unwrap() = Some(kill_tx);
        let inner = client.inner.clone();
        handle.spawn(async move {
            let status = tokio::select! {
                s = child.wait() => s.ok(),
                _ = kill_rx => {
                    let _ = child.kill().await;
                    child.wait().await.ok()
                }
            };
            inner.fail_pending();
            inner.report_exit(status.and_then(|s| s.code()));
        });
        Ok((client, events))
    }

    /// Connects over arbitrary streams (used directly by tests). With
    /// `report_eof_as_exit`, reader EOF emits `Event::Exited`.
    pub fn connect<R, W>(
        handle: &Handle,
        reader: R,
        writer: W,
        report_eof_as_exit: bool,
    ) -> (Client, mpsc::UnboundedReceiver<Event>)
    where
        R: AsyncBufRead + AsyncRead + Unpin + Send + 'static,
        W: AsyncWrite + Unpin + Send + 'static,
    {
        let (out_tx, mut out_rx) = mpsc::unbounded_channel::<Value>();
        let (ev_tx, ev_rx) = mpsc::unbounded_channel();
        let inner = Arc::new(Inner {
            next_id: AtomicI64::new(1),
            pending: Mutex::new(HashMap::new()),
            out: out_tx,
            events: ev_tx,
            exit_reported: AtomicBool::new(false),
            workspace_folders: Mutex::new(Value::Null),
            kill: Mutex::new(None),
        });

        handle.spawn(async move {
            let mut writer = writer;
            while let Some(msg) = out_rx.recv().await {
                let Ok(body) = serde_json::to_vec(&msg) else { continue };
                if write_message(&mut writer, &body).await.is_err() {
                    break;
                }
            }
        });

        let reader_inner = inner.clone();
        handle.spawn(async move {
            let mut reader = reader;
            while let Ok(Some(body)) = read_message(&mut reader).await {
                if let Ok(msg) = serde_json::from_slice::<Value>(&body) {
                    dispatch(&reader_inner, msg);
                }
            }
            reader_inner.fail_pending();
            if report_eof_as_exit {
                reader_inner.report_exit(None);
            }
        });

        (Client { inner }, ev_rx)
    }

    pub async fn request<R: Request>(&self, params: R::Params) -> Result<R::Result, Error> {
        let value = self.request_raw(R::METHOD, serde_json::to_value(params)?).await?;
        Ok(serde_json::from_value(value)?)
    }

    pub async fn request_raw(&self, method: &str, params: Value) -> Result<Value, Error> {
        let inner = &*self.inner;
        let id = inner.next_id.fetch_add(1, Ordering::SeqCst);
        let (tx, rx) = oneshot::channel();
        inner.pending.lock().unwrap().insert(id, tx);
        let mut cancel = Cancel { inner, id, armed: true };
        inner
            .out
            .send(json!({"jsonrpc":"2.0","id":id,"method":method,"params":params}))
            .map_err(|_| Error::Closed)?;
        match tokio::time::timeout(REQUEST_TIMEOUT, rx).await {
            Ok(Ok(reply)) => {
                cancel.armed = false;
                reply
            }
            Ok(Err(_)) => {
                cancel.armed = false;
                Err(Error::Closed)
            }
            // `cancel` drops here and sends $/cancelRequest.
            Err(_) => Err(Error::Timeout),
        }
    }

    pub fn notify<N: Notification>(&self, params: N::Params) -> Result<(), Error> {
        self.notify_raw(N::METHOD, serde_json::to_value(params)?)
    }

    pub fn notify_raw(&self, method: &str, params: Value) -> Result<(), Error> {
        self.inner
            .out
            .send(json!({"jsonrpc":"2.0","method":method,"params":params}))
            .map_err(|_| Error::Closed)
    }

    /// `initialize` + `initialized`. `root` becomes the single workspace folder.
    pub async fn initialize(
        &self,
        root: Option<&Path>,
        init_options: Option<Value>,
    ) -> Result<lsp_types::InitializeResult, Error> {
        let root_uri = root.map(path_to_uri);
        let folders = match (&root_uri, root) {
            (Some(uri), Some(root)) => {
                let name = root.file_name().map(|n| n.to_string_lossy().into_owned());
                json!([{"uri": uri, "name": name.unwrap_or_default()}])
            }
            _ => Value::Null,
        };
        *self.inner.workspace_folders.lock().unwrap() = folders.clone();
        let params = json!({
            "processId": std::process::id(),
            "clientInfo": {"name": "forge"},
            "rootUri": root_uri,
            "workspaceFolders": folders,
            "initializationOptions": init_options,
            "capabilities": client_capabilities(),
        });
        let value = self.request_raw("initialize", params).await?;
        let result = serde_json::from_value(value)?;
        self.notify_raw("initialized", json!({}))?;
        Ok(result)
    }

    /// Graceful stop: `shutdown`, `exit`, then kill if the process lingers.
    pub async fn shutdown(&self) {
        let _ = tokio::time::timeout(SHUTDOWN_TIMEOUT, self.request_raw("shutdown", Value::Null))
            .await;
        let _ = self.notify_raw("exit", Value::Null);
        tokio::time::sleep(Duration::from_millis(500)).await;
        self.kill();
    }

    /// Terminates the process immediately (no-op for non-process connections).
    pub fn kill(&self) {
        if let Some(tx) = self.inner.kill.lock().unwrap().take() {
            let _ = tx.send(());
        }
    }
}

fn dispatch(inner: &Arc<Inner>, msg: Value) {
    let method = msg.get("method").and_then(Value::as_str).map(str::to_owned);
    let id = msg.get("id").cloned().filter(|v| !v.is_null());
    match (method, id) {
        (Some(method), Some(id)) => {
            let params = msg.get("params").cloned().unwrap_or(Value::Null);
            let reply = match server_request(inner, &method, params) {
                Some(result) => json!({"jsonrpc":"2.0","id":id,"result":result}),
                None => json!({"jsonrpc":"2.0","id":id,"error":
                    {"code":-32601,"message":format!("method not found: {method}")}}),
            };
            let _ = inner.out.send(reply);
        }
        (Some(method), None) => {
            let params = msg.get("params").cloned().unwrap_or(Value::Null);
            notification(inner, &method, params);
        }
        (None, Some(id)) => {
            let Some(id) = id.as_i64() else { return };
            let Some(tx) = inner.pending.lock().unwrap().remove(&id) else { return };
            let reply = match msg.get("error") {
                Some(err) => Err(Error::Server {
                    code: err.get("code").and_then(Value::as_i64).unwrap_or(0),
                    message: err
                        .get("message")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_owned(),
                }),
                None => Ok(msg.get("result").cloned().unwrap_or(Value::Null)),
            };
            let _ = tx.send(reply);
        }
        (None, None) => {}
    }
}

/// `Some(result)` if handled, `None` for MethodNotFound.
fn server_request(inner: &Inner, method: &str, params: Value) -> Option<Value> {
    Some(match method {
        "workspace/configuration" => {
            let n = params.get("items").and_then(Value::as_array).map_or(0, Vec::len);
            Value::Array(vec![Value::Null; n])
        }
        "workspace/workspaceFolders" => inner.workspace_folders.lock().unwrap().clone(),
        "client/registerCapability"
        | "client/unregisterCapability"
        | "window/workDoneProgress/create" => Value::Null,
        "window/showMessageRequest" => {
            if let Ok(p) = serde_json::from_value(params) {
                let _ = inner.events.send(Event::ShowMessage(p));
            }
            Value::Null
        }
        "workspace/applyEdit" => json!({"applied": false}),
        _ => return None,
    })
}

fn notification(inner: &Inner, method: &str, params: Value) {
    fn parse<T: DeserializeOwned>(v: Value) -> Option<T> {
        serde_json::from_value(v).ok()
    }
    let event = match method {
        "textDocument/publishDiagnostics" => parse(params).map(Event::Diagnostics),
        "$/progress" => parse(params).map(Event::Progress),
        "window/showMessage" => parse(params).map(Event::ShowMessage),
        "window/logMessage" => parse(params).map(Event::LogMessage),
        _ => None,
    };
    if let Some(event) = event {
        let _ = inner.events.send(event);
    }
}

fn client_capabilities() -> Value {
    json!({
        "general": {"positionEncodings": ["utf-16"]},
        "window": {"workDoneProgress": true, "showMessage": {}},
        "workspace": {"workspaceFolders": true, "configuration": true},
        "textDocument": {
            "synchronization": {"didSave": true, "dynamicRegistration": false},
            "publishDiagnostics": {"relatedInformation": true, "versionSupport": true},
            "hover": {"contentFormat": ["markdown", "plaintext"]},
            "completion": {"completionItem": {
                "snippetSupport": true,
                "documentationFormat": ["markdown", "plaintext"]
            }},
            "signatureHelp": {"signatureInformation": {"parameterInformation": {"labelOffsetSupport": true}, "activeParameterSupport": true,
                "documentationFormat": ["markdown", "plaintext"]
            }},
            "definition": {"linkSupport": true},
            "references": {},
            "rename": {"prepareSupport": true},
            "formatting": {},
            "codeAction": {"codeActionLiteralSupport": {"codeActionKind": {"valueSet": [
                "", "quickfix", "refactor", "source"
            ]}}}
        }
    })
}

/// `file:///C:/dir/file.rs` style URI with percent-encoding of unsafe bytes.
pub fn path_to_uri(path: &Path) -> String {
    let s = path.to_string_lossy().replace('\\', "/");
    let s = s.strip_prefix("//?/").unwrap_or(&s);
    let mut out = String::from("file://");
    if !s.starts_with('/') {
        out.push('/');
    }
    let drive_colon = "file:///".len() + 1;
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' | b'/' => {
                out.push(b as char)
            }
            b':' if out.len() == drive_colon => out.push(':'),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use lsp_types::request::{HoverRequest, Initialize};
    use tokio::io::{DuplexStream, ReadHalf, WriteHalf, split};

    type Server = (BufReader<ReadHalf<DuplexStream>>, WriteHalf<DuplexStream>);

    fn pair() -> (Client, mpsc::UnboundedReceiver<Event>, Server) {
        let (c, s) = tokio::io::duplex(64); // tiny buffer forces split chunks
        let (cr, cw) = split(c);
        let (sr, sw) = split(s);
        let (client, events) = Client::connect(&Handle::current(), BufReader::new(cr), cw, true);
        (client, events, (BufReader::new(sr), sw))
    }

    async fn recv(s: &mut Server) -> Value {
        serde_json::from_slice(&read_message(&mut s.0).await.unwrap().unwrap()).unwrap()
    }

    async fn send(s: &mut Server, v: Value) {
        write_message(&mut s.1, &serde_json::to_vec(&v).unwrap()).await.unwrap();
    }

    fn hover_params() -> lsp_types::HoverParams {
        serde_json::from_value(json!({
            "textDocument": {"uri": "file:///a.rs"},
            "position": {"line": 0, "character": 0}
        }))
        .unwrap()
    }

    #[tokio::test]
    async fn out_of_order_responses_match_by_id() {
        let (client, _ev, mut server) = pair();
        let (c1, c2) = (client.clone(), client.clone());
        let a = tokio::spawn(async move { c1.request_raw("a", Value::Null).await });
        let b = tokio::spawn(async move { c2.request_raw("b", Value::Null).await });
        let (m1, m2) = (recv(&mut server).await, recv(&mut server).await);
        for m in [&m2, &m1] {
            let result = m["method"].clone();
            send(&mut server, json!({"jsonrpc":"2.0","id":m["id"],"result":result})).await;
        }
        assert_eq!(a.await.unwrap().unwrap(), json!("a"));
        assert_eq!(b.await.unwrap().unwrap(), json!("b"));
    }

    #[tokio::test]
    async fn typed_request_and_server_error() {
        let (client, _ev, mut server) = pair();
        let c = client.clone();
        let t = tokio::spawn(async move { c.request::<HoverRequest>(hover_params()).await });
        let m = recv(&mut server).await;
        assert_eq!(m["method"], "textDocument/hover");
        send(&mut server, json!({"jsonrpc":"2.0","id":m["id"],
            "result":{"contents":"hi"}})).await;
        assert!(t.await.unwrap().unwrap().is_some());

        let c = client.clone();
        let t = tokio::spawn(async move { c.request::<HoverRequest>(hover_params()).await });
        let m = recv(&mut server).await;
        send(&mut server, json!({"jsonrpc":"2.0","id":m["id"],
            "error":{"code":-32801,"message":"nope"}})).await;
        assert!(matches!(t.await.unwrap(), Err(Error::Server { code: -32801, .. })));
    }

    #[tokio::test(start_paused = true)]
    async fn timeout_sends_cancel() {
        let (client, _ev, mut server) = pair();
        let c = client.clone();
        let t = tokio::spawn(async move { c.request_raw("slow", Value::Null).await });
        let m = recv(&mut server).await;
        assert!(matches!(t.await.unwrap(), Err(Error::Timeout)));
        let cancel = recv(&mut server).await;
        assert_eq!(cancel["method"], "$/cancelRequest");
        assert_eq!(cancel["params"]["id"], m["id"]);
    }

    #[tokio::test]
    async fn dropped_future_sends_cancel() {
        let (client, _ev, mut server) = pair();
        let c = client.clone();
        let t = tokio::spawn(async move { c.request_raw("slow", Value::Null).await });
        let m = recv(&mut server).await;
        t.abort();
        let cancel = recv(&mut server).await;
        assert_eq!(cancel["method"], "$/cancelRequest");
        assert_eq!(cancel["params"]["id"], m["id"]);
    }

    #[tokio::test]
    async fn answers_server_requests_and_emits_events() {
        let (_client, mut ev, mut server) = pair();
        send(&mut server, json!({"jsonrpc":"2.0","id":7,"method":"workspace/configuration",
            "params":{"items":[{},{}]}})).await;
        let r = recv(&mut server).await;
        assert_eq!(r["id"], 7);
        assert_eq!(r["result"], json!([null, null]));

        send(&mut server, json!({"jsonrpc":"2.0","id":8,"method":"weird/thing"})).await;
        let r = recv(&mut server).await;
        assert_eq!(r["error"]["code"], -32601);

        send(&mut server, json!({"jsonrpc":"2.0","method":"textDocument/publishDiagnostics",
            "params":{"uri":"file:///a.rs","diagnostics":[]}})).await;
        send(&mut server, json!({"jsonrpc":"2.0","method":"window/logMessage",
            "params":{"type":3,"message":"hello"}})).await;
        assert!(matches!(ev.recv().await, Some(Event::Diagnostics(_))));
        assert!(matches!(ev.recv().await, Some(Event::LogMessage(_))));
    }

    #[tokio::test]
    async fn initialize_handshake() {
        let (client, _ev, mut server) = pair();
        let c = client.clone();
        let t = tokio::spawn(async move {
            c.initialize(Some(Path::new("C:\\my proj")), None).await
        });
        let m = recv(&mut server).await;
        assert_eq!(m["method"], Initialize::METHOD);
        assert_eq!(m["params"]["rootUri"], "file:///C:/my%20proj");
        send(&mut server, json!({"jsonrpc":"2.0","id":m["id"],
            "result":{"capabilities":{"hoverProvider":true}}})).await;
        let res = t.await.unwrap().unwrap();
        assert!(res.capabilities.hover_provider.is_some());
        assert_eq!(recv(&mut server).await["method"], "initialized");
    }

    #[tokio::test]
    async fn disconnect_fails_pending_and_reports_exit() {
        let (client, mut ev, server) = pair();
        let c = client.clone();
        let t = tokio::spawn(async move { c.request_raw("x", Value::Null).await });
        tokio::task::yield_now().await;
        drop(server);
        assert!(matches!(t.await.unwrap(), Err(Error::Closed)));
        assert!(matches!(ev.recv().await, Some(Event::Exited(None))));
    }

    #[test]
    fn uri_encoding() {
        assert_eq!(path_to_uri(Path::new("C:\\a b\\c#.rs")), "file:///C:/a%20b/c%23.rs");
        assert_eq!(path_to_uri(Path::new("/usr/x")), "file:///usr/x");
    }

    /// Needs rust-analyzer on PATH: `cargo test -p lsp_client -- --ignored`.
    #[tokio::test]
    #[ignore]
    async fn rust_analyzer_handshake() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"));
        let (client, mut ev) = Client::spawn(&Handle::current(), Path::new("rust-analyzer"), &[], Some(root))
            .expect("spawn rust-analyzer");
        let res = client.initialize(Some(root), None).await.expect("initialize");
        assert!(res.capabilities.hover_provider.is_some());
        while let Ok(Some(e)) = tokio::time::timeout(Duration::from_secs(2), ev.recv()).await {
            if let Event::Progress(p) = e { println!("progress: {p:?}"); }
        }
        client.shutdown().await;
        loop {
            match ev.recv().await {
                Some(Event::Exited(code)) => { println!("exited {code:?}"); break; }
                Some(_) => {}
                None => panic!("no exit event"),
            }
        }
    }
}
