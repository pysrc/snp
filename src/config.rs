// src/config.rs
use serde::{Deserialize, Serialize};

#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct Config {
    pub server: Option<ServerConfig>,
    pub client: Option<ClientConfig>,
}

fn default_transport() -> String {
    "quic".to_string()
}

#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct ServerConfig {
    pub bind: String,
    pub tls: TlsConfig,
    #[serde(default = "default_transport")]
    pub transport: String,  // "quic" or "tcp"
}

fn default_ipv4() -> String {
    "ipv4".to_string()
}

fn default_reconnect_interval() -> u64 {
    5 // 默认5秒
}

fn default_max_reconnect_attempts() -> u32 {
    0 // 0表示无限重试
}

#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct ClientConfig {
    pub bind: String,
    #[serde(rename = "server-ip-version", default = "default_ipv4")]
    pub server_ip_version: String,
    pub server: String,
    pub tls: TlsConfig,
    pub rules: Vec<Rule>,
    #[serde(default = "default_transport")]
    pub transport: String,  // "quic" or "tcp"
    #[serde(rename = "reconnect-interval", default = "default_reconnect_interval")]
    pub reconnect_interval: u64,  // 重连间隔秒数
    #[serde(rename = "max-reconnect-attempts", default = "default_max_reconnect_attempts")]
    pub max_reconnect_attempts: u32,  // 最大重连次数，0表示无限
}

#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct TlsConfig {
    pub key: Option<String>,
    pub cert: String,
}

#[derive(Debug, Deserialize, Serialize, Clone)]
#[serde(tag = "type", content = "value")]
pub enum Rule {
    #[serde(rename = "cs/forward")]
    ClientToServerForward { bind: String, forward: String },
    #[serde(rename = "cs/socks5")]
    ClientToServerSocks5 { bind: String },
    #[serde(rename = "sc/forward")]
    ServerToClientForward { bind: String, forward: String },
    #[serde(rename = "sc/socks5")]
    ServerToClientSocks5 { bind: String },
}