// src/server.rs
use quinn::Endpoint;
use tokio_util::sync::CancellationToken;
use tokio::io::AsyncReadExt;
use std::{net::SocketAddr, str::FromStr, sync::{Arc, atomic::AtomicUsize}};
use tokio::net::TcpListener;
use tokio_rustls::TlsAcceptor;

use crate::proxy;
use crate::config::ServerConfig;
use crate::tls;
use crate::transport::{SnpConnection, QuicConnection, MuxConnection};

pub struct SnpServer {
    bind: String,
    tls_config: rustls::ServerConfig,
    transport: String,
    global_count: Arc<AtomicUsize>,
}

impl SnpServer {
    pub async fn new(config: ServerConfig, global_count: Arc<AtomicUsize>) -> Result<Self, Box<dyn std::error::Error>> {
        let tls_config = tls::load_rustls_server_config(&config.tls)?;

        Ok(SnpServer {
            bind: config.bind,
            tls_config,
            transport: config.transport,
            global_count,
        })
    }

    pub async fn run(&self) -> Result<(), Box<dyn std::error::Error>> {
        log::info!("[Server] Starting with transport: {}, bind: {}", self.transport, self.bind);
        if self.transport == "tcp" {
            self.run_mux().await
        } else {
            self.run_quic().await
        }
    }

    async fn run_quic(&self) -> Result<(), Box<dyn std::error::Error>> {
        let addr = SocketAddr::from_str(&self.bind)?;
        let quinn_config = tls::quinn_server_config(&self.tls_config)?;
        let endpoint = Endpoint::server(quinn_config, addr)?;
        log::info!("[Server] QUIC listening on {}", addr);

        while let Some(conn) = endpoint.accept().await {
            log::info!("[Server] QUIC incoming connection");
            let gc = self.global_count.clone();
            tokio::spawn(async move {
                match conn.await {
                    Ok(connection) => {
                        log::info!("[Server] QUIC connection established from {:?}", connection.remote_address());
                        let quic_conn = QuicConnection::new(connection.clone());
                        if let Err(e) = handle_connection(quic_conn, gc).await {
                            log::error!("[Server] Connection error: {}", e);
                        }
                    }
                    Err(e) => {
                        log::error!("[Server] QUIC connection failed: {}", e);
                    }
                }
            });
        }
        Ok(())
    }

    async fn run_mux(&self) -> Result<(), Box<dyn std::error::Error>> {
        let acceptor = TlsAcceptor::from(Arc::new(self.tls_config.clone()));
        let listener = TcpListener::bind(&self.bind).await?;
        log::info!("[Server] TCP+Mux listening on {}", self.bind);

        loop {
            let (tcp_stream, addr) = listener.accept().await?;
            log::info!("[Server] TCP incoming connection from {}", addr);
            let acceptor = acceptor.clone();
            let gc = self.global_count.clone();

            tokio::spawn(async move {
                match acceptor.accept(tcp_stream).await {
                    Ok(tls_stream) => {
                        log::info!("[Server] TLS handshake success");
                        let mux_conn = MuxConnection::new_server(tls_stream);
                        if let Err(e) = handle_connection(mux_conn, gc).await {
                            log::error!("[Server] Connection error: {}", e);
                        }
                    }
                    Err(e) => {
                        log::error!("[Server] TLS handshake failed: {}", e);
                    }
                }
            });
        }
    }
}

async fn handle_connection<C: SnpConnection>(connection: C, global_count: Arc<AtomicUsize>) -> Result<(), Box<dyn std::error::Error>> {
    log::info!("[Server] Handling connection, waiting for streams...");
    let token = CancellationToken::new();
    loop {
        match connection.accept_bi().await {
            Ok(Some(stream)) => {
                log::info!("[Server] Accepted new stream");
                let cmd_stream = stream;
                // Read command byte
                let mut recv = cmd_stream;
                let cmd = recv.read_u8().await?;
                log::info!("[Server] Received command: {}", cmd);
                match cmd {
                    10 => {
                        // forward
                        let gc = global_count.clone();
                        let tc = token.clone();
                        // Need to pass the stream that was already read from
                        tokio::spawn(async move {
                            if let Err(e) = proxy::handle_forward_with_cmd(recv, cmd, gc, tc).await {
                                log::error!("[Server] Forward error: {}", e);
                            }
                        });
                    }
                    20 => {
                        // 反向代理
                        let gc = global_count.clone();
                        let tc = token.clone();
                        let conn = connection.clone();
                        tokio::spawn(async move {
                            if let Err(e) = handle_reverse_proxy(conn, recv, gc, tc).await {
                                log::error!("[Server] Reverse proxy error: {}", e);
                            }
                        });
                    }
                    21 => {
                        // 反向socks5
                        let gc = global_count.clone();
                        let tc = token.clone();
                        let conn = connection.clone();
                        tokio::spawn(async move {
                            if let Err(e) = handle_reverse_socks5(conn, recv, gc, tc).await {
                                log::error!("[Server] Reverse socks5 error: {}", e);
                            }
                        });
                    }
                    _ => {
                        log::warn!("[Server] Unknown command: {}", cmd);
                    }
                }
            }
            Ok(None) => {
                log::info!("[Server] Connection closed by peer");
                token.cancel();
                return Ok(())
            }
            Err(e) => {
                log::error!("[Server] Accept bi error: {}", e);
                token.cancel();
                return Err(e);
            }
        }
    }
}

async fn handle_reverse_proxy<C: SnpConnection>(
    connection: C,
    mut recv: C::Stream,
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

    log::info!("[Server] Start forward client from {} to {}", bind, forward);
    _ = proxy::start_forward_client(connection, bind.to_string(), forward.to_string(), global_count, token).await;

    Ok(())
}

async fn handle_reverse_socks5<C: SnpConnection>(
    connection: C,
    mut recv: C::Stream,
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

    log::info!("[Server] Start socks5 client {}", bind);
    _ = proxy::start_socks5_client(connection, bind.to_string(), global_count, token).await;

    Ok(())
}