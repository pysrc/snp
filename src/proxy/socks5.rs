// src/proxy/socks5.rs
use quinn::Connection;
use tokio_util::sync::CancellationToken;
use std::sync::{Arc, atomic::{AtomicUsize, Ordering}};
use tokio::{io::{AsyncReadExt, AsyncWriteExt}, net::TcpListener, select};

pub async fn start_socks5_client(
    connection: Connection,
    bind_addr: String,
    global_count: Arc<AtomicUsize>,
    token: CancellationToken,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let listener = TcpListener::bind(bind_addr).await?;
    
    loop {
        select! {
            _ = token.cancelled() => {
                log::info!("SOCKS5 client cancelled");
                return Ok(());
            }
            _ = connection.closed() => {
                log::info!("SOCKS5 client connection closed");
                return Ok(());
            }

            ac = listener.accept() => {
                let (tcp_stream, _) = ac?;
                let connection = connection.clone();
                let gc = global_count.clone();
                tokio::spawn(async move {
                    if let Err(e) = handle_socks5_connection(tcp_stream, connection, gc).await {
                        log::error!("SOCKS5 error: {}", e);
                    }
                });
            }
        }
    }
}

async fn handle_socks5_connection(
    mut tcp_stream: tokio::net::TcpStream,
    connection: Connection,
    global_count: Arc<AtomicUsize>,
) -> Result<(), Box<dyn std::error::Error>> {
    // SOCKS5 握手
    let mut buf = [0u8; 256];
    let _ = tcp_stream.read(&mut buf).await?;
    
    // 简化的握手处理 - 仅接受无认证
    if buf[0] != 0x05 {
        return Err("Invalid SOCKS5 version".into());
    }
    
    // 发送握手响应
    tcp_stream.write_all(&[0x05, 0x00]).await?;
    
    // 读取连接请求
    let _ = tcp_stream.read(&mut buf).await?;
    
    if buf[0] != 0x05 || buf[1] != 0x01 {
        return Err("Invalid SOCKS5 request".into());
    }
    
    // 解析目标地址
    let addr_type = buf[3];
    let (target_addr, _) = match addr_type {
        0x01 => { // IPv4
            let ip = std::net::Ipv4Addr::new(buf[4], buf[5], buf[6], buf[7]);
            let port = ((buf[8] as u16) << 8) | (buf[9] as u16);
            (format!("{}:{}", ip, port), 10)
        },
        0x03 => { // 域名
            let domain_len = buf[4] as usize;
            let domain = String::from_utf8_lossy(&buf[5..5 + domain_len]);
            let port = ((buf[5 + domain_len] as u16) << 8) | (buf[5 + domain_len + 1] as u16);
            (format!("{}:{}", domain, port), 5 + domain_len + 2)
        },
        0x04 => { // IPv6
            return Err("IPv6 not supported".into());
        },
        _ => return Err("Invalid address type".into()),
    };
    
    // 连接到远程地址通过 QUIC
    let (mut send, recv) = connection.open_bi().await?;
    
    // 发送目标地址
    let t = target_addr.as_bytes();
    let l = t.len() as u16;
    send.write_u8(10).await?; // 10 代表代理
    send.write_all(&l.to_be_bytes()).await?;
    send.write_all(t).await?;
    send.flush().await?;
    
    // 发送 SOCKS5 响应
    tcp_stream.write_all(&[0x05, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00]).await?;
    
    // 双向数据传输
    let (mut tcp_reader, mut tcp_writer) = tcp_stream.split();
    let (mut quic_reader, mut quic_writer) = (recv, send);
    
    global_count.fetch_add(1, Ordering::Relaxed);
    
    tokio::try_join!(
        tokio::io::copy(&mut tcp_reader, &mut quic_writer),
        tokio::io::copy(&mut quic_reader, &mut tcp_writer)
    )?;
    global_count.fetch_sub(1, Ordering::Relaxed);
    log::info!("Connection closed count: {}", global_count.load(Ordering::Relaxed));
    
    Ok(())
}