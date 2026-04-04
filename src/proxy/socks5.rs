use tokio_util::sync::CancellationToken;
use std::sync::{Arc, atomic::{AtomicUsize, Ordering}};
use tokio::{io::{AsyncReadExt, AsyncWriteExt}, net::TcpListener};

use crate::transport::SnpConnection;

pub async fn start_socks5_client<C: SnpConnection>(
    connection: C,
    bind_addr: String,
    global_count: Arc<AtomicUsize>,
    token: CancellationToken,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    log::info!("[Socks5] Binding to {}", bind_addr);
    let listener = match TcpListener::bind(&bind_addr).await {
        Ok(l) => l,
        Err(e) => {
            log::error!("[Socks5] Failed to bind {}: {}", bind_addr, e);
            return Err(e.into());
        }
    };
    log::info!("[Socks5] Listening on {}", bind_addr);

    loop {
        tokio::select! {
            _ = token.cancelled() => {
                log::info!("[Socks5] Cancelled for {}", bind_addr);
                return Ok(());
            }
            _ = connection.closed() => {
                log::info!("[Socks5] Connection closed for {}", bind_addr);
                return Ok(());
            }

            ac = listener.accept() => {
                match ac {
                    Ok((tcp_stream, addr)) => {
                        log::info!("[Socks5] New connection from {} on {}", addr, bind_addr);
                        let connection = connection.clone();
                        let gc = global_count.clone();
                        tokio::spawn(async move {
                            if let Err(e) = handle_socks5_connection(tcp_stream, connection, gc).await {
                                log::error!("[Socks5] Error handling {}: {}", addr, e);
                            }
                        });
                    }
                    Err(e) => {
                        log::error!("[Socks5] Accept error on {}: {}", bind_addr, e);
                    }
                }
            }
        }
    }
}

async fn handle_socks5_connection<C: SnpConnection>(
    mut tcp_stream: tokio::net::TcpStream,
    connection: C,
    global_count: Arc<AtomicUsize>,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    // SOCKS5 握手
    let mut buf = [0u8; 256];
    let _ = tcp_stream.read(&mut buf).await?;

    // 简化的握手处理 - 仅接受无认证
    if buf[0] != 0x05 {
        log::error!("[Socks5] Invalid version: {}", buf[0]);
        return Err("Invalid SOCKS5 version".into());
    }

    // 发送握手响应
    tcp_stream.write_all(&[0x05, 0x00]).await?;
    log::debug!("[Socks5] Sent handshake response");

    // 读取连接请求
    let _ = tcp_stream.read(&mut buf).await?;

    if buf[0] != 0x05 || buf[1] != 0x01 {
        log::error!("[Socks5] Invalid request: version={}, cmd={}", buf[0], buf[1]);
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
            log::error!("[Socks5] IPv6 not supported");
            return Err("IPv6 not supported".into());
        },
        _ => {
            log::error!("[Socks5] Invalid address type: {}", addr_type);
            return Err("Invalid address type".into());
        }
    };

    log::info!("[Socks5] Target: {}", target_addr);

    // 连接到远程地址通过隧道
    let stream = connection.open_bi().await.map_err(|e| {
        log::error!("[Socks5] Failed to open stream: {}", e);
        e.to_string()
    })?;

    // 发送目标地址
    let t = target_addr.as_bytes();
    let l = t.len() as u16;
    let (mut stream_reader, mut stream_writer) = tokio::io::split(stream);
    stream_writer.write_u8(10).await?; // 10 代表代理
    stream_writer.write_all(&l.to_be_bytes()).await?;
    stream_writer.write_all(t).await?;
    stream_writer.flush().await?;
    log::info!("[Socks5] Sent target address to tunnel");

    // 发送 SOCKS5 响应
    tcp_stream.write_all(&[0x05, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00]).await?;
    log::debug!("[Socks5] Sent SOCKS5 response");

    // 双向数据传输
    let (mut tcp_reader, mut tcp_writer) = tcp_stream.split();

    global_count.fetch_add(1, Ordering::Relaxed);
    log::info!("[Socks5] Starting bidirectional copy for {}", target_addr);

    tokio::select! {
        _ = tokio::io::copy(&mut tcp_reader, &mut stream_writer) => {
            log::debug!("[Socks5] TCP->Stream ended for {}", target_addr);
        }
        _ = tokio::io::copy(&mut stream_reader, &mut tcp_writer) => {
            log::debug!("[Socks5] Stream->TCP ended for {}", target_addr);
        }
    }
    global_count.fetch_sub(1, Ordering::Relaxed);
    log::info!("[Socks5] Done for {}, connections: {}", target_addr, global_count.load(Ordering::Relaxed));

    Ok(())
}