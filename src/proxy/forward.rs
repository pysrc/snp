use std::sync::{Arc, atomic::{AtomicUsize, Ordering}};

// src/proxy/forward.rs
use quinn::Connection;
use tokio::{io::AsyncWriteExt, net::TcpStream, select};
use tokio_util::sync::CancellationToken;

pub async fn start_forward_client(
    connection: Connection,
    local_addr: String,
    remote_addr: String,
    global_count: Arc<AtomicUsize>,
    token: CancellationToken,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let listener = tokio::net::TcpListener::bind(local_addr).await?;
    loop {
        select! {
            _ = token.cancelled() => {
                log::info!("Forward client cancelled");
                return Ok(());
            }
            _ = connection.closed() => {
                log::info!("Forward client connection closed");
                return Ok(());
            }
            tcp_stream = listener.accept() => {
                let (tcp_stream, _) = tcp_stream?;
                let connection = connection.clone();
                let remote_addr = remote_addr.clone();
                let gc = global_count.clone();
                tokio::spawn(async move {
                    if let Err(e) = handle_tcp_to_quic(tcp_stream, connection, remote_addr, gc).await {
                        log::error!("Forward error: {}", e);
                    }
                });
            }
        }
    }
}

async fn handle_tcp_to_quic(
    mut tcp_stream: TcpStream,
    connection: Connection,
    remote_addr: String,
    global_count: Arc<AtomicUsize>,
) -> Result<(), Box<dyn std::error::Error>> {
    let (mut quic_send, quic_recv) = connection.open_bi().await?;
    
    // 发送目标地址
    let res = remote_addr.as_bytes();
    let l = res.len() as u16;
    quic_send.write_u8(10).await?; // 10 代表代理
    quic_send.write_all(&l.to_be_bytes()).await?;
    quic_send.write_all(res).await?;
    quic_send.flush().await?;
    
    // 双向数据传输
    let (mut tcp_reader, mut tcp_writer) = tcp_stream.split();
    let (mut quic_reader, mut quic_writer) = (quic_recv, quic_send);
    
    global_count.fetch_add(1, Ordering::Relaxed);
    tokio::try_join!(
        tokio::io::copy(&mut tcp_reader, &mut quic_writer),
        tokio::io::copy(&mut quic_reader, &mut tcp_writer)
    )?;
    global_count.fetch_sub(1, Ordering::Relaxed);
    log::info!("Forward done, current connections: {}", global_count.load(Ordering::Relaxed));
    
    Ok(())
}