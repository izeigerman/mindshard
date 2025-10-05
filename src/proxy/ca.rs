use anyhow::Result;
use async_trait::async_trait;
use http::uri::Authority;
use moka::future::Cache;
use openssl::{
    asn1::{Asn1Integer, Asn1Time},
    bn::BigNum,
    hash::MessageDigest,
    pkey::{PKey, Private},
    rand,
    x509::{extension::SubjectAlternativeName, X509Builder, X509NameBuilder, X509},
};
use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs1KeyDer};
use std::{
    sync::Arc,
    time::{Duration, SystemTime},
};
use tokio_rustls::rustls::{self, ServerConfig};

const TTL_SECS: i64 = 365 * 24 * 60 * 60;
const CACHE_TTL: u64 = TTL_SECS as u64 / 2;
const NOT_BEFORE_OFFSET: i64 = 60;

#[async_trait]
pub trait CertificateAuthority {
    async fn get_server_config(&self, authority: &Authority) -> Result<Arc<ServerConfig>>;
}

pub(crate) struct OpenSslAuthority {
    openssl_private_key: PKey<Private>,
    private_key_der: PrivateKeyDer<'static>,
    ca_cert: X509,
    hash_algo: MessageDigest,
    server_config_cache: Cache<Authority, Arc<ServerConfig>>,
}

impl OpenSslAuthority {
    pub fn new(private_key_pem_bytes: &[u8], ca_cert_bytes: &[u8]) -> Result<Self> {
        let private_key = PKey::private_key_from_pem(private_key_pem_bytes)?;
        let der_bytes: Vec<u8> = private_key.private_key_to_der()?;
        let private_key_der: PrivateKeyDer<'static> =
            PrivateKeyDer::Pkcs1(PrivatePkcs1KeyDer::from(der_bytes));
        let ca_cert = X509::from_pem(ca_cert_bytes)?;

        Ok(Self {
            openssl_private_key: private_key,
            private_key_der,
            ca_cert,
            hash_algo: MessageDigest::sha256(),
            server_config_cache: Cache::builder()
                .max_capacity(10_000)
                .time_to_live(Duration::from_secs(CACHE_TTL))
                .build(),
        })
    }

    fn generate_certificate(&self, authority: &Authority) -> Result<CertificateDer<'static>> {
        let mut name_builder = X509NameBuilder::new()?;
        name_builder.append_entry_by_text("CN", authority.host())?;
        let name = name_builder.build();

        let mut x509_builder = X509Builder::new()?;
        x509_builder.set_subject_name(&name)?;
        x509_builder.set_version(2)?;

        let not_before = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)?
            .as_secs() as i64
            - NOT_BEFORE_OFFSET;
        x509_builder.set_not_before(Asn1Time::from_unix(not_before)?.as_ref())?;
        x509_builder.set_not_after(Asn1Time::from_unix(not_before + TTL_SECS)?.as_ref())?;

        x509_builder.set_pubkey(&self.openssl_private_key)?;
        x509_builder.set_issuer_name(self.ca_cert.subject_name())?;

        let alternative_name = SubjectAlternativeName::new()
            .dns(authority.host())
            .build(&x509_builder.x509v3_context(Some(&self.ca_cert), None))?;
        x509_builder.append_extension(alternative_name)?;

        let mut serial_number = [0; 16];
        rand::rand_bytes(&mut serial_number)?;

        let serial_number = BigNum::from_slice(&serial_number)?;
        let serial_number = Asn1Integer::from_bn(&serial_number)?;
        x509_builder.set_serial_number(&serial_number)?;

        x509_builder.sign(&self.openssl_private_key, self.hash_algo)?;
        let x509 = x509_builder.build();
        let x509_der_bytes = x509.to_der()?;
        Ok(CertificateDer::from(x509_der_bytes))
    }
}

#[async_trait]
impl CertificateAuthority for OpenSslAuthority {
    async fn get_server_config(&self, authority: &Authority) -> Result<Arc<ServerConfig>> {
        if let Some(server_cfg) = self.server_config_cache.get(authority).await {
            tracing::debug!("Using cached server config");
            return Ok(server_cfg);
        }
        tracing::debug!("Generating server config");

        let cert: CertificateDer<'static> = self.generate_certificate(authority)?;

        let mut server_cfg = ServerConfig::builder()
            .with_no_client_auth()
            .with_single_cert(vec![cert], self.private_key_der.clone_key())?;

        server_cfg.alpn_protocols = vec![b"http/1.1".to_vec()];

        let server_cfg_arc = Arc::new(server_cfg);

        self.server_config_cache
            .insert(authority.clone(), server_cfg_arc.clone())
            .await;

        Ok(server_cfg_arc)
    }
}
