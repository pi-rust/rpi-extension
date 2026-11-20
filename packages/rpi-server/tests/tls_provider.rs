//! Keep this regression in its own test executable: the process-wide provider
//! cannot be reset, and other tests must not mask accidental global installation.
use rpi_server::tls::load_server_config;
use rpi_server::transport::{Transport, WebSocketTransport};
use rustls::crypto::{aws_lc_rs, CryptoProvider};
use std::path::PathBuf;
use std::sync::Arc;

struct TempDir(PathBuf);

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[tokio::test]
async fn tls_configs_do_not_install_or_override_the_global_provider() {
    assert!(CryptoProvider::get_default().is_none());
    let dir = TempDir(
        std::env::temp_dir().join(format!("rpi-server-local-provider-{}", std::process::id())),
    );
    std::fs::create_dir_all(&dir.0).expect("temp directory");
    let pair = rcgen::generate_simple_self_signed(vec!["localhost".into()]).expect("certificate");
    let cert = dir.0.join("cert.pem");
    let key = dir.0.join("key.pem");
    std::fs::write(&cert, pair.cert.pem()).expect("write certificate");
    std::fs::write(&key, pair.key_pair.serialize_pem()).expect("write key");

    for client_ca in [None, Some(cert.as_path())] {
        load_server_config(&cert, &key, client_ca)
            .expect("local provider without a global default");
        assert!(
            CryptoProvider::get_default().is_none(),
            "config construction must not install a process-wide provider"
        );
    }

    // Exercise the real outbound TLS path too. An untrusted self-signed
    // certificate must still fail verification, not panic or install a default.
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("listener");
    let port = listener.local_addr().expect("address").port();
    let acceptor = tokio_rustls::TlsAcceptor::from(
        load_server_config(&cert, &key, None).expect("server configuration"),
    );
    let server = async {
        let (stream, _) = listener.accept().await.expect("accept");
        assert!(acceptor.accept(stream).await.is_err());
    };
    let client = async {
        let error = WebSocketTransport::secure()
            .connect(&format!("wss://localhost:{port}/"))
            .await
            .err()
            .expect("self-signed certificate must be rejected");
        assert!(error.contains("tls handshake failed"), "{error}");
        assert!(error.contains("UnknownIssuer"), "{error}");
    };
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        tokio::join!(server, client);
    })
    .await
    .expect("bounded TLS handshake");
    assert!(CryptoProvider::get_default().is_none());

    // A caller's deliberately restricted default must neither be overwritten
    // nor silently adopted by this library's local TLS configuration.
    let mut caller_provider = aws_lc_rs::default_provider();
    caller_provider.cipher_suites.truncate(1);
    caller_provider.install_default().expect("caller default");
    let caller_default = CryptoProvider::get_default().expect("installed default");
    for client_ca in [None, Some(cert.as_path())] {
        let config = load_server_config(&cert, &key, client_ca).expect("local TLS configuration");
        assert!(!Arc::ptr_eq(config.crypto_provider(), caller_default));
        assert_eq!(
            config.crypto_provider().cipher_suites.len(),
            aws_lc_rs::default_provider().cipher_suites.len()
        );
        assert!(Arc::ptr_eq(
            CryptoProvider::get_default().expect("preserved default"),
            caller_default
        ));
    }
}
