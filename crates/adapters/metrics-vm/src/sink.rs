// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! [`VmMetrics`]: the bounded queue, the background flush task, and the HTTP transport.

use core::future::Future;
use std::sync::Arc;

use serde_json::{Map, Value};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::sync::{mpsc, oneshot};

use pos_ports::metrics_sink::{MetricSample, MetricsSink};
use pos_ports::{PortError, PortName};

/// How many batches the in-memory queue holds before [`VmMetrics::record`] starts dropping.
///
/// A batch, not a sample: one `record` call is one queue slot. Bounded so a stalled backend
/// cannot grow memory without limit — dropping telemetry is the correct response, since the next
/// scrape is along shortly and a sample nobody can plot is worth less than a sale.
pub const QUEUE_CAPACITY: usize = 256;

/// `VictoriaMetrics`' default listen port, used when a URL omits one.
const DEFAULT_PORT: u16 = 8428;

/// Where a flushed batch goes.
///
/// The seam ([ADR-0031](../../../docs/adr/0031-cloud-adapter-transports.md)) that lets the port
/// contract be verified in process against a capturing implementation while production uses
/// [`HttpTransport`]. The risk in this adapter is the queueing above the transport, not the
/// transport itself.
pub trait MetricTransport: Send + Sync + 'static {
    /// Sends one batch onward. A telemetry failure is dropped by the caller, never retried onto
    /// the sales path.
    ///
    /// # Errors
    ///
    /// [`PortError::unavailable`] if the backend cannot be reached or refuses the batch.
    fn send(&self, samples: &[MetricSample]) -> impl Future<Output = Result<(), PortError>> + Send;
}

/// What travels the queue to the background flush task.
#[derive(Debug)]
enum Command {
    /// A batch to send onward.
    Batch(Vec<MetricSample>),
    /// A request to acknowledge once everything queued before it has been sent — how [`VmMetrics`]
    /// offers a deterministic flush without exposing the transport's timing.
    Flush(oneshot::Sender<()>),
}

/// A [`MetricsSink`] that never blocks the caller: samples enter a bounded queue and a background
/// task flushes them through a [`MetricTransport`].
///
/// Cloneable and shareable — every clone feeds the same queue and the same task.
#[derive(Debug, Clone)]
pub struct VmMetrics<T: MetricTransport> {
    commands: mpsc::Sender<Command>,
    transport: Arc<T>,
}

impl<T: MetricTransport> VmMetrics<T> {
    /// Builds a sink over `transport` and spawns its flush task on the current runtime.
    ///
    /// Public rather than test-only because it is also how a binary composes the sink over a
    /// non-HTTP transport, and how a test drives the port contract against a capturing one.
    #[must_use]
    pub fn with_transport(transport: T) -> Self {
        let transport = Arc::new(transport);
        let (commands, mut receiver) = mpsc::channel::<Command>(QUEUE_CAPACITY);
        let worker = Arc::clone(&transport);
        // FIFO with a single consumer, so a Flush acknowledged after the batches queued before it
        // proves those batches reached the transport — that is what makes `flush` deterministic.
        drop(tokio::spawn(async move {
            while let Some(command) = receiver.recv().await {
                match command {
                    Command::Batch(batch) => {
                        // Telemetry: a failed flush is dropped, not retried. Retrying onto the
                        // sales path is exactly what this port refuses to do.
                        let _ = worker.send(&batch).await;
                    }
                    Command::Flush(reply) => {
                        let _ = reply.send(());
                    }
                }
            }
        }));
        Self {
            commands,
            transport,
        }
    }

    /// The transport this sink flushes through.
    #[must_use]
    pub fn transport(&self) -> &T {
        &self.transport
    }

    /// Waits until everything queued so far has been sent onward.
    ///
    /// For an orderly shutdown, and for a test to read back what a batch became. Not on the hot
    /// path: [`Self::record`] never calls it.
    ///
    /// # Errors
    ///
    /// [`PortError::unavailable`] if the background flush task has stopped.
    pub async fn flush(&self) -> Result<(), PortError> {
        let (reply, done) = oneshot::channel();
        self.commands
            .send(Command::Flush(reply))
            .await
            .map_err(|_| worker_gone())?;
        done.await.map_err(|_| worker_gone())
    }
}

impl VmMetrics<HttpTransport> {
    /// Builds a sink that imports into the `VictoriaMetrics` at `base_url` (an `http://host:port`,
    /// with an optional path defaulting to `/api/v1/import`).
    ///
    /// # Errors
    ///
    /// [`PortError::invalid_argument`] if `base_url` is not a usable `http://` address.
    pub fn connect(base_url: &str) -> Result<Self, PortError> {
        Ok(Self::with_transport(HttpTransport::new(base_url)?))
    }
}

impl<T: MetricTransport> MetricsSink for VmMetrics<T> {
    async fn record(&self, samples: &[MetricSample]) -> Result<(), PortError> {
        if samples.is_empty() {
            return Ok(());
        }
        // `try_send`, never `send`: the whole point is not to block. A full queue drops the batch
        // and reports success, because the caller has nothing useful to do with the news and a `?`
        // here would put telemetry on the sales path.
        match self.commands.try_send(Command::Batch(samples.to_vec())) {
            Ok(()) | Err(mpsc::error::TrySendError::Full(_)) => Ok(()),
            Err(mpsc::error::TrySendError::Closed(_)) => Err(worker_gone()),
        }
    }
}

/// The background flush task is gone — only reachable if it panicked, which the flush path cannot
/// cause. Reported as unavailable rather than swallowed so a genuinely broken sink is visible.
fn worker_gone() -> PortError {
    PortError::unavailable(PortName::MetricsSink, "the metrics flush task has stopped")
}

/// A hand-rolled HTTP/1.1 client for `VictoriaMetrics`' JSON line import
/// ([ADR-0031](../../../docs/adr/0031-cloud-adapter-transports.md)): one endpoint, one POST, no
/// client crate. Plain `http://` only — the cloud reaches its own `VictoriaMetrics` over the private
/// network of its box, and any public path terminates TLS at the proxy (P8), not here.
#[derive(Debug, Clone)]
pub struct HttpTransport {
    host: String,
    port: u16,
    path: String,
}

impl HttpTransport {
    /// Parses an `http://host[:port][/path]` base URL.
    ///
    /// # Errors
    ///
    /// [`PortError::invalid_argument`] if the scheme is not `http://` or the port does not parse.
    pub fn new(base_url: &str) -> Result<Self, PortError> {
        let rest = base_url.strip_prefix("http://").ok_or_else(|| {
            PortError::invalid_argument(
                PortName::MetricsSink,
                "the metrics url must be http://host:port[/path]",
            )
        })?;
        let (authority, path) = match rest.find('/') {
            Some(index) => (
                rest.get(..index).unwrap_or(rest),
                rest.get(index..).unwrap_or("/api/v1/import"),
            ),
            None => (rest, "/api/v1/import"),
        };
        let (host, port) = match authority.rsplit_once(':') {
            Some((host, port)) => {
                let port = port.parse::<u16>().map_err(|error| {
                    PortError::invalid_argument(
                        PortName::MetricsSink,
                        "the metrics port is not a number",
                    )
                    .with_source(error)
                })?;
                (host.to_owned(), port)
            }
            None => (authority.to_owned(), DEFAULT_PORT),
        };
        if host.is_empty() {
            return Err(PortError::invalid_argument(
                PortName::MetricsSink,
                "the metrics url has no host",
            ));
        }
        Ok(Self {
            host,
            port,
            path: path.to_owned(),
        })
    }
}

impl MetricTransport for HttpTransport {
    async fn send(&self, samples: &[MetricSample]) -> Result<(), PortError> {
        if samples.is_empty() {
            return Ok(());
        }
        let body = encode(samples);
        let mut stream = TcpStream::connect((self.host.as_str(), self.port))
            .await
            .map_err(unreachable_backend)?;
        let head = format!(
            "POST {} HTTP/1.1\r\nHost: {}\r\nContent-Type: application/json\r\n\
             Content-Length: {}\r\nConnection: close\r\n\r\n",
            self.path,
            self.host,
            body.len()
        );
        stream
            .write_all(head.as_bytes())
            .await
            .map_err(unreachable_backend)?;
        stream
            .write_all(body.as_bytes())
            .await
            .map_err(unreachable_backend)?;
        stream.flush().await.map_err(unreachable_backend)?;

        // `Connection: close`, so the server closes after the response and `read_to_end` completes.
        let mut response = Vec::new();
        stream
            .read_to_end(&mut response)
            .await
            .map_err(unreachable_backend)?;
        if response_is_success(&response) {
            Ok(())
        } else {
            Err(PortError::unavailable(
                PortName::MetricsSink,
                "victoriametrics did not accept the import",
            ))
        }
    }
}

/// Encodes a batch as `VictoriaMetrics`' JSON line import: one object per line, the unit carried as a
/// label so no float is needed and a dashboard threshold is set against a known unit.
fn encode(samples: &[MetricSample]) -> String {
    let mut body = String::new();
    for sample in samples {
        let mut metric = Map::new();
        metric.insert(
            "__name__".to_owned(),
            Value::String(sample.name.as_str().to_owned()),
        );
        metric.insert(
            "unit".to_owned(),
            Value::String(sample.unit.as_label().to_owned()),
        );
        for (label, value) in &sample.labels {
            metric.insert(
                label.as_str().to_owned(),
                Value::String(value.as_str().to_owned()),
            );
        }
        let mut line = Map::new();
        line.insert("metric".to_owned(), Value::Object(metric));
        line.insert(
            "values".to_owned(),
            Value::Array(vec![Value::Number(sample.value.into())]),
        );
        line.insert(
            "timestamps".to_owned(),
            Value::Array(vec![Value::Number(
                sample.at.as_milliseconds_since_epoch().into(),
            )]),
        );
        // Every component is validated and finite, so serialising the object cannot fail; if it
        // somehow did, dropping the one sample is the right telemetry behaviour anyway.
        if let Ok(text) = serde_json::to_string(&Value::Object(line)) {
            body.push_str(&text);
            body.push('\n');
        }
    }
    body
}

/// Whether an HTTP response's status line is a 2xx.
fn response_is_success(response: &[u8]) -> bool {
    let line_end = response
        .iter()
        .position(|&byte| byte == b'\n')
        .unwrap_or(response.len());
    let status_line = response.get(..line_end).unwrap_or(response);
    matches!(
        status_line.split(|&byte| byte == b' ').nth(1),
        Some([b'2', ..])
    )
}

fn unreachable_backend(error: std::io::Error) -> PortError {
    PortError::unavailable(PortName::MetricsSink, "the metrics backend is unreachable")
        .with_source(error)
}
