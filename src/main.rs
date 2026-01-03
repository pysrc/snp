// src/main.rs
mod config;
mod tls;
mod server;
mod client;
mod proxy;

use clap::Parser;
use config::Config;
use serde_yaml;
use tokio::{fs::File, io::AsyncWriteExt};
use tokio_util::sync::CancellationToken;
use std::{fs, str::FromStr, sync::{Arc, atomic::AtomicUsize}};

/// Config
#[derive(Parser, Debug)]
#[command(version, about, long_about = None)]
struct Args {
    /// Path of the config file
    #[arg(short, long, default_value_t = String::from_str("snp-config.yml").unwrap())]
    config: String,
    /// Generate tls cert and key
    #[arg(short, long, default_value_t = false)]
    generate_tls: bool,
}

async fn gentls() -> std::io::Result<()> {
    let subject_alt_names = vec!["snp".to_string()];
    let cert = rcgen::generate_simple_self_signed(subject_alt_names).unwrap();


    let mut file = File::create("cert.pem").await?;
    file.write_all(cert.serialize_pem().unwrap().as_bytes()).await?;
    file.flush().await?;

    let mut file = File::create("key.pem").await?;
    file.write_all(cert.serialize_private_key_pem().as_bytes()).await?;
    file.flush().await?;
    Ok(())
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    simple_logger::init_with_level(log::Level::Info)?;

    let a = Args::parse();
    
    if a.generate_tls {
        _ = gentls().await?;
        return Ok(());
    }

    let config_content = fs::read_to_string(a.config.as_str())?;
    let config: Config = serde_yaml::from_str(&config_content)?;
    
    let scfg = config.clone();

    // 全局连接统计
    let global_count = Arc::new(AtomicUsize::new(0));

    // 根据配置启动服务端或客户端
    let sgc = global_count.clone();
    tokio::spawn(async {
        if let Some(server_config) = scfg.server {
            let tls_config = tls::load_tls_config(&server_config.tls).unwrap();
            let server = server::SnpServer::new(server_config.bind, tls_config, sgc).await.unwrap();
            _ = server.run().await;
        }
    });
    
    let token = CancellationToken::new();
    let cgc = global_count.clone();
    let tc = token.clone();
    tokio::spawn(async {
        if let Some(client_config) = config.client {
            let client = client::SnpClient::new(client_config.bind, client_config.server, client_config.server_ip_version, client_config.tls.clone(), client_config.rules.clone(), cgc, tc).await.unwrap(); // 需要定义server_addr
            _ = client.run().await;
        }
    });


    tokio::signal::ctrl_c().await?;
    token.cancel();
    
    Ok(())
}