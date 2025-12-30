// src/config.rs
use serde::{Deserialize, Serialize};

#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct Config {
    pub server: Option<ServerConfig>,
    pub client: Option<ClientConfig>,
}

#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct ServerConfig {
    pub bind: String,
    pub tls: TlsConfig,
}

#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct ClientConfig {
    pub bind: String,
    pub server: String,
    pub tls: TlsConfig,
    pub rules: Vec<Rule>,
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