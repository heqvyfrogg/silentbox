//! HTTP クライアント構築と TLS / CA・プロキシ環境の診断。
//!
//! `reqwest` は `rustls-tls-native-roots` と `rustls-tls-webpki-roots` の両方を
//! 有効にしている。これにより通常の公開 CA に加え、OS のネイティブ CA ストア
//! および `SSL_CERT_FILE` / `SSL_CERT_DIR`（TLS 中継プロキシや企業 CA）を
//! 信頼できる。
//!
//! `rustls-native-certs` は Linux では `SSL_CERT_FILE` を参照するが、Windows /
//! macOS では OS ストアのみを見る。全 OS で一貫して `SSL_CERT_FILE` を効かせる
//! ため、ここでは `SSL_CERT_FILE` の PEM バンドルも明示的に追加登録する。

use std::path::Path;

use reqwest::{Certificate, ClientBuilder};

use crate::error::TbxError;

/// `SSL_CERT_FILE` / `SSL_CERT_DIR` の検査結果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CaEnvReport {
    /// `SSL_CERT_FILE` の値（未設定または空なら `None`）。
    pub ssl_cert_file: Option<String>,
    /// `SSL_CERT_DIR` の値（未設定または空なら `None`）。
    pub ssl_cert_dir: Option<String>,
    /// `SSL_CERT_FILE` から読み込めた PEM 証明書の件数。
    pub custom_cert_count: usize,
    /// 設定されているのに読めない・壊れている等の問題があれば内容が入る。
    pub error: Option<String>,
}

impl CaEnvReport {
    /// 設定に異常がないか（未設定の場合も正常とみなす）。
    pub fn is_ok(&self) -> bool {
        self.error.is_none()
    }
}

/// 共通の `reqwest::ClientBuilder` を返す。
///
/// OS ネイティブ CA + webpki 埋め込み CA に加え、`SSL_CERT_FILE` で指定された
/// PEM バンドルがあればルート証明書へ追加する。
/// `SSL_CERT_FILE` の読取に失敗した場合は警告ログを出し、残りの CA で続行する。
pub fn client_builder() -> Result<ClientBuilder, TbxError> {
    let cert_file = non_empty_env("SSL_CERT_FILE");
    client_builder_with_ca_file(cert_file.as_deref())
}

/// 指定パスの PEM バンドルを追加した `ClientBuilder` を返す。
pub fn client_builder_with_ca_file(ca_file: Option<&str>) -> Result<ClientBuilder, TbxError> {
    let mut builder = reqwest::Client::builder();
    if let Some(path) = ca_file {
        match load_pem_certificates(Path::new(path)) {
            Ok(certs) => {
                for cert in certs {
                    builder = builder.add_root_certificate(cert);
                }
            }
            Err(reason) => {
                tracing::warn!(path = %path, "SSL_CERT_FILE の読み込みをスキップしました: {reason}");
            }
        }
    }
    Ok(builder)
}

/// PEM バンドルファイルを読み込み、`Certificate` の一覧を返す。
pub fn load_pem_certificates(path: &Path) -> Result<Vec<Certificate>, String> {
    let bytes = std::fs::read(path)
        .map_err(|e| format!("{} を読めませんでした: {e}", path.display()))?;
    let certs = Certificate::from_pem_bundle(&bytes)
        .map_err(|e| format!("{} の PEM 証明書を解釈できませんでした: {e}", path.display()))?;
    if certs.is_empty() {
        return Err(format!(
            "{} に有効な PEM 証明書が含まれていません",
            path.display()
        ));
    }
    Ok(certs)
}

/// 現在の環境変数から CA 設定を検査する。
pub fn inspect_ca_env() -> CaEnvReport {
    let cert_file = non_empty_env("SSL_CERT_FILE");
    let cert_dir = non_empty_env("SSL_CERT_DIR");
    inspect_ca_env_with(cert_file.as_deref(), cert_dir.as_deref())
}

/// パスを直接渡して CA 設定を検査する（テスト用）。
pub fn inspect_ca_env_with(cert_file: Option<&str>, cert_dir: Option<&str>) -> CaEnvReport {
    let mut custom_cert_count = 0usize;
    let mut errors: Vec<String> = Vec::new();

    if let Some(file) = cert_file {
        match load_pem_certificates(Path::new(file)) {
            Ok(certs) => custom_cert_count = certs.len(),
            Err(e) => errors.push(format!("SSL_CERT_FILE: {e}")),
        }
    }

    if let Some(dir) = cert_dir {
        let p = Path::new(dir);
        if !p.is_dir() {
            errors.push(format!("SSL_CERT_DIR: {} がディレクトリとして存在しません", p.display()));
        }
    }

    CaEnvReport {
        ssl_cert_file: cert_file.map(ToString::to_string),
        ssl_cert_dir: cert_dir.map(ToString::to_string),
        custom_cert_count,
        error: if errors.is_empty() {
            None
        } else {
            Some(errors.join(" / "))
        },
    }
}

/// プロキシ関連の環境変数を収集する（URL 内の認証情報はマスクする）。
///
/// Windows は環境変数名の大文字・小文字を区別しないため、同一キーの重複を防ぐ。
/// Unix でも大文字版と小文字版が同じ値なら 1 回だけ出す。
pub fn inspect_proxy_env() -> Vec<(String, String)> {
    const GROUPS: &[(&str, &str, bool)] = &[
        ("HTTPS_PROXY", "https_proxy", true),
        ("HTTP_PROXY", "http_proxy", true),
        ("ALL_PROXY", "all_proxy", true),
        ("NO_PROXY", "no_proxy", false),
    ];
    let mut out = Vec::new();
    for (upper, lower, is_url) in GROUPS {
        let u = non_empty_env(upper);
        let l = if cfg!(windows) {
            None
        } else {
            non_empty_env(lower)
        };
        if let Some(ref val) = u {
            let display = if *is_url {
                mask_proxy_url(val)
            } else {
                val.clone()
            };
            out.push(((*upper).to_string(), display));
        }
        if let Some(ref val) = l {
            if u.as_deref() != Some(val.as_str()) {
                let display = if *is_url {
                    mask_proxy_url(val)
                } else {
                    val.clone()
                };
                out.push(((*lower).to_string(), display));
            }
        }
    }
    out
}

/// プロキシ URL 内の `user:pass@` 認証情報を `***@` に伏せる。
///
/// ログや診断出力に資格情報を残さないための単一境界。パスワード中に `@` や
/// `:` が含まれていても、authority 内の最後の `@` より前をすべて伏せる。
pub fn mask_proxy_url(raw: &str) -> String {
    let trimmed = raw.trim();
    let (prefix, rest) = match trimmed.find("://") {
        Some(idx) => (&trimmed[..idx + 3], &trimmed[idx + 3..]),
        None => ("", trimmed),
    };

    // authority の終端（`/`, `?`, `#` の最初）。
    let auth_end = rest
        .find(['/', '?', '#'])
        .unwrap_or(rest.len());
    let authority = &rest[..auth_end];
    let suffix = &rest[auth_end..];

    match authority.rfind('@') {
        Some(at) => format!("{prefix}***@{}{suffix}", &authority[at + 1..]),
        None => trimmed.to_string(),
    }
}

fn non_empty_env(name: &str) -> Option<String> {
    std::env::var(name)
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mask_proxy_url_hides_userinfo() {
        assert_eq!(
            mask_proxy_url("http://alice:s3cr3t@proxy.example:8080"),
            "http://***@proxy.example:8080"
        );
        assert_eq!(
            mask_proxy_url("https://user:p@ss:word@10.0.0.1:3128/path"),
            "https://***@10.0.0.1:3128/path"
        );
        assert_eq!(
            mask_proxy_url("socks5://proxy.local:1080"),
            "socks5://proxy.local:1080"
        );
        assert_eq!(
            mask_proxy_url("alice:secret@proxy.local:8080"),
            "***@proxy.local:8080"
        );
        let masked = mask_proxy_url("http://bob:topsecret@127.0.0.1:8888");
        assert!(!masked.contains("bob"));
        assert!(!masked.contains("topsecret"));
    }

    #[test]
    fn inspect_ca_env_reports_missing_file() {
        let report = inspect_ca_env_with(Some("/definitely/missing/ca-bundle.pem"), None);
        assert!(!report.is_ok());
        assert_eq!(report.custom_cert_count, 0);
        assert!(report.error.as_deref().unwrap_or("").contains("SSL_CERT_FILE"));
    }

    #[test]
    fn inspect_ca_env_ok_when_unset() {
        let report = inspect_ca_env_with(None, None);
        assert!(report.is_ok());
        assert_eq!(report.custom_cert_count, 0);
    }

    #[test]
    fn client_builder_builds_with_defaults() {
        let client = client_builder_with_ca_file(None).unwrap().build();
        assert!(client.is_ok());
    }
}
