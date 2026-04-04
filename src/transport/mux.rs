use yamux::{Connection, Config, Mode, Stream};
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::net::TcpStream;
use tokio_util::compat::TokioAsyncReadCompatExt;
use futures::io::{AsyncRead as FuturesAsyncRead, AsyncWrite as FuturesAsyncWrite};
use std::sync::Arc;
use std::pin::Pin;
use std::task::{Context, Poll};
use crate::transport::SnpConnection;

/// Yamux bidirectional stream wrapper - adapts futures-io to tokio-io
pub struct MuxStream {
    stream: Stream,
}

impl AsyncRead for MuxStream {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut tokio::io::ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        let mut read_buf = buf.initialize_unfilled();
        let stream = &mut self.get_mut().stream;
        // yamux::Stream implements futures::io::AsyncRead
        match FuturesAsyncRead::poll_read(Pin::new(stream), cx, &mut read_buf) {
            Poll::Ready(Ok(n)) => {
                buf.advance(n);
                Poll::Ready(Ok(()))
            }
            Poll::Ready(Err(e)) => Poll::Ready(Err(e)),
            Poll::Pending => Poll::Pending,
        }
    }
}

impl AsyncWrite for MuxStream {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        // yamux::Stream implements futures::io::AsyncWrite
        FuturesAsyncWrite::poll_write(Pin::new(&mut self.get_mut().stream), cx, buf)
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        FuturesAsyncWrite::poll_flush(Pin::new(&mut self.get_mut().stream), cx)
    }

    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        FuturesAsyncWrite::poll_close(Pin::new(&mut self.get_mut().stream), cx)
    }
}

/// Type alias for server TLS stream
type ServerTlsStream = tokio_rustls::server::TlsStream<TcpStream>;
/// Type alias for client TLS stream
type ClientTlsStream = tokio_rustls::client::TlsStream<TcpStream>;

/// Mux Connection wrapper for server
#[derive(Clone)]
pub struct MuxServerConnection {
    /// Channel to request opening a new stream
    open_tx: tokio::sync::mpsc::Sender<tokio::sync::oneshot::Sender<Result<MuxStream, yamux::ConnectionError>>>,
    /// Channel to receive incoming streams
    accept_rx: Arc<tokio::sync::Mutex<tokio::sync::mpsc::Receiver<Stream>>>,
    /// Closed flag
    closed: Arc<tokio::sync::Mutex<bool>>,
}

/// Mux Connection wrapper for client
#[derive(Clone)]
pub struct MuxClientConnection {
    /// Channel to request opening a new stream
    open_tx: tokio::sync::mpsc::Sender<tokio::sync::oneshot::Sender<Result<MuxStream, yamux::ConnectionError>>>,
    /// Channel to receive incoming streams
    accept_rx: Arc<tokio::sync::Mutex<tokio::sync::mpsc::Receiver<Stream>>>,
    /// Closed flag
    closed: Arc<tokio::sync::Mutex<bool>>,
}

/// Create yamux config
fn create_config() -> Config {
    Config::default()
}

/// Driver task that polls the yamux connection
async fn run_driver<S>(
    mut conn: Connection<S>,
    mut open_rx: tokio::sync::mpsc::Receiver<tokio::sync::oneshot::Sender<Result<MuxStream, yamux::ConnectionError>>>,
    accept_tx: tokio::sync::mpsc::Sender<Stream>,
    closed: Arc<tokio::sync::Mutex<bool>>,
    label: &'static str,
) where
    S: futures::io::AsyncRead + futures::io::AsyncWrite + Unpin + Send + 'static,
{
    log::info!("[{}] Driver task started", label);

    // Pending open request that needs to be fulfilled when outbound becomes available
    let mut pending_request: Option<tokio::sync::oneshot::Sender<Result<MuxStream, yamux::ConnectionError>>> = None;

    loop {
        // Use a flag to track if we have a pending request for poll_fn
        let has_pending = pending_request.is_some();

        let result = futures::future::poll_fn(|cx| {
            // First check if we have a pending request that needs outbound stream
            if has_pending {
                match conn.poll_new_outbound(cx) {
                    Poll::Ready(Ok(stream)) => {
                        log::info!("[{}] Opened pending outbound stream id={}", label, stream.id());
                        return Poll::Ready(DriverEvent::PendingOpen(Ok(stream)));
                    }
                    Poll::Ready(Err(e)) => {
                        log::error!("[{}] Failed to open pending stream: {}", label, e);
                        return Poll::Ready(DriverEvent::PendingOpen(Err(e)));
                    }
                    Poll::Pending => {}
                }
            }

            // Poll for inbound streams
            match conn.poll_next_inbound(cx) {
                Poll::Ready(Some(Ok(stream))) => {
                    log::info!("[{}] Accepted inbound stream id={}", label, stream.id());
                    return Poll::Ready(DriverEvent::Inbound(stream));
                }
                Poll::Ready(Some(Err(e))) => {
                    log::error!("[{}] Connection error: {}", label, e);
                    return Poll::Ready(DriverEvent::Error);
                }
                Poll::Ready(None) => {
                    log::info!("[{}] Connection closed", label);
                    return Poll::Ready(DriverEvent::Closed);
                }
                Poll::Pending => {}
            }

            // Only poll for new open requests if we don't have a pending one
            if !has_pending {
                match open_rx.poll_recv(cx) {
                    Poll::Ready(Some(response_tx)) => {
                        log::info!("[{}] Received open stream request", label);
                        // Try to open a new outbound stream immediately
                        match conn.poll_new_outbound(cx) {
                            Poll::Ready(Ok(stream)) => {
                                log::info!("[{}] Opened outbound stream id={}", label, stream.id());
                                return Poll::Ready(DriverEvent::NewOpen(response_tx, Ok(stream)));
                            }
                            Poll::Ready(Err(e)) => {
                                log::error!("[{}] Failed to open stream: {}", label, e);
                                return Poll::Ready(DriverEvent::NewOpen(response_tx, Err(e)));
                            }
                            Poll::Pending => {
                                // Can't store in poll_fn, return a marker event
                                return Poll::Ready(DriverEvent::NeedPending(response_tx));
                            }
                        }
                    }
                    Poll::Ready(None) => {
                        log::info!("[{}] Open channel closed", label);
                        return Poll::Ready(DriverEvent::Closed);
                    }
                    Poll::Pending => {}
                }
            }

            Poll::Pending
        }).await;

        match result {
            DriverEvent::Inbound(stream) => {
                if accept_tx.send(stream).await.is_err() {
                    log::info!("[{}] Accept channel closed", label);
                    break;
                }
            }
            DriverEvent::NewOpen(response_tx, stream_result) => {
                let _ = response_tx.send(stream_result.map(|s| MuxStream { stream: s }));
            }
            DriverEvent::NeedPending(response_tx) => {
                // Store for next iteration
                pending_request = Some(response_tx);
            }
            DriverEvent::PendingOpen(stream_result) => {
                // Take the pending request
                if let Some(response_tx) = pending_request.take() {
                    let _ = response_tx.send(stream_result.map(|s| MuxStream { stream: s }));
                }
            }
            DriverEvent::Error | DriverEvent::Closed => {
                break;
            }
        }
    }

    *closed.lock().await = true;
    log::info!("[{}] Driver task ended", label);
}

enum DriverEvent {
    Inbound(Stream),
    NewOpen(tokio::sync::oneshot::Sender<Result<MuxStream, yamux::ConnectionError>>, Result<Stream, yamux::ConnectionError>),
    NeedPending(tokio::sync::oneshot::Sender<Result<MuxStream, yamux::ConnectionError>>),
    PendingOpen(Result<Stream, yamux::ConnectionError>),
    Error,
    Closed,
}

impl MuxServerConnection {
    pub fn new(tls_stream: ServerTlsStream) -> Self {
        log::info!("[MuxServer] Creating yamux server connection");
        let compat_stream = tls_stream.compat();
        let conn = Connection::new(compat_stream, create_config(), Mode::Server);

        let (open_tx, open_rx) = tokio::sync::mpsc::channel::<tokio::sync::oneshot::Sender<Result<MuxStream, yamux::ConnectionError>>>(64);
        let (accept_tx, accept_rx) = tokio::sync::mpsc::channel::<Stream>(64);
        let closed = Arc::new(tokio::sync::Mutex::new(false));

        // Spawn driver task
        tokio::spawn(run_driver(conn, open_rx, accept_tx, closed.clone(), "MuxServer"));

        Self {
            open_tx,
            accept_rx: Arc::new(tokio::sync::Mutex::new(accept_rx)),
            closed,
        }
    }
}

impl MuxClientConnection {
    pub fn new(tls_stream: ClientTlsStream) -> Self {
        log::info!("[MuxClient] Creating yamux client connection");
        let compat_stream = tls_stream.compat();
        let conn = Connection::new(compat_stream, create_config(), Mode::Client);

        let (open_tx, open_rx) = tokio::sync::mpsc::channel::<tokio::sync::oneshot::Sender<Result<MuxStream, yamux::ConnectionError>>>(64);
        let (accept_tx, accept_rx) = tokio::sync::mpsc::channel::<Stream>(64);
        let closed = Arc::new(tokio::sync::Mutex::new(false));

        // Spawn driver task
        tokio::spawn(run_driver(conn, open_rx, accept_tx, closed.clone(), "MuxClient"));

        Self {
            open_tx,
            accept_rx: Arc::new(tokio::sync::Mutex::new(accept_rx)),
            closed,
        }
    }
}

impl SnpConnection for MuxServerConnection {
    type Stream = MuxStream;

    async fn open_bi(&self) -> Result<Self::Stream, Box<dyn std::error::Error + Send + Sync>> {
        log::info!("[MuxServer] Opening stream via channel");
        let (response_tx, response_rx) = tokio::sync::oneshot::channel();
        self.open_tx.send(response_tx).await.map_err(|e| e.to_string())?;
        match response_rx.await {
            Ok(Ok(stream)) => Ok(stream),
            Ok(Err(e)) => Err(e.into()),
            Err(_) => Err("Driver task closed".into()),
        }
    }

    async fn accept_bi(&self) -> Result<Option<Self::Stream>, Box<dyn std::error::Error + Send + Sync>> {
        let mut rx = self.accept_rx.lock().await;
        match rx.recv().await {
            Some(stream) => {
                log::info!("[MuxServer] Accepted stream id={}", stream.id());
                Ok(Some(MuxStream { stream }))
            }
            None => {
                log::info!("[MuxServer] No more streams");
                Ok(None)
            }
        }
    }

    async fn closed(&self) {
        loop {
            if *self.closed.lock().await {
                log::info!("[MuxServer] Connection closed");
                return;
            }
            tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;
        }
    }
}

impl SnpConnection for MuxClientConnection {
    type Stream = MuxStream;

    async fn open_bi(&self) -> Result<Self::Stream, Box<dyn std::error::Error + Send + Sync>> {
        log::info!("[MuxClient] Opening stream via channel");
        let (response_tx, response_rx) = tokio::sync::oneshot::channel();
        self.open_tx.send(response_tx).await.map_err(|e| e.to_string())?;
        match response_rx.await {
            Ok(Ok(stream)) => Ok(stream),
            Ok(Err(e)) => Err(e.into()),
            Err(_) => Err("Driver task closed".into()),
        }
    }

    async fn accept_bi(&self) -> Result<Option<Self::Stream>, Box<dyn std::error::Error + Send + Sync>> {
        let mut rx = self.accept_rx.lock().await;
        match rx.recv().await {
            Some(stream) => {
                log::info!("[MuxClient] Accepted stream id={}", stream.id());
                Ok(Some(MuxStream { stream }))
            }
            None => {
                log::info!("[MuxClient] No more streams");
                Ok(None)
            }
        }
    }

    async fn closed(&self) {
        loop {
            if *self.closed.lock().await {
                log::info!("[MuxClient] Connection closed");
                return;
            }
            tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;
        }
    }
}

/// Unified connection enum for both client and server
#[derive(Clone)]
pub enum MuxConnection {
    Server(MuxServerConnection),
    Client(MuxClientConnection),
}

impl MuxConnection {
    pub fn new_server(tls_stream: ServerTlsStream) -> Self {
        MuxConnection::Server(MuxServerConnection::new(tls_stream))
    }

    pub fn new_client(tls_stream: ClientTlsStream) -> Self {
        MuxConnection::Client(MuxClientConnection::new(tls_stream))
    }
}

impl SnpConnection for MuxConnection {
    type Stream = MuxStream;

    async fn open_bi(&self) -> Result<Self::Stream, Box<dyn std::error::Error + Send + Sync>> {
        match self {
            MuxConnection::Server(c) => c.open_bi().await,
            MuxConnection::Client(c) => c.open_bi().await,
        }
    }

    async fn accept_bi(&self) -> Result<Option<Self::Stream>, Box<dyn std::error::Error + Send + Sync>> {
        match self {
            MuxConnection::Server(c) => c.accept_bi().await,
            MuxConnection::Client(c) => c.accept_bi().await,
        }
    }

    async fn closed(&self) {
        match self {
            MuxConnection::Server(c) => c.closed().await,
            MuxConnection::Client(c) => c.closed().await,
        }
    }
}