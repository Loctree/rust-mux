use std::collections::HashMap;
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::Arc;

use anyhow::{anyhow, Context, Result};
use clap::Parser;
use futures::{SinkExt, StreamExt};
use rmcp::transport::async_rw::JsonRpcMessageCodec;
use serde_json::Value;
use tokio::net::{UnixListener, UnixStream};
use tokio::process::Command;
use tokio::signal;
use tokio::sync::{mpsc, Mutex, Semaphore};
use tokio_util::codec::{FramedRead, FramedWrite};
use tokio_util::sync::CancellationToken;
use tracing::{error, info, warn};

/// Odporny mux MCP: jeden child-proces serwera MCP, wielu klientów przez UNIX socket,
/// cache initialize, przepisywanie ID, restart childa po błędzie i limit aktywnych klientów.
#[derive(Parser, Debug)]
#[command(author, version, about)]
struct Cli {
    /// Ścieżka gniazda UNIX, na którym mux nasłuchuje.
    #[arg(long)]
    socket: PathBuf,
    /// Komenda serwera MCP (np. `npx`).
    #[arg(long)]
    cmd: String,
    /// Argumenty przekazywane do komendy serwera.
    #[arg(last = true)]
    args: Vec<String>,
    /// Maksymalna liczba aktywnych klientów (pozwolenia na równoległe użycie serwera).
    #[arg(long, default_value = "5")]
    max_active_clients: usize,
    /// Poziom logów (trace|debug|info|warn|error).
    #[arg(long, default_value = "info")]
    log_level: String,
}

#[derive(Clone, Debug)]
struct Pending {
    client_id: u64,
    local_id: Value,
    is_initialize: bool,
}

#[derive(Clone)]
struct MuxState {
    next_client_id: u64,
    next_global_id: u64,
    clients: HashMap<u64, mpsc::UnboundedSender<Value>>,
    pending: HashMap<String, Pending>,
    cached_initialize: Option<Value>,
    init_waiting: Vec<(u64, Value)>,
    initializing: bool,
}

impl MuxState {
    fn new() -> Self {
        Self {
            next_client_id: 1,
            next_global_id: 1,
            clients: HashMap::new(),
            pending: HashMap::new(),
            cached_initialize: None,
            init_waiting: Vec::new(),
            initializing: false,
        }
    }

    fn register_client(&mut self, tx: mpsc::UnboundedSender<Value>) -> u64 {
        let id = self.next_client_id;
        self.next_client_id += 1;
        self.clients.insert(id, tx);
        id
    }

    fn unregister_client(&mut self, client_id: u64) {
        self.clients.remove(&client_id);
        self.pending.retain(|_, p| p.client_id != client_id);
        self.init_waiting.retain(|(cid, _)| *cid != client_id);
    }

    fn next_request_id(&mut self) -> u64 {
        let id = self.next_global_id;
        self.next_global_id += 1;
        id
    }
}

enum ServerEvent {
    Message(Value),
    Reset(String),
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    tracing_subscriber::fmt()
        .with_env_filter(format!("mcp_mux={}", cli.log_level))
        .with_target(false)
        .init();

    if let Some(parent) = cli.socket.parent() {
        tokio::fs::create_dir_all(parent)
            .await
            .context("failed to create socket parent dir")?;
    }
    let _ = tokio::fs::remove_file(&cli.socket).await;

    let listener = UnixListener::bind(&cli.socket)
        .with_context(|| format!("failed to bind socket {}", cli.socket.display()))?;
    info!("mcp_mux nasłuchuje na {}", cli.socket.display());

    let shutdown = CancellationToken::new();
    let shutdown_signal = shutdown.clone();
    tokio::spawn(async move {
        let _ = signal::ctrl_c().await;
        shutdown_signal.cancel();
    });

    let state = Arc::new(Mutex::new(MuxState::new()));
    let active_clients = Arc::new(Semaphore::new(cli.max_active_clients));

    let (to_server_tx, to_server_rx) = mpsc::unbounded_channel::<Value>();
    let (server_events_tx, server_events_rx) = mpsc::unbounded_channel::<ServerEvent>();

    // Router serwera -> klientów
    let router_state = state.clone();
    tokio::spawn(async move {
        handle_server_events(router_state, server_events_rx).await;
    });

    // Zarządca child-procesu
    let server_state = state.clone();
    let server_shutdown = shutdown.clone();
    tokio::spawn(async move {
        if let Err(e) = server_manager(
            cli.cmd.clone(),
            cli.args.clone(),
            to_server_rx,
            server_events_tx,
            server_state,
            server_shutdown,
        )
        .await
        {
            error!("server manager exited with error: {e}");
        }
    });

    // Akceptowanie klientów
    loop {
        tokio::select! {
            _ = shutdown.cancelled() => {
                info!("shutdown requested; closing listener");
                break;
            }
            accept_res = listener.accept() => {
                let (stream, _) = match accept_res {
                    Ok(v) => v,
                    Err(e) => {
                        warn!("accept failed: {e}");
                        continue;
                    }
                };
                let state = state.clone();
                let to_server_tx = to_server_tx.clone();
                let active_clients = active_clients.clone();
                let shutdown = shutdown.clone();
                tokio::spawn(async move {
                    if let Err(e) = handle_client(stream, state, to_server_tx, active_clients, shutdown).await {
                        warn!("client handler error: {e}");
                    }
                });
            }
        }
    }

    // Sprzątanie socketu
    let _ = tokio::fs::remove_file(&cli.socket).await;
    Ok(())
}

async fn handle_client(
    stream: UnixStream,
    state: Arc<Mutex<MuxState>>,
    to_server_tx: mpsc::UnboundedSender<Value>,
    active_clients: Arc<Semaphore>,
    shutdown: CancellationToken,
) -> Result<()> {
    // limit aktywnych klientów
    let _permit = active_clients
        .acquire_owned()
        .await
        .map_err(|_| anyhow!("semaphore closed"))?;

    let (read_half, write_half) = stream.into_split();
    let mut client_reader = FramedRead::new(read_half, JsonRpcMessageCodec::<Value>::new());
    let mut client_writer = FramedWrite::new(write_half, JsonRpcMessageCodec::<Value>::new());

    let (client_tx, mut client_rx) = mpsc::unbounded_channel::<Value>();
    let client_id = {
        let mut st = state.lock().await;
        st.register_client(client_tx)
    };
    info!("client {client_id} connected");

    // Writer task
    let writer_state = state.clone();
    let writer_handle = tokio::spawn(async move {
        while let Some(msg) = client_rx.recv().await {
            if let Err(e) = client_writer.send(msg).await {
                warn!("write to client {client_id} failed: {e}");
                break;
            }
        }
        let mut st = writer_state.lock().await;
        st.unregister_client(client_id);
        info!("client {client_id} writer closed");
    });

    // Reader loop
    loop {
        tokio::select! {
            _ = shutdown.cancelled() => break,
            frame = client_reader.next() => {
                let Some(frame) = frame else { break; };
                let msg = frame?;
                if let Err(e) = handle_client_message(client_id, msg, &state, &to_server_tx).await {
                    warn!("client {client_id} message error: {e}");
                }
            }
        }
    }

    {
        let mut st = state.lock().await;
        st.unregister_client(client_id);
    }
    writer_handle.abort();
    info!("client {client_id} disconnected");
    Ok(())
}

async fn handle_client_message(
    client_id: u64,
    mut msg: Value,
    state: &Arc<Mutex<MuxState>>,
    to_server_tx: &mpsc::UnboundedSender<Value>,
) -> Result<()> {
    // Notification (brak id) - forward as-is
    if msg.get("id").is_none() {
        to_server_tx
            .send(msg)
            .map_err(|_| anyhow!("server channel closed"))?;
        return Ok(());
    }

    let method = msg
        .get("method")
        .and_then(|m| m.as_str())
        .unwrap_or_default()
        .to_string();
    let local_id = msg
        .get("id")
        .cloned()
        .ok_or_else(|| anyhow!("missing id"))?;

    if method == "initialize" {
        let mut st = state.lock().await;
        if let Some(cached) = st.cached_initialize.clone() {
            // Odsyłamy z cache
            if let Some(tx) = st.clients.get(&client_id) {
                let mut resp = cached.clone();
                set_id(&mut resp, local_id);
                tx.send(resp).ok();
            }
            return Ok(());
        }

        if st.initializing {
            // Czekamy na wynik initialize
            st.init_waiting.push((client_id, local_id));
            return Ok(());
        }

        // Pierwszy initialize: przepuszczamy do serwera
        st.initializing = true;
        let global_id = format!("c{client_id}:{}", st.next_request_id());
        st.pending.insert(
            global_id.clone(),
            Pending {
                client_id,
                local_id: local_id.clone(),
                is_initialize: true,
            },
        );
        drop(st);
        set_id(&mut msg, Value::String(global_id));
        to_server_tx
            .send(msg)
            .map_err(|_| anyhow!("server channel closed"))?;
        return Ok(());
    }

    // Zwykłe żądanie
    let global_id = {
        let mut st = state.lock().await;
        let gid = format!("c{client_id}:{}", st.next_request_id());
        st.pending.insert(
            gid.clone(),
            Pending {
                client_id,
                local_id: local_id.clone(),
                is_initialize: false,
            },
        );
        gid
    };

    set_id(&mut msg, Value::String(global_id));
    to_server_tx
        .send(msg)
        .map_err(|_| anyhow!("server channel closed"))?;
    Ok(())
}

async fn handle_server_events(
    state: Arc<Mutex<MuxState>>,
    mut rx: mpsc::UnboundedReceiver<ServerEvent>,
) {
    while let Some(evt) = rx.recv().await {
        match evt {
            ServerEvent::Message(msg) => {
                if let Err(e) = handle_server_message(msg, &state).await {
                    warn!("server message routing failed: {e}");
                }
            }
            ServerEvent::Reset(reason) => {
                reset_state(&state, &reason).await;
            }
        }
    }
}

async fn handle_server_message(msg: Value, state: &Arc<Mutex<MuxState>>) -> Result<()> {
    if msg.get("id").is_none() {
        // notification -> broadcast
        let st = state.lock().await;
        for tx in st.clients.values() {
            tx.send(msg.clone()).ok();
        }
        return Ok(());
    }

    let id_val = msg
        .get("id")
        .cloned()
        .ok_or_else(|| anyhow!("missing id in server response"))?;
    let id_str = id_val
        .as_str()
        .map(|s| s.to_string())
        .or_else(|| id_val.as_i64().map(|n| n.to_string()))
        .ok_or_else(|| anyhow!("unsupported id type"))?;

    let pending = {
        let mut st = state.lock().await;
        st.pending.remove(&id_str)
    };

    let Some(pending) = pending else {
        warn!("no pending request for id {id_str}");
        return Ok(());
    };

    let target_tx = {
        let st = state.lock().await;
        st.clients.get(&pending.client_id).cloned()
    };

    if let Some(tx) = target_tx {
        let mut resp = msg.clone();
        set_id(&mut resp, pending.local_id.clone());
        let is_init = pending.is_initialize;
        tx.send(resp.clone()).ok();

        if is_init {
            let mut st = state.lock().await;
            st.cached_initialize = Some(resp.clone());
            st.initializing = false;
            // Odpowiedzi dla oczekujących
            let waiters = std::mem::take(&mut st.init_waiting);
            for (cid, lid) in waiters {
                if let Some(wait_tx) = st.clients.get(&cid) {
                    let mut clone_resp = resp.clone();
                    set_id(&mut clone_resp, lid);
                    wait_tx.send(clone_resp).ok();
                }
            }
        }
    }
    Ok(())
}

async fn reset_state(state: &Arc<Mutex<MuxState>>, reason: &str) {
    let mut st = state.lock().await;
    let pending = std::mem::take(&mut st.pending);
    let waiters = std::mem::take(&mut st.init_waiting);
    st.cached_initialize = None;
    st.initializing = false;

    for (_, p) in pending {
        if let Some(tx) = st.clients.get(&p.client_id) {
            tx.send(error_response(p.local_id, reason)).ok();
        }
    }
    for (cid, lid) in waiters {
        if let Some(tx) = st.clients.get(&cid) {
            tx.send(error_response(lid, reason)).ok();
        }
    }
}

fn error_response(id: Value, message: &str) -> Value {
    serde_json::json!({
        "jsonrpc": "2.0",
        "id": id,
        "error": {
            "code": -32000,
            "message": message,
        }
    })
}

fn set_id(msg: &mut Value, id: Value) {
    if let Some(obj) = msg.as_object_mut() {
        obj.insert("id".to_string(), id);
    }
}

async fn server_manager(
    cmd: String,
    args: Vec<String>,
    mut to_server_rx: mpsc::UnboundedReceiver<Value>,
    server_events_tx: mpsc::UnboundedSender<ServerEvent>,
    state: Arc<Mutex<MuxState>>,
    shutdown: CancellationToken,
) -> Result<()> {
    loop {
        if shutdown.is_cancelled() {
            break;
        }

        info!("starting MCP server: {} {:?}", cmd, args);
        let mut child = Command::new(&cmd)
            .args(&args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .context("failed to spawn MCP server")?;

        let child_stdin = child
            .stdin
            .take()
            .ok_or_else(|| anyhow!("failed to capture stdin"))?;
        let child_stdout = child
            .stdout
            .take()
            .ok_or_else(|| anyhow!("failed to capture stdout"))?;

        let mut writer = FramedWrite::new(child_stdin, JsonRpcMessageCodec::<Value>::new());
        let mut reader = FramedRead::new(child_stdout, JsonRpcMessageCodec::<Value>::new());

        // czytanie z serwera
        let reader_task = {
            let server_events_tx = server_events_tx.clone();
            tokio::spawn(async move {
                loop {
                    let next = reader.next().await;
                    match next {
                        Some(Ok(msg)) => {
                            if server_events_tx.send(ServerEvent::Message(msg)).is_err() {
                                break;
                            }
                        }
                        Some(Err(e)) => {
                            error!("server reader error: {e}");
                            break;
                        }
                        None => {
                            warn!("server stdout closed");
                            break;
                        }
                    }
                }
            })
        };

        // pętla pisania i monitorowania
        let server_events_tx_clone = server_events_tx.clone();
        while !shutdown.is_cancelled() {
            tokio::select! {
                maybe_msg = to_server_rx.recv() => {
                    let Some(msg) = maybe_msg else { break; };
                    if let Err(e) = writer.send(msg).await { warn!("write to server failed: {e}"); break; }
                }
                status = child.wait() => {
                    match status {
                        Ok(status) => warn!("server exited with status {status}"),
                        Err(e) => warn!("server wait error: {e}"),
                    }
                    break;
                }
                _ = shutdown.cancelled() => { break; }
            }
        }

        // sprzątanie childa
        let _ = child.kill().await;
        reader_task.abort();

        // reset stanu
        server_events_tx_clone
            .send(ServerEvent::Reset("MCP server restarted".into()))
            .ok();
        {
            let mut st = state.lock().await;
            st.cached_initialize = None;
            st.initializing = false;
        }

        if shutdown.is_cancelled() {
            break;
        }
        info!("restarting MCP server after failure");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::sync::mpsc::UnboundedReceiver;

    fn capture_client(state: &mut MuxState) -> (u64, UnboundedReceiver<Value>) {
        let (tx, rx) = mpsc::unbounded_channel();
        let id = state.register_client(tx);
        (id, rx)
    }

    #[tokio::test]
    async fn set_id_updates_object() {
        let mut obj = serde_json::json!({"id": "old"});
        set_id(&mut obj, Value::String("new".into()));
        assert_eq!(obj.get("id"), Some(&Value::String("new".into())));
    }

    #[tokio::test]
    async fn error_response_has_code_and_message() {
        let resp = error_response(Value::Number(1.into()), "boom");
        assert_eq!(resp.get("id"), Some(&Value::Number(1.into())));
        assert_eq!(
            resp.get("error").and_then(|e| e.get("message")),
            Some(&Value::String("boom".into()))
        );
    }

    #[tokio::test]
    async fn initialize_response_is_cached_and_fanned_out() {
        let state = Arc::new(Mutex::new(MuxState::new()));
        let mut st = state.lock().await;
        let (cid1, mut rx1) = capture_client(&mut st);
        let (cid2, mut rx2) = capture_client(&mut st);

        // pending initialize for client1
        st.pending.insert(
            "g1".into(),
            Pending {
                client_id: cid1,
                local_id: Value::String("loc1".into()),
                is_initialize: true,
            },
        );
        // waiter client2
        st.init_waiting.push((cid2, Value::String("loc2".into())));
        st.initializing = true;
        drop(st);

        let server_msg = serde_json::json!({
            "id": "g1",
            "result": { "ok": true }
        });
        assert!(handle_server_message(server_msg, &state).await.is_ok());

        // client1 got response with local id
        let m1 = rx1.recv().await.expect("client1 message");
        assert_eq!(m1.get("id"), Some(&Value::String("loc1".into())));

        // client2 got fanned out cached response with its id
        let m2 = rx2.recv().await.expect("client2 message");
        assert_eq!(m2.get("id"), Some(&Value::String("loc2".into())));

        // cache is populated, initializing flag cleared
        let st = state.lock().await;
        assert!(st.cached_initialize.is_some());
        assert!(!st.initializing);
        assert!(st.init_waiting.is_empty());
    }

    #[tokio::test]
    async fn non_initialize_response_routed_without_caching() {
        let state = Arc::new(Mutex::new(MuxState::new()));
        let mut st = state.lock().await;
        let (cid1, mut rx1) = capture_client(&mut st);
        st.pending.insert(
            "g2".into(),
            Pending {
                client_id: cid1,
                local_id: Value::Number(7.into()),
                is_initialize: false,
            },
        );
        drop(st);

        let server_msg = serde_json::json!({"id": "g2", "result": 123});
        assert!(handle_server_message(server_msg, &state).await.is_ok());

        let msg = rx1.recv().await.expect("client message");
        assert_eq!(msg.get("id"), Some(&Value::Number(7.into())));
        let st = state.lock().await;
        assert!(st.cached_initialize.is_none());
    }

    #[tokio::test]
    async fn reset_state_broadcasts_errors() {
        let state = Arc::new(Mutex::new(MuxState::new()));
        let mut st = state.lock().await;
        let (cid1, mut rx1) = capture_client(&mut st);
        let (cid2, mut rx2) = capture_client(&mut st);
        st.pending.insert(
            "g3".into(),
            Pending {
                client_id: cid1,
                local_id: Value::Number(1.into()),
                is_initialize: false,
            },
        );
        st.init_waiting.push((cid2, Value::Number(2.into())));
        drop(st);

        reset_state(&state, "reset").await;

        let m1 = rx1.recv().await.expect("pending error");
        let m2 = rx2.recv().await.expect("waiter error");
        assert_eq!(
            m1.get("error").and_then(|e| e.get("message")),
            Some(&Value::String("reset".into()))
        );
        assert_eq!(
            m2.get("error").and_then(|e| e.get("message")),
            Some(&Value::String("reset".into()))
        );
    }
}
