//! カスタム CA による TLS 中継環境を再現する結合テスト（Issue #3 回帰テスト）。
//!
//! テスト実行時にメモリ上で自己署名 CA と `127.0.0.1` 向けサーバー証明書を生成し、
//! ローカル HTTPS サーバーを立てて以下を実測する:
//! 1. CA 未指定のクライアントでは証明書検証エラーで拒否されること。
//! 2. CA PEM バンドルを指定したクライアントでは HTTP 200 が返ること。
//! 3. `inspect_ca_env_with` が PEM 内の証明書件数を正しく報告すること。

use std::sync::Arc;
use std::time::Duration;

use rcgen::{BasicConstraints, CertificateParams, DnType, IsCa, KeyPair};
use rustls_pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio_rustls::TlsAcceptor;

fn build_ca_and_server_cert() -> (String, Vec<CertificateDer<'static>>, PrivateKeyDer<'static>) {
    // 1. テスト用カスタム CA
    let mut ca_params = CertificateParams::default();
    ca_params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
    ca_params
        .distinguished_name
        .push(DnType::CommonName, "tbx-test-egress-ca");
    let ca_key = KeyPair::generate().expect("CA key");
    let ca_cert = ca_params.self_signed(&ca_key).expect("CA cert");
    let ca_pem = ca_cert.pem();

    // 2. 127.0.0.1 向けサーバー証明書（カスタム CA で署名）
    let server_params =
        CertificateParams::new(vec!["127.0.0.1".to_string(), "localhost".to_string()])
            .expect("server params");
    let server_key = KeyPair::generate().expect("server key");
    let server_cert = server_params
        .signed_by(&server_key, &ca_cert, &ca_key)
        .expect("signed server cert");

    let chain = vec![server_cert.der().clone(), ca_cert.der().clone()];
    let key_der = PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(server_key.serialize_der()));
    (ca_pem, chain, key_der)
}

async fn spawn_https_server(
    chain: Vec<CertificateDer<'static>>,
    key: PrivateKeyDer<'static>,
) -> (u16, tokio::task::JoinHandle<()>) {
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let server_config = rustls::ServerConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .expect("tls versions")
        .with_no_client_auth()
        .with_single_cert(chain, key)
        .expect("server config");
    let acceptor = TlsAcceptor::from(Arc::new(server_config));

    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let port = listener.local_addr().unwrap().port();

    let handle = tokio::spawn(async move {
        // 複数接続（失敗試行 + 成功試行）を受け付ける。
        for _ in 0..4 {
            let Ok((stream, _)) = listener.accept().await else {
                break;
            };
            let acceptor = acceptor.clone();
            tokio::spawn(async move {
                let Ok(mut tls) = acceptor.accept(stream).await else {
                    return;
                };
                let mut buf = [0u8; 1024];
                let _ = tls.read(&mut buf).await;
                let resp = "HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok";
                let _ = tls.write_all(resp.as_bytes()).await;
                let _ = tls.shutdown().await;
            });
        }
    });

    (port, handle)
}

#[tokio::test]
async fn custom_ca_bundle_resolves_unknown_issuer() {
    let (ca_pem, chain, key) = build_ca_and_server_cert();
    let (port, _server) = spawn_https_server(chain, key).await;
    let url = format!("https://127.0.0.1:{port}/");

    let ca_file = tempfile::NamedTempFile::new().expect("temp ca file");
    std::fs::write(ca_file.path(), ca_pem.as_bytes()).expect("write ca pem");
    let ca_path = ca_file.path().to_string_lossy().into_owned();

    // 1. CA 検査関数が 1 件の有効証明書として報告すること。
    let report = terabox_core::http::inspect_ca_env_with(Some(&ca_path), None);
    assert!(report.is_ok(), "CA 診断が失敗: {:?}", report.error);
    assert_eq!(report.custom_cert_count, 1);

    // 2. カスタム CA を渡さないクライアントは UnknownIssuer 等で失敗すること。
    let without_ca = terabox_core::http::client_builder_with_ca_file(None)
        .unwrap()
        .no_proxy()
        .timeout(Duration::from_secs(5))
        .build()
        .unwrap();
    let err = without_ca
        .get(&url)
        .send()
        .await
        .expect_err("カスタム CA 未指定なら証明書検証で失敗するはず");
    let err_chain = format!("{err:?}");
    assert!(
        err_chain.contains("UnknownIssuer") || err_chain.contains("certificate") || err.is_connect(),
        "証明書検証の失敗ではないエラー: {err_chain}"
    );

    // 3. カスタム CA バンドルを渡したクライアントは HTTP 200 で通ること。
    let with_ca = terabox_core::http::client_builder_with_ca_file(Some(&ca_path))
        .unwrap()
        .no_proxy()
        .timeout(Duration::from_secs(5))
        .build()
        .unwrap();
    let resp = with_ca
        .get(&url)
        .send()
        .await
        .expect("カスタム CA 指定後は TLS 検証を通過するはず");
    assert_eq!(resp.status().as_u16(), 200);
    assert_eq!(resp.text().await.unwrap(), "ok");

    // 4. 環境変数 SSL_CERT_FILE 経由（本番の client_builder() 経路）でも HTTP 200 で通ること。
    let prev = std::env::var_os("SSL_CERT_FILE");
    std::env::set_var("SSL_CERT_FILE", &ca_path);
    let env_client = terabox_core::http::client_builder()
        .unwrap()
        .no_proxy()
        .timeout(Duration::from_secs(5))
        .build()
        .unwrap();
    let env_res = env_client.get(&url).send().await;
    match prev {
        Some(v) => std::env::set_var("SSL_CERT_FILE", v),
        None => std::env::remove_var("SSL_CERT_FILE"),
    }
    let resp2 = env_res.expect("SSL_CERT_FILE 経由でも TLS 検証を通過するはず");
    assert_eq!(resp2.status().as_u16(), 200);
    assert_eq!(resp2.text().await.unwrap(), "ok");
}
