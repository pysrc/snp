use quinn::{ClientConfig, ServerConfig};
use rustls::pki_types::{CertificateDer, PrivatePkcs8KeyDer};
use std::{io::Cursor, sync::Arc, time::Duration};

use crate::config::TlsConfig;

pub fn load_tls_config(tls_config: &TlsConfig) -> Result<ServerConfig, Box<dyn std::error::Error>> {
    let mut br = Cursor::new(&tls_config.cert);
    let cetrs = rustls_pemfile::certs(&mut br)?;

    match &tls_config.key {
        Some(key) => {
            let mut brk = Cursor::new(&key);
            let keys = rustls_pemfile::pkcs8_private_keys(&mut brk)?;

            let cert_der = CertificateDer::from(cetrs[0].clone());
            let priv_key = PrivatePkcs8KeyDer::from(keys[0].clone());

            let mut server_config =
                ServerConfig::with_single_cert(vec![cert_der.clone()], priv_key.into())?;

            let mut transport = quinn::TransportConfig::default();
            // 关键配置：防止闲置超时
            transport
            .max_idle_timeout(Some(Duration::from_secs(60).try_into()?))// 最大闲置 60s
            .keep_alive_interval(None); // 服务端不发送心跳

            server_config.transport = Arc::new(transport);


            Ok(server_config)
        }
        None => {
            std::panic!("No key provided");
        }
    }
    
}

pub fn client_tls_config(tls_config: &TlsConfig) -> Result<ClientConfig, Box<dyn std::error::Error>> {
    let mut br = Cursor::new(&tls_config.cert);
    let cetrs = rustls_pemfile::certs(&mut br).unwrap();
    let cert_der = CertificateDer::from(cetrs[0].clone());

    let mut certs = rustls::RootCertStore::empty();
    certs.add(cert_der)?;

    let mut transport = quinn::TransportConfig::default();
    transport
        .max_idle_timeout(Some(Duration::from_secs(60).try_into()?))
        .keep_alive_interval(Some(Duration::from_secs(20)));

    let mut cc = ClientConfig::with_root_certificates(Arc::new(certs))?;
    cc.transport_config(Arc::new(transport));

    Ok(cc)
}