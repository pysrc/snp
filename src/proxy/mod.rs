mod forward;
mod socks5;

use std::sync::{Arc, atomic::{AtomicUsize, Ordering}};

pub use forward::start_forward_client;
pub use socks5::start_socks5_client;
use tokio::{io::AsyncWriteExt, net::TcpStream, select};
use tokio_util::sync::CancellationToken;

// 统一处理代理
pub async fn handle_forward(send: quinn::SendStream, 
    mut recv: quinn::RecvStream, 
    global_count: Arc<AtomicUsize>,
    token: CancellationToken,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut lenbuf = [0u8; 2];
    recv.read_exact(&mut lenbuf).await?;
    let len = u16::from_be_bytes(lenbuf);
    let mut buf = vec![0u8; len as usize];
    recv.read_exact(&mut buf).await?;
    let addr = std::str::from_utf8(&buf)?;
    log::info!("Forward to {}", addr);
    let to = tokio::net::lookup_host(addr).await?.next().unwrap();
    let mut tcp_stream = TcpStream::connect(to).await?;

    global_count.fetch_add(1, Ordering::Relaxed);
    // 双向数据传输
    let (mut tcp_reader, mut tcp_writer) = tcp_stream.split();
    let (mut quic_reader, mut quic_writer) = (recv, send);
    
    // let t1 = async {
    //     let _ = tokio::io::copy(&mut tcp_reader, &mut quic_writer).await;
    //     quic_writer.shutdown().await
    // };
    // let t2 = async {
    //     let _ = tokio::io::copy(&mut quic_reader, &mut tcp_writer).await;
    //     tcp_writer.shutdown().await
    // };
    select! {
        _ = token.cancelled() => {}
        _ = tokio::io::copy(&mut tcp_reader, &mut quic_writer) => {}
        _ = tokio::io::copy(&mut quic_reader, &mut tcp_writer) => {}
    }
    quic_writer.shutdown().await?;
    tcp_writer.shutdown().await?;
    global_count.fetch_sub(1, Ordering::Relaxed);
    log::info!("Forward done, current connections: {}", global_count.load(Ordering::Relaxed));
    Ok(())
}