mod forward;
mod socks5;

use std::sync::{Arc, atomic::{AtomicUsize, Ordering}};
use tokio::{io::{AsyncWriteExt, AsyncReadExt}, net::TcpStream};
use tokio_util::sync::CancellationToken;
use tokio::io::{AsyncRead, AsyncWrite};

pub use forward::start_forward_client;
pub use socks5::start_socks5_client;

// 处理已经读取了cmd的情况
pub async fn handle_forward_with_cmd<S: AsyncRead + AsyncWrite + Send + Unpin>(
    mut send: S,
    _cmd: u8,
    global_count: Arc<AtomicUsize>,
    token: CancellationToken,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut lenbuf = [0u8; 2];
    send.read_exact(&mut lenbuf).await?;
    let len = u16::from_be_bytes(lenbuf);
    let mut buf = vec![0u8; len as usize];
    send.read_exact(&mut buf).await?;
    let addr = std::str::from_utf8(&buf)?;
    log::info!("[Proxy] Forward to {}", addr);

    // 添加DNS解析超时
    let to = match tokio::time::timeout(
        tokio::time::Duration::from_secs(10),
        tokio::net::lookup_host(addr)
    ).await {
        Ok(result) => match result?.next() {
            Some(t) => t,
            None => {
                log::error!("[Proxy] Failed to resolve {}", addr);
                return Err("Failed to resolve address".into());
            }
        },
        Err(_) => {
            log::error!("[Proxy] DNS resolution timeout for {}", addr);
            return Err("DNS resolution timeout".into());
        }
    };
    log::info!("[Proxy] Resolved {} to {}", addr, to);

    // 添加TCP连接超时
    let mut tcp_stream = match tokio::time::timeout(
        tokio::time::Duration::from_secs(10),
        TcpStream::connect(to)
    ).await {
        Ok(result) => match result {
            Ok(s) => s,
            Err(e) => {
                log::error!("[Proxy] Failed to connect to {}: {}", addr, e);
                return Err(e.into());
            }
        },
        Err(_) => {
            log::error!("[Proxy] Connection timeout for {}", addr);
            return Err("Connection timeout".into());
        }
    };
    log::info!("[Proxy] Connected to {}", addr);

    global_count.fetch_add(1, Ordering::Relaxed);
    // 双向数据传输
    let (mut tcp_reader, mut tcp_writer) = tcp_stream.split();
    let (mut stream_reader, mut stream_writer) = tokio::io::split(&mut send);

    log::info!("[Proxy] Starting bidirectional copy for {}", addr);
    tokio::select! {
        _ = token.cancelled() => {
            log::info!("[Proxy] Forward cancelled for {}", addr);
        }
        res = tokio::io::copy(&mut tcp_reader, &mut stream_writer) => {
            log::info!("[Proxy] TCP->Stream copy ended for {}: {:?}", addr, res);
        }
        res = tokio::io::copy(&mut stream_reader, &mut tcp_writer) => {
            log::info!("[Proxy] Stream->TCP copy ended for {}: {:?}", addr, res);
        }
    }
    let _ = stream_writer.shutdown().await;
    let _ = tcp_writer.shutdown().await;
    global_count.fetch_sub(1, Ordering::Relaxed);
    log::info!("[Proxy] Forward done for {}, current connections: {}", addr, global_count.load(Ordering::Relaxed));
    Ok(())
}