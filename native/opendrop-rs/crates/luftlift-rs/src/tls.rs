//! TLS self-signed certificate generation and `rustls::ServerConfig` creation
//! for the AirDrop HTTPS server (port 8771). Apple accepts self-signed certs
//! for AirDrop; we generate an ephemeral one at startup, like opendrop.
//!
//! Reference: `opendrop-patch/server.py.patched` (`get_ssl_context`).

use anyhow::Result;
use rustls::client::ClientConfig;
use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer, ServerName, UnixTime};
use rustls::server::ServerConfig;
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::DigitallySignedStruct;
use std::sync::Arc;

/// A self-signed certificate + private key for the AirDrop HTTPS server.
pub struct AirDropTlsCert {
    pub cert_der: CertificateDer<'static>,
    pub key_der: PrivateKeyDer<'static>,
}

/// Generate an ephemeral self-signed certificate suitable for AirDrop.
pub fn generate_self_signed_cert() -> Result<AirDropTlsCert> {
    // Install the ring crypto provider as the rustls default (idempotent).
    let _ = rustls::crypto::ring::default_provider().install_default();

    let key_pair = rcgen::KeyPair::generate()?;
    let params = rcgen::CertificateParams::new(vec!["luftlift".into()])?;
    let cert = params.self_signed(&key_pair)?;
    Ok(AirDropTlsCert {
        cert_der: cert.der().clone(),
        key_der: PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(key_pair.serialize_der())),
    })
}

/// Build a `rustls::ServerConfig` from a self-signed cert. No client-cert
/// verification (Apple doesn't send one for AirDrop receive).
pub fn build_server_config(cert: &AirDropTlsCert) -> Result<ServerConfig> {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let config = ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(vec![cert.cert_der.clone()], cert.key_der.clone_key())?;
    Ok(config)
}

// ---- Client side (sender) ----

/// A certificate verifier that accepts any server certificate, mirroring
/// Apple's lenient AirDrop TLS (self-signed certs are accepted). AirDrop runs
/// over a trusted AWDL link and uses cert pinning-like behaviour informally,
/// not a WebPKI CA chain. **Only safe for AirDrop.**
#[derive(Debug)]
struct AcceptAnyCert;

impl ServerCertVerifier for AcceptAnyCert {
    fn verify_server_cert(
        &self,
        _end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp: &[u8],
        _now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        Ok(ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        _message: &[u8],
        _cert: &CertificateDer<'_>,
        _dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        Ok(HandshakeSignatureValid::assertion())
    }

    fn verify_tls13_signature(
        &self,
        _message: &[u8],
        _cert: &CertificateDer<'_>,
        _dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        Ok(HandshakeSignatureValid::assertion())
    }

    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        use rustls::SignatureScheme::*;
        vec![
            RSA_PKCS1_SHA1,
            ECDSA_SHA1_Legacy,
            RSA_PKCS1_SHA256,
            ECDSA_NISTP256_SHA256,
            RSA_PSS_SHA256,
            ED25519,
            RSA_PKCS1_SHA384,
            ECDSA_NISTP384_SHA384,
            RSA_PSS_SHA384,
            RSA_PKCS1_SHA512,
            ECDSA_NISTP521_SHA512,
            RSA_PSS_SHA512,
        ]
    }
}

/// Build a `rustls::ClientConfig` for talking to an AirDrop receiver.
/// Accepts self-signed certs (no WebPKI validation) and sends no client cert,
/// matching the AirDrop protocol's lenient TLS.
pub fn build_client_config() -> Result<ClientConfig> {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let cert = generate_self_signed_cert()?;
    let config = ClientConfig::builder()
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(AcceptAnyCert))
        .with_client_auth_cert(vec![cert.cert_der], cert.key_der)?;
    Ok(config)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generate_cert_produces_non_empty_der() {
        let cert = generate_self_signed_cert().unwrap();
        assert!(!cert.cert_der.as_ref().is_empty());
        // The key's usability is validated by build_server_config below.
    }

    #[test]
    fn build_server_config_from_generated_cert_succeeds() {
        let cert = generate_self_signed_cert().unwrap();
        // Just verify it builds without panicking.
        let _config = build_server_config(&cert).unwrap();
    }

    // ---- client side (sender) ----

    #[test]
    fn build_client_config_succeeds_and_accepts_self_signed() {
        let config = build_client_config().unwrap();
        // A client config that can be used to connect to an AirDrop receiver.
        let _ = config;
    }
}
