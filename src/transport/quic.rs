use quinn::{Connection, SendStream, RecvStream};
use tokio::io::{AsyncRead, AsyncWrite};
use std::pin::Pin;
use std::task::{Context, Poll};
use crate::transport::SnpConnection;

/// Combined QUIC bidirectional stream
pub struct QuicBiStream {
    send: SendStream,
    recv: RecvStream,
}

impl AsyncRead for QuicBiStream {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut tokio::io::ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.get_mut().recv).poll_read(cx, buf)
    }
}

impl AsyncWrite for QuicBiStream {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        // quinn SendStream uses WriteError, convert to io::Error
        match Pin::new(&mut self.get_mut().send).poll_write(cx, buf) {
            Poll::Ready(Ok(n)) => Poll::Ready(Ok(n)),
            Poll::Ready(Err(e)) => Poll::Ready(Err(std::io::Error::new(
                std::io::ErrorKind::Other,
                e.to_string()
            ))),
            Poll::Pending => Poll::Pending,
        }
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        match Pin::new(&mut self.get_mut().send).poll_flush(cx) {
            Poll::Ready(Ok(())) => Poll::Ready(Ok(())),
            Poll::Ready(Err(e)) => Poll::Ready(Err(std::io::Error::new(
                std::io::ErrorKind::Other,
                e.to_string()
            ))),
            Poll::Pending => Poll::Pending,
        }
    }

    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        match Pin::new(&mut self.get_mut().send).poll_shutdown(cx) {
            Poll::Ready(Ok(())) => Poll::Ready(Ok(())),
            Poll::Ready(Err(e)) => Poll::Ready(Err(std::io::Error::new(
                std::io::ErrorKind::Other,
                e.to_string()
            ))),
            Poll::Pending => Poll::Pending,
        }
    }
}

/// QUIC Connection wrapper
#[derive(Clone)]
pub struct QuicConnection {
    conn: Connection,
}

impl QuicConnection {
    pub fn new(conn: Connection) -> Self {
        Self { conn }
    }

    pub fn inner(&self) -> &Connection {
        &self.conn
    }
}

impl SnpConnection for QuicConnection {
    type Stream = QuicBiStream;

    async fn open_bi(&self) -> Result<Self::Stream, Box<dyn std::error::Error + Send + Sync>> {
        let (send, recv) = self.conn.open_bi().await?;
        Ok(QuicBiStream { send, recv })
    }

    async fn accept_bi(&self) -> Result<Option<Self::Stream>, Box<dyn std::error::Error + Send + Sync>> {
        match self.conn.accept_bi().await {
            Ok((send, recv)) => Ok(Some(QuicBiStream { send, recv })),
            Err(_) => Ok(None),
        }
    }

    async fn closed(&self) {
        self.conn.closed().await;
    }
}