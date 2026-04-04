use quinn::{ClientConfig as QuinnClientConfig, ServerConfig as QuinnServerConfig};
use rustls::pki_types::{CertificateDer, PrivatePkcs8KeyDer};
use std::{io::Cursor, sync::Arc, time::Duration};

use crate::config::TlsConfig;

/// Load rustls ServerConfig from TlsConfig
pub fn load_rustls_server_config(tls_config: &TlsConfig) -> Result<rustls::ServerConfig, Box<dyn std::error::Error>> {
    let mut br = Cursor::new(&tls_config.cert);
    let certs = rustls_pemfile::certs(&mut br)?;

    match &tls_config.key {
        Some(key) => {
            let mut brk = Cursor::new(&key);
            let keys = rustls_pemfile::pkcs8_private_keys(&mut brk)?;

            let cert_chain: Vec<CertificateDer> = certs.into_iter().map(CertificateDer::from).collect();
            let priv_key = PrivatePkcs8KeyDer::from(keys[0].clone());

            let server_config = rustls::ServerConfig::builder()
                .with_no_client_auth()
                .with_single_cert(cert_chain, priv_key.into())?;

            Ok(server_config)
        }
        None => {
            Err("No key provided".into())
        }
    }
}

/// Convert rustls::ServerConfig to quinn ServerConfig
pub fn quinn_server_config(rustls_config: &rustls::ServerConfig) -> Result<QuinnServerConfig, Box<dyn std::error::Error>> {
    let server_config = QuinnServerConfig::with_crypto(Arc::new(
        quinn::crypto::rustls::QuicServerConfig::try_from(rustls_config.clone())?
    ));

    let mut transport = quinn::TransportConfig::default();
    transport
        .max_idle_timeout(Some(Duration::from_secs(60).try_into()?))
        .keep_alive_interval(None);

    let mut config = server_config;
    config.transport = Arc::new(transport);

    Ok(config)
}

/// Legacy function for compatibility
pub fn load_tls_config(tls_config: &TlsConfig) -> Result<QuinnServerConfig, Box<dyn std::error::Error>> {
    let rustls_config = load_rustls_server_config(tls_config)?;
    quinn_server_config(&rustls_config)
}

pub fn client_tls_config(tls_config: &TlsConfig) -> Result<QuinnClientConfig, Box<dyn std::error::Error>> {
    let mut br = Cursor::new(&tls_config.cert);
    let certs = rustls_pemfile::certs(&mut br).unwrap();
    let cert_der = CertificateDer::from(certs[0].clone());

    let mut certs = rustls::RootCertStore::empty();
    certs.add(cert_der)?;

    let mut transport = quinn::TransportConfig::default();
    transport
        .max_idle_timeout(Some(Duration::from_secs(60).try_into()?))
        .keep_alive_interval(Some(Duration::from_secs(20)));

    let mut cc = QuinnClientConfig::with_root_certificates(Arc::new(certs))?;
    cc.transport_config(Arc::new(transport));

    Ok(cc)
}

/// Load rustls ClientConfig for mux transport
pub fn load_rustls_client_config(tls_config: &TlsConfig) -> Result<rustls::ClientConfig, Box<dyn std::error::Error>> {
    let mut br = Cursor::new(&tls_config.cert);
    let certs = rustls_pemfile::certs(&mut br).unwrap();
    let cert_der = CertificateDer::from(certs[0].clone());

    let mut root_certs = rustls::RootCertStore::empty();
    root_certs.add(cert_der)?;

    let client_config = rustls::ClientConfig::builder()
        .with_root_certificates(Arc::new(root_certs))
        .with_no_client_auth();

    Ok(client_config)
}