// src/server.rs
use quinn::{Endpoint, ServerConfig};
use tokio_util::sync::CancellationToken;
use std::{net::SocketAddr, str::FromStr, sync::{Arc, atomic::AtomicUsize}};
use tokio::io::AsyncReadExt;

use crate::proxy;

pub struct SnpServer {
    endpoint: Endpoint,
    global_count: Arc<AtomicUsize>,
}

impl SnpServer {
    pub async fn new(bind_addr: String, tls_config: ServerConfig, global_count: Arc<AtomicUsize>) -> Result<Self, Box<dyn std::error::Error>> {
        let addr = SocketAddr::from_str(&bind_addr)?;
        let endpoint = Endpoint::server(tls_config.clone(), addr)?;
        
        Ok(SnpServer {
            endpoint,
            global_count,
        })
    }

    pub async fn run(&self) -> Result<(), Box<dyn std::error::Error>> {
        while let Some(conn) = self.endpoint.accept().await {
            let gc = self.global_count.clone();
            tokio::spawn(async move {
                if let Err(e) = handle_connection(conn.await.unwrap(), gc).await {
                    log::error!("Connection error: {}", e);
                }
            });
        }
        Ok(())
    }
}

async fn handle_connection(connection: quinn::Connection, global_count: Arc<AtomicUsize>) -> Result<(), Box<dyn std::error::Error>> {
    let token = CancellationToken::new();
    loop {
        match connection.accept_bi().await {
            Ok((send, mut recv)) => {
                // todo
                let cmd = recv.read_u8().await?;
                match cmd {
                    10 => {
                        // forward
                        let gc = global_count.clone();
                        let tc = token.clone();
                        tokio::spawn(async move {
                            if let Err(e) = proxy::handle_forward(send, recv, gc, tc).await {
                                log::error!("Forward error: {}", e);
                            }
                        });
                    }
                    20 => {
                        // 反向代理
                        let c = connection.clone();
                        let gc = global_count.clone();
                        let tc = token.clone();
                        tokio::spawn(async move {
                            if let Err(e) = handle_reverse_proxy(c, recv, gc, tc).await {
                                log::error!("Reverse proxy error: {}", e);
                            }
                        });
                    }
                    21 => {
                        // 反向socks5
                        let c = connection.clone();
                        let gc = global_count.clone();
                        let tc = token.clone();
                        tokio::spawn(async move {
                            if let Err(e) = handle_reverse_socks5(c, recv, gc, tc).await {
                                log::error!("Reverse socks5 error: {}", e);
                            }
                        });
                    }
                    _=> {
                        log::info!("Unknown command: {}", cmd);
                    }
                }
            }
            Err(e) => {
                log::error!("Accept bi error: {}", e);
                token.cancel();
                return Err(e.into());
            }
        }
    }
}

async fn handle_reverse_proxy(
    connection: quinn::Connection, 
    mut recv: quinn::RecvStream, 
    global_count: Arc<AtomicUsize>,
    token: CancellationToken,
) -> Result<(), Box<dyn std::error::Error>> {

    // 读取绑定地址
    let mut lenbuf = [0u8; 2];
    recv.read_exact(&mut lenbuf).await?;
    let len = u16::from_be_bytes(lenbuf);
    let mut buf = vec![0u8; len as usize];
    recv.read_exact(&mut buf).await?;
    let bind = std::str::from_utf8(&buf)?;

    // 读取目标地址
    recv.read_exact(&mut lenbuf).await?;
    let len = u16::from_be_bytes(lenbuf);
    let mut buf = vec![0u8; len as usize];
    recv.read_exact(&mut buf).await?;
    let forward = std::str::from_utf8(&buf)?;

    log::info!("Start forward client from {} to {}", bind, forward);
    _ = proxy::start_forward_client(connection, bind.to_string(), forward.to_string(), global_count, token).await;

    Ok(())
}

async fn handle_reverse_socks5(
    connection: quinn::Connection, 
    mut recv: quinn::RecvStream, 
    global_count: Arc<AtomicUsize>,
    token: CancellationToken,
) -> Result<(), Box<dyn std::error::Error>> {

    // 读取绑定地址
    let mut lenbuf = [0u8; 2];
    recv.read_exact(&mut lenbuf).await?;
    let len = u16::from_be_bytes(lenbuf);
    let mut buf = vec![0u8; len as usize];
    recv.read_exact(&mut buf).await?;
    let bind = std::str::from_utf8(&buf)?;

    log::info!("Start socks5 client {}", bind);
    _ = proxy::start_socks5_client(connection, bind.to_string(), global_count, token).await;

    Ok(())
}