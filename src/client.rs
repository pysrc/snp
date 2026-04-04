// src/client.rs
use quinn::{Endpoint, Connection as QuinnConnection};
use tokio_rustls::TlsConnector;
use tokio::{io::{AsyncReadExt, AsyncWriteExt}, select, net::TcpStream};
use tokio_util::sync::CancellationToken;
use std::{net::SocketAddr, str::FromStr, sync::{Arc, atomic::AtomicUsize}};

use crate::{config::ClientConfig, proxy, tls};
use crate::transport::{SnpConnection, QuicConnection, MuxConnection};

pub struct SnpClient {
    bind: String,
    server_addr: String,
    server_ip_version: String,
    tlsconfig: crate::config::TlsConfig,
    rules: Vec<crate::config::Rule>,
    transport: String,
    global_count: Arc<AtomicUsize>,
    token: CancellationToken,
}

impl SnpClient {
    pub async fn new(config: ClientConfig,
        global_count: Arc<AtomicUsize>,
        token: CancellationToken,
    ) -> Result<Self, Box<dyn std::error::Error>> {
        Ok(SnpClient {
            bind: config.bind,
            server_addr: config.server,
            server_ip_version: config.server_ip_version,
            tlsconfig: config.tls,
            rules: config.rules,
            transport: config.transport,
            global_count,
            token,
        })
    }

    pub async fn run(&self) -> Result<(), Box<dyn std::error::Error>> {
        log::info!("[Client] Starting with transport: {}, server: {}", self.transport, self.server_addr);
        if self.transport == "tcp" {
            self.run_mux().await
        } else {
            self.run_quic().await
        }
    }

    async fn run_quic(&self) -> Result<(), Box<dyn std::error::Error>> {
        let addr = SocketAddr::from_str(&self.bind)?;
        let mut endpoint = Endpoint::client(addr)?;
        let tc = tls::client_tls_config(&self.tlsconfig)?;
        endpoint.set_default_client_config(tc);
        log::info!("[Client] QUIC endpoint bound to {}", addr);

        let connection = self.connect_quic(&endpoint).await?;
        log::info!("[Client] QUIC connected to server");
        let quic_conn = QuicConnection::new(connection.clone());
        let token = self.token.clone();

        // 反向代理服务器
        let gc = self.global_count.clone();
        let tc = token.clone();
        tokio::spawn(start_reverse_server(quic_conn.clone(), gc, tc));

        // 启动所有配置的规则
        self.start_rules(quic_conn).await;

        Ok(())
    }

    async fn connect_quic(&self, endpoint: &Endpoint) -> Result<QuinnConnection, Box<dyn std::error::Error>> {
        let ids = tokio::net::lookup_host(&self.server_addr).await?;
        for to in ids {
            if (self.server_ip_version == "ipv4" && to.is_ipv4()) ||
                (self.server_ip_version == "ipv6" && to.is_ipv6()) {
                log::info!("[Client] Connecting to server {:?}", to);
                match endpoint.connect(to, "snp")?.await {
                    Ok(conn) => {
                        log::info!("[Client] QUIC connection established");
                        return Ok(conn);
                    }
                    Err(e) => {
                        log::error!("[Client] QUIC connection failed: {}", e);
                        return Err(e.into());
                    }
                }
            }
        }
        Err("Can not connect to server".into())
    }

    async fn run_mux(&self) -> Result<(), Box<dyn std::error::Error>> {
        let rustls_config = tls::load_rustls_client_config(&self.tlsconfig)?;
        let connector = TlsConnector::from(Arc::new(rustls_config));

        let tcp_stream = self.connect_tcp().await?;
        log::info!("[Client] TCP connected, performing TLS handshake");

        let server_name = rustls::pki_types::ServerName::try_from("snp")
            .map_err(|e| { log::error!("[Client] Invalid server name: {:?}", e); "Invalid server name" })?;

        let tls_stream = connector.connect(server_name, tcp_stream).await?;
        log::info!("[Client] TLS handshake success, creating yamux connection");

        let mux_conn = MuxConnection::new_client(tls_stream);
        log::info!("[Client] Yamux connection created");

        let token = self.token.clone();

        // 反向代理服务器
        let gc = self.global_count.clone();
        let tc = token.clone();
        tokio::spawn(start_reverse_server(mux_conn.clone(), gc, tc));

        // 启动所有配置的规则
        self.start_rules(mux_conn).await;

        Ok(())
    }

    async fn connect_tcp(&self) -> Result<TcpStream, Box<dyn std::error::Error>> {
        let ids = tokio::net::lookup_host(&self.server_addr).await?;
        for to in ids {
            if (self.server_ip_version == "ipv4" && to.is_ipv4()) ||
                (self.server_ip_version == "ipv6" && to.is_ipv6()) {
                log::info!("[Client] Connecting TCP to {:?}", to);
                match TcpStream::connect(to).await {
                    Ok(stream) => {
                        log::info!("[Client] TCP connected");
                        return Ok(stream);
                    }
                    Err(e) => {
                        log::error!("[Client] TCP connect failed: {}", e);
                        return Err(e.into());
                    }
                }
            }
        }
        Err("Can not connect to server".into())
    }

    async fn start_rules<C: SnpConnection>(&self, connection: C) {
        let token = self.token.clone();
        log::info!("[Client] Starting {} rules", self.rules.len());
        for (i, rule) in self.rules.iter().enumerate() {
            match &rule {
                crate::config::Rule::ClientToServerForward { bind, forward } => {
                    log::info!("[Client] Rule {}: forward {} -> {}", i, bind, forward);
                    let gc = self.global_count.clone();
                    let tc = token.clone();
                    tokio::spawn(proxy::start_forward_client(connection.clone(), bind.clone(), forward.clone(), gc, tc));
                }
                crate::config::Rule::ClientToServerSocks5 { bind } => {
                    log::info!("[Client] Rule {}: socks5 on {}", i, bind);
                    let gc = self.global_count.clone();
                    let tc = token.clone();
                    tokio::spawn(proxy::start_socks5_client(connection.clone(), bind.clone(), gc, tc));
                }
                crate::config::Rule::ServerToClientForward { bind, forward } => {
                    log::info!("[Client] Rule {}: reverse forward {} -> {}", i, bind, forward);
                    tokio::spawn(start_reverse_forward(connection.clone(), bind.clone(), forward.clone()));
                }
                crate::config::Rule::ServerToClientSocks5 { bind } => {
                    log::info!("[Client] Rule {}: reverse socks5 on {}", i, bind);
                    tokio::spawn(start_reverse_socks5(connection.clone(), bind.clone()));
                }
            }
        }
    }
}

pub async fn start_reverse_server<C: SnpConnection>(connection: C,
    global_count: Arc<AtomicUsize>,
    token: CancellationToken,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    log::info!("[Client] Reverse server started, waiting for streams");
    loop {
        select! {
            _ = token.cancelled() => {
                log::info!("[Client] Reverse server stopped (cancelled)");
                return Ok(());
            }
            _ = connection.closed() => {
                log::info!("[Client] Reverse server stopped (connection closed)");
                return Ok(());
            }
            stream = connection.accept_bi() => {
                match stream {
                    Ok(Some(s)) => {
                        log::info!("[Client] Reverse server accepted stream");
                        let mut recv = s;
                        let cmd = recv.read_u8().await?;
                        log::info!("[Client] Reverse server received command: {}", cmd);
                        match cmd {
                            10 => {
                                // forward
                                let gc = global_count.clone();
                                let tc = token.clone();
                                tokio::spawn(async move {
                                    if let Err(e) = proxy::handle_forward_with_cmd(recv, cmd, gc, tc).await {
                                        log::error!("[Client] Forward error: {}", e);
                                    }
                                });
                            }
                            _ => {
                                log::warn!("[Client] Unknown command: {}", cmd);
                            }
                        }
                    }
                    Ok(None) => {
                        log::info!("[Client] Reverse server: connection closed");
                        return Ok(())
                    }
                    Err(e) => {
                        log::error!("[Client] Accept bi error: {}", e);
                        token.cancel();
                        return Err(e.into());
                    }
                }
            }
        }
    }
}

pub async fn start_reverse_forward<C: SnpConnection>(
    connection: C,
    local_addr: String,
    remote_addr: String,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    log::info!("[Client] Opening stream for reverse forward {} -> {}", local_addr, remote_addr);
    let mut quic_send = connection.open_bi().await?;
    log::info!("[Client] Stream opened, sending reverse forward request");

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
    log::info!("[Client] Reverse forward request sent");
    Ok(())
}

pub async fn start_reverse_socks5<C: SnpConnection>(
    connection: C,
    local_addr: String,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    log::info!("[Client] Opening stream for reverse socks5 on {}", local_addr);
    let mut quic_send = connection.open_bi().await?;
    log::info!("[Client] Stream opened, sending reverse socks5 request");

    quic_send.write_u8(21).await?; // 21 代表反向socks5
    // 发送绑定地址
    {
        let res = local_addr.as_bytes();
        let l = res.len() as u16;
        quic_send.write_all(&l.to_be_bytes()).await?;
        quic_send.write_all(res).await?;
    }
    quic_send.flush().await?;
    log::info!("[Client] Reverse socks5 request sent");
    Ok(())
}