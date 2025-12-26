//! STDIO proxy for connecting to the mux socket.
//!
//! Bridges stdin/stdout to a Unix socket using JSON-RPC framing.
//! Handles half-close semantics: when stdin closes (client done sending),
//! we keep reading from socket until we get all responses.

use std::path::PathBuf;

use anyhow::{Context, Result};
use futures::{SinkExt, StreamExt};
use rmcp::transport::async_rw::JsonRpcMessageCodec;
use serde_json::Value;
use tokio::io::{stdin, stdout};
use tokio::net::UnixStream;
use tokio_util::codec::{FramedRead, FramedWrite};
use tracing::{debug, warn};

/// Proxy STDIO to the mux socket using JSON-RPC framing.
///
/// Important: This handles "half-close" semantics properly:
/// - When stdin closes (EOF), we signal the socket write side is done
///   but continue reading responses from the socket
/// - Only exit when BOTH directions are done (stdin closed AND socket closed)
pub async fn run_proxy(socket: PathBuf) -> Result<()> {
    let stream = UnixStream::connect(&socket)
        .await
        .with_context(|| format!("failed to connect to {}", socket.display()))?;
    let (sr, sw) = stream.into_split();
    let mut sock_reader = FramedRead::new(sr, JsonRpcMessageCodec::<Value>::new());
    let mut sock_writer = FramedWrite::new(sw, JsonRpcMessageCodec::<Value>::new());
    let mut stdin_reader = FramedRead::new(stdin(), JsonRpcMessageCodec::<Value>::new());
    let mut stdout_writer = FramedWrite::new(stdout(), JsonRpcMessageCodec::<Value>::new());

    // Track pending requests (requests with id that need responses)
    let mut pending_requests: u64 = 0;
    let mut stdin_closed = false;

    loop {
        tokio::select! {
            // Read from stdin, write to socket
            msg = stdin_reader.next(), if !stdin_closed => {
                match msg {
                    Some(Ok(v)) => {
                        // Track if this is a request (has id) vs notification (no id)
                        if v.get("id").is_some() && v.get("method").is_some() {
                            pending_requests += 1;
                            debug!("request sent, pending={pending_requests}");
                        }
                        if let Err(e) = sock_writer.send(v).await {
                            warn!("socket write error: {e}");
                            break;
                        }
                    }
                    Some(Err(e)) => {
                        warn!("stdin decode error: {e}");
                        stdin_closed = true;
                    }
                    None => {
                        // stdin closed (EOF) - client done sending
                        // Don't exit! Keep reading responses from socket
                        debug!("stdin closed, waiting for {pending_requests} responses");
                        stdin_closed = true;
                    }
                }
            }

            // Read from socket, write to stdout
            msg = sock_reader.next() => {
                match msg {
                    Some(Ok(v)) => {
                        // Track if this is a response (has id, no method)
                        if v.get("id").is_some() && v.get("method").is_none() {
                            pending_requests = pending_requests.saturating_sub(1);
                            debug!("response received, pending={pending_requests}");
                        }
                        if let Err(e) = stdout_writer.send(v).await {
                            warn!("stdout write error: {e}");
                            break;
                        }
                        // If stdin is closed and no more pending requests, we're done
                        if stdin_closed && pending_requests == 0 {
                            debug!("all responses received, exiting");
                            break;
                        }
                    }
                    Some(Err(e)) => {
                        warn!("socket decode error: {e}");
                        break;
                    }
                    None => {
                        // socket closed
                        debug!("socket closed");
                        break;
                    }
                }
            }
        }
    }

    Ok(())
}
