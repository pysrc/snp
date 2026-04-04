use std::sync::{Arc, atomic::{AtomicUsize, Ordering}};
use tokio::{io::AsyncWriteExt, net::TcpStream};
use tokio_util::sync::CancellationToken;

use crate::transport::SnpConnection;

pub async fn start_forward_client<C: SnpConnection>(
    connection: C,
    local_addr: String,
    remote_addr: String,
    global_count: Arc<AtomicUsize>,
    token: CancellationToken,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    log::info!("[Forward] Binding to {}", local_addr);
    let listener = match tokio::net::TcpListener::bind(&local_addr).await {
        Ok(l) => l,
        Err(e) => {
            log::error!("[Forward] Failed to bind {}: {}", local_addr, e);
            return Err(e.into());
        }
    };
    log::info!("[Forward] Listening on {}, forwarding to {}", local_addr, remote_addr);

    loop {
        tokio::select! {
            _ = token.cancelled() => {
                log::info!("[Forward] Cancelled for {}", local_addr);
                return Ok(());
            }
            _ = connection.closed() => {
                log::info!("[Forward] Connection closed for {}", local_addr);
                return Ok(());
            }
            tcp_stream = listener.accept() => {
                match tcp_stream {
                    Ok((tcp_stream, addr)) => {
                        log::info!("[Forward] New connection from {} on {}", addr, local_addr);
                        let connection = connection.clone();
                        let remote_addr = remote_addr.clone();
                        let gc = global_count.clone();
                        tokio::spawn(async move {
                            if let Err(e) = handle_tcp_to_stream(tcp_stream, connection, remote_addr.clone(), gc).await {
                                log::error!("[Forward] Error handling {}: {}", addr, e);
                            }
                        });
                    }
                    Err(e) => {
                        log::error!("[Forward] Accept error on {}: {}", local_addr, e);
                    }
                }
            }
        }
    }
}

async fn handle_tcp_to_stream<C: SnpConnection>(
    mut tcp_stream: TcpStream,
    connection: C,
    remote_addr: String,
    global_count: Arc<AtomicUsize>,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    log::info!("[Forward] Opening stream to {}", remote_addr);
    let stream = connection.open_bi().await.map_err(|e| {
        log::error!("[Forward] Failed to open stream: {}", e);
        e.to_string()
    })?;

    // 发送目标地址
    let res = remote_addr.as_bytes();
    let l = res.len() as u16;
    let (mut stream_reader, mut stream_writer) = tokio::io::split(stream);
    stream_writer.write_u8(10).await?; // 10 代表代理
    stream_writer.write_all(&l.to_be_bytes()).await?;
    stream_writer.write_all(res).await?;
    stream_writer.flush().await?;
    log::info!("[Forward] Sent target address {}", remote_addr);

    // 双向数据传输
    let (mut tcp_reader, mut tcp_writer) = tcp_stream.split();

    global_count.fetch_add(1, Ordering::Relaxed);
    log::info!("[Forward] Starting bidirectional copy for {}", remote_addr);
    tokio::select! {
        _ = tokio::io::copy(&mut tcp_reader, &mut stream_writer) => {
            log::debug!("[Forward] TCP->Stream ended for {}", remote_addr);
        }
        _ = tokio::io::copy(&mut stream_reader, &mut tcp_writer) => {
            log::debug!("[Forward] Stream->TCP ended for {}", remote_addr);
        }
    }
    let _ = stream_writer.shutdown().await;
    let _ = tcp_writer.shutdown().await;
    global_count.fetch_sub(1, Ordering::Relaxed);
    log::info!("[Forward] Done for {}, connections: {}", remote_addr, global_count.load(Ordering::Relaxed));

    Ok(())
}