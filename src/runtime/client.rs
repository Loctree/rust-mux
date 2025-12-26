//! Client connection handling.

use std::sync::Arc;
use std::time::Instant;

use anyhow::{Result, anyhow};
use futures::{SinkExt, StreamExt};
use rmcp::transport::async_rw::JsonRpcMessageCodec;
use serde_json::Value;
use tokio::net::UnixStream;
use tokio::sync::{Mutex, Semaphore, mpsc, watch};
use tokio_util::codec::{FramedRead, FramedWrite};
use tokio_util::sync::CancellationToken;
use tracing::{debug, info, warn};

use crate::state::{
    HANDSHAKE_TIMEOUT, MuxState, Pending, StatusSnapshot, error_response, publish_status, set_id,
};

use super::types::MAX_PENDING;

/// Update queue_depth in state based on channel capacity.
pub async fn update_queue_depth(state: &Arc<Mutex<MuxState>>, to_server_tx: &mpsc::Sender<Value>) {
    let mut st = state.lock().await;
    st.queue_depth = to_server_tx.max_capacity() - to_server_tx.capacity();
}

/// Handle a single client connection.
pub async fn handle_client(
    stream: UnixStream,
    state: Arc<Mutex<MuxState>>,
    to_server_tx: mpsc::Sender<Value>,
    active_clients: Arc<Semaphore>,
    status_tx: watch::Sender<StatusSnapshot>,
    shutdown: CancellationToken,
) -> Result<()> {
    // limit active clients
    let _permit = active_clients
        .clone()
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
    publish_status(&state, &active_clients, &status_tx).await;

    // Writer task
    let writer_state = state.clone();
    let writer_status = status_tx.clone();
    let writer_active = active_clients.clone();
    let writer_handle = tokio::spawn(async move {
        while let Some(msg) = client_rx.recv().await {
            if let Err(e) = client_writer.send(msg).await {
                warn!("write to client {client_id} failed: {e}");
                break;
            }
        }
        let mut st = writer_state.lock().await;
        st.unregister_client(client_id);
        drop(st);
        publish_status(&writer_state, &writer_active, &writer_status).await;
        info!("client {client_id} writer closed");
    });

    // Reader loop with handshake timeout checking
    let mut handshake_check_interval = tokio::time::interval(std::time::Duration::from_secs(1));
    loop {
        tokio::select! {
            _ = shutdown.cancelled() => break,
            _ = handshake_check_interval.tick() => {
                // Check for handshake timeout
                let timed_out = {
                    let st = state.lock().await;
                    st.is_handshake_timed_out(client_id)
                };
                if timed_out {
                    warn!("client {client_id} handshake timed out after {:?}", HANDSHAKE_TIMEOUT);
                    let st = state.lock().await;
                    if let Some(tx) = st.clients.get(&client_id) {
                        tx.send(error_response(
                            Value::Null,
                            "handshake timeout: initialize/initialized sequence not completed",
                        ))
                        .ok();
                    }
                    break;
                }
            }
            frame = client_reader.next() => {
                let Some(frame) = frame else { break; };
                let msg = frame?;
                let max_request_bytes = {
                    let st = state.lock().await;
                    st.max_request_bytes
                };
                if let Err(e) = handle_client_message(
                    client_id,
                    msg,
                    &state,
                    &to_server_tx,
                    &active_clients,
                    &status_tx,
                    max_request_bytes,
                )
                .await
                {
                    warn!("client {client_id} message error: {e}");
                }
            }
        }
    }

    {
        let mut st = state.lock().await;
        st.unregister_client(client_id);
    }
    publish_status(&state, &active_clients, &status_tx).await;
    // Cancel writer task and wait for cleanup to complete
    writer_handle.abort();
    let _ = writer_handle.await;
    info!("client {client_id} disconnected");
    Ok(())
}

/// Handle a single message from a client.
///
/// Implements MCP protocol tolerance by buffering messages that arrive
/// before the handshake (initialize + notifications/initialized) completes.
/// This prevents backend crashes when clients like Claude Code send
/// `tools/list` before completing the handshake sequence.
pub async fn handle_client_message(
    client_id: u64,
    mut msg: Value,
    state: &Arc<Mutex<MuxState>>,
    to_server_tx: &mpsc::Sender<Value>,
    active_clients: &Arc<Semaphore>,
    status_tx: &watch::Sender<StatusSnapshot>,
    max_request_bytes: usize,
) -> Result<()> {
    let encoded_len = serde_json::to_vec(&msg)?.len();
    if encoded_len > max_request_bytes {
        let st = state.lock().await;
        if let Some(tx) = st.clients.get(&client_id) {
            tx.send(error_response(
                msg.get("id").cloned().unwrap_or(Value::Null),
                "request too large",
            ))
            .ok();
        }
        return Ok(());
    }

    let method = msg
        .get("method")
        .and_then(|m| m.as_str())
        .unwrap_or_default()
        .to_string();

    // Handle notifications/initialized - completes the handshake
    if method == "notifications/initialized" {
        info!("client {client_id} sent notifications/initialized, completing handshake");

        // Only forward to server if this is the FIRST initialized notification
        // Subsequent clients' initialized notifications should NOT be forwarded
        // because the MCP server only expects ONE initialized notification after startup
        let should_forward = {
            let mut st = state.lock().await;
            if !st.server_initialized {
                st.server_initialized = true;
                true
            } else {
                debug!(
                    "dropping notifications/initialized from client {client_id} - server already initialized"
                );
                false
            }
        };

        if should_forward && let Err(e) = to_server_tx.try_send(msg) {
            warn!("dropping notifications/initialized from client {client_id}: {e}");
        }

        // Complete handshake and flush buffered messages
        let buffered = {
            let mut st = state.lock().await;
            st.complete_handshake(client_id)
        };

        if !buffered.is_empty() {
            info!(
                "client {client_id} handshake complete, flushing {} buffered messages",
                buffered.len()
            );
            for buffered_msg in buffered {
                // Recursively process buffered messages (they already passed size check)
                if let Err(e) = Box::pin(forward_request(
                    client_id,
                    buffered_msg,
                    state,
                    to_server_tx,
                    active_clients,
                    status_tx,
                ))
                .await
                {
                    warn!("client {client_id} failed to forward buffered message: {e}");
                }
            }
        }

        update_queue_depth(state, to_server_tx).await;
        publish_status(state, active_clients, status_tx).await;
        return Ok(());
    }

    // Notifications (no id) that are NOT notifications/initialized
    // Forward best-effort; if the queue is full we drop with a warning.
    if msg.get("id").is_none() {
        if let Err(e) = to_server_tx.try_send(msg) {
            warn!("dropping notification from client {client_id}: {e}");
        }
        update_queue_depth(state, to_server_tx).await;
        publish_status(state, active_clients, status_tx).await;
        return Ok(());
    }

    let local_id = msg
        .get("id")
        .cloned()
        .ok_or_else(|| anyhow!("missing id"))?;

    // Handle initialize request
    if method == "initialize" {
        let mut st = state.lock().await;

        // Mark handshake as pending (waiting for initialize response)
        if let Some(h) = st.get_handshake_mut(client_id) {
            h.initialize_pending = true;
        }

        if let Some(cached) = st.cached_initialize.clone() {
            // Serve initialize from cache
            if let Some(tx) = st.clients.get(&client_id) {
                let mut resp = cached.clone();
                set_id(&mut resp, local_id);
                tx.send(resp).ok();
            }
            return Ok(());
        }

        if st.initializing {
            st.init_waiting.push((client_id, local_id));
            return Ok(());
        }

        if st.pending.len() >= MAX_PENDING {
            if let Some(tx) = st.clients.get(&client_id) {
                tx.send(error_response(
                    local_id.clone(),
                    "mux overloaded (too many pending)",
                ))
                .ok();
            }
            return Ok(());
        }
        let global_id = format!("c{client_id}:{}", st.next_request_id());
        st.pending.insert(
            global_id.clone(),
            Pending {
                client_id,
                local_id: local_id.clone(),
                is_initialize: true,
                started_at: Instant::now(),
            },
        );
        drop(st);
        set_id(&mut msg, Value::String(global_id));
        to_server_tx
            .send(msg)
            .await
            .map_err(|_| anyhow!("server channel closed"))?;
        update_queue_depth(state, to_server_tx).await;
        publish_status(state, active_clients, status_tx).await;
        return Ok(());
    }

    // For all other requests, check handshake state
    let handshake_complete = {
        let st = state.lock().await;
        st.is_handshake_complete(client_id)
    };

    if !handshake_complete {
        // Buffer the message until handshake completes
        info!(
            "client {client_id} sent {} before handshake complete, buffering",
            method
        );
        let mut st = state.lock().await;
        st.buffer_message(client_id, msg);
        return Ok(());
    }

    // Handshake complete - forward normally
    forward_request(
        client_id,
        msg,
        state,
        to_server_tx,
        active_clients,
        status_tx,
    )
    .await
}

/// Forward a request to the server (used for both immediate and buffered messages).
async fn forward_request(
    client_id: u64,
    mut msg: Value,
    state: &Arc<Mutex<MuxState>>,
    to_server_tx: &mpsc::Sender<Value>,
    active_clients: &Arc<Semaphore>,
    status_tx: &watch::Sender<StatusSnapshot>,
) -> Result<()> {
    let local_id = msg
        .get("id")
        .cloned()
        .ok_or_else(|| anyhow!("missing id"))?;

    let global_id = {
        let mut st = state.lock().await;
        if st.pending.len() >= MAX_PENDING {
            if let Some(tx) = st.clients.get(&client_id) {
                tx.send(error_response(
                    local_id.clone(),
                    "mux overloaded (too many pending)",
                ))
                .ok();
            }
            return Ok(());
        }
        let gid = format!("c{client_id}:{}", st.next_request_id());
        st.pending.insert(
            gid.clone(),
            Pending {
                client_id,
                local_id: local_id.clone(),
                is_initialize: false,
                started_at: Instant::now(),
            },
        );
        gid
    };

    set_id(&mut msg, Value::String(global_id));
    to_server_tx
        .send(msg)
        .await
        .map_err(|_| anyhow!("server channel closed"))?;
    update_queue_depth(state, to_server_tx).await;
    publish_status(state, active_clients, status_tx).await;
    Ok(())
}
