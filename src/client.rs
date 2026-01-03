// src/client.rs
use quinn::{Endpoint, Connection};
use tokio::{io::{AsyncReadExt, AsyncWriteExt}, select};
use tokio_util::sync::CancellationToken;
use std::{net::SocketAddr, str::FromStr, sync::{Arc, atomic::AtomicUsize}};

use crate::{config::{Rule, TlsConfig}, proxy, tls};

pub struct SnpClient {
    endpoint: Endpoint,
    server_addr: String,
    server_ip_version: String,
    rules: Vec<Rule>,
    global_count: Arc<AtomicUsize>,
    token: CancellationToken,
}

impl SnpClient {
    pub async fn new(bind: String, 
        server_addr: String, 
        server_ip_version: String,
        tlsconfig: TlsConfig, 
        rules: Vec<Rule>,
        global_count: Arc<AtomicUsize>,
        token: CancellationToken,
    ) -> Result<Self, Box<dyn std::error::Error>> {
        let addr = SocketAddr::from_str(&bind)?;
        let mut endpoint = Endpoint::client(addr)?;
        let tc = tls::client_tls_config(&tlsconfig)?;
        endpoint.set_default_client_config(tc);
        
        Ok(SnpClient {
            endpoint,
            server_addr,
            server_ip_version,
            rules,
            global_count,
            token,
        })
    }

    pub async fn connect_to_server(&self) -> Result<Connection, Box<dyn std::error::Error>> {
        let ids = tokio::net::lookup_host(&self.server_addr).await?;
        for to in ids {
            if (self.server_ip_version == "ipv4" && to.is_ipv4()) ||
                (self.server_ip_version == "ipv6" && to.is_ipv6()) {
                log::info!("Connect to server {:?}", to);
                let connection = self.endpoint.connect(to, "snp")?.await?;
                return Ok(connection);
            }
        }
        Err("Can not connect to server".into())
    }

    pub async fn run(&self) -> Result<(), Box<dyn std::error::Error>> {
        let connection = self.connect_to_server().await?;
        let token = self.token.clone();
        // 反向代理服务器
        let gc = self.global_count.clone();
        let tc = token.clone();
        tokio::spawn(start_reverse_server(connection.clone(), gc, tc));
        // 启动所有配置的规则
        for rule in &self.rules {
            match &rule {
                Rule::ClientToServerForward { bind, forward } => {
                    log::info!("Start forward client from {} to {}", bind, forward);
                    let gc = self.global_count.clone();
                    let tc = token.clone();
                    tokio::spawn(proxy::start_forward_client(connection.clone(), bind.clone(), forward.clone(), gc, tc));
                }
                Rule::ClientToServerSocks5 { bind } => {
                    log::info!("Start socks5 client on {}", bind);
                    let gc = self.global_count.clone();
                    let tc = token.clone();
                    tokio::spawn(proxy::start_socks5_client(connection.clone(), bind.clone(), gc, tc));
                }
                Rule::ServerToClientForward { bind, forward } => {
                    log::info!("Start reverse forward from {} to {}", bind, forward);
                    tokio::spawn(start_reverse_forward(connection.clone(), bind.clone(), forward.clone()));
                }
                Rule::ServerToClientSocks5 { bind } => {
                    log::info!("Start reverse socks5 on {}", bind);
                    tokio::spawn(start_reverse_socks5(connection.clone(), bind.clone()));
                }
            }
        }
        Ok(())
    }
}

pub async fn start_reverse_server(connection: Connection, 
    global_count: Arc<AtomicUsize>,
    token: CancellationToken,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    loop {
        select! {
            _ = token.cancelled() => {
                log::info!("Reverse server stopped");
                return Ok(());
            }
            _ = connection.closed() => {
                log::info!("Reverse server stopped");
                return Ok(());
            }
            bi = connection.accept_bi() => {
                match bi {
                    Ok((send, mut recv)) => {
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
    }
}

pub async fn start_reverse_forward(
    connection: Connection,
    local_addr: String,
    remote_addr: String,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    // 反向代理
    let (mut quic_send, _) = connection.open_bi().await?;
    
    quic_send.write_u8(20).await?; // 20 代表反向代理

    // 发送绑定地址
    {
        let res = local_addr.as_bytes();
        let l = res.len() as u16;
        quic_send.write_all(&l.to_be_bytes()).await?;
        quic_send.write_all(res).await?;
    }
    // 发送目标地址
    {
        let res = remote_addr.as_bytes();
        let l = res.len() as u16;
        quic_send.write_all(&l.to_be_bytes()).await?;
        quic_send.write_all(res).await?;
    }
    quic_send.flush().await?;
    Ok(())
}

pub async fn start_reverse_socks5(
    connection: Connection,
    local_addr: String,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    // 反向代理
    let (mut quic_send, _) = connection.open_bi().await?;
    quic_send.write_u8(21).await?; // 21 代表反向socks5
    // 发送绑定地址
    {
        let res = local_addr.as_bytes();
        let l = res.len() as u16;
        quic_send.write_all(&l.to_be_bytes()).await?;
        quic_send.write_all(res).await?;
    }
    quic_send.flush().await?;
    Ok(())
}