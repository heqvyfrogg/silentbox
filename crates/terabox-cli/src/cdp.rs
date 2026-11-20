//! Chrome DevTools Protocol の最小クライアント。
//!
//! 目的は 1 つだけ: 実際のブラウザ（起動したブラウザ、または起動済みの
//! ヘッドレス Chromium）から `ndus` Cookie を読むこと。
//!
//! なぜ WebView2 埋め込みではなく外部ブラウザか:
//! - TeraBox のログインは Google / 電話番号 / メールの多段認証を含み、
//!   埋め込み WebView ではポリシー上弾かれる経路がある。
//! - 既にインストール済みの Chrome / Edge / Chromium を使えば追加ランタイムが不要。
//! - Cookie の取得は CDP の `Network.getAllCookies`（ブラウザレベル接続時は
//!   `Storage.getCookies`）で行える。

use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use tokio_tungstenite::tungstenite::Message;

/// CDP のエラー。
#[derive(Debug, thiserror::Error)]
pub enum CdpError {
    #[error("HTTP: {0}")]
    Http(#[from] reqwest::Error),
    #[error("WebSocket: {0}")]
    Ws(#[from] tokio_tungstenite::tungstenite::Error),
    #[error("CDP がエラーを返しました: {0}")]
    Protocol(String),
    #[error("デバッグ用エンドポイントに接続できません（{0}）")]
    NotReachable(String),
    #[error("{0}")]
    Other(String),
}

/// 1 本の WebSocket 接続。
pub struct CdpSession {
    socket: tokio_tungstenite::WebSocketStream<
        tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
    >,
    next_id: u64,
}

impl CdpSession {
    /// ローカルのデバッグポートへ接続する。
    pub async fn connect(port: u16) -> Result<Self, CdpError> {
        Self::connect_url(&format!("http://127.0.0.1:{port}")).await
    }

    /// 指定した CDP エンドポイント（`http://host:port` または `ws://...`）へ接続する。
    ///
    /// - `ws://...`: 直接 WebSocket へ接続する。
    /// - `http://...`: `/json/list` からページターゲット（無ければ先頭）の
    ///   `webSocketDebuggerUrl` を取得し、取れなければ `/json/version` の
    ///   ブラウザターゲットへフォールバックする。
    /// - `https://` / `wss://`: 現行の非 TLS WebSocket 構成では未対応のため明示エラー。
    pub async fn connect_url(cdp_url: &str) -> Result<Self, CdpError> {
        let trimmed = cdp_url.trim().trim_end_matches('/');
        if trimmed.is_empty() {
            return Err(CdpError::Other("CDP URL が空です".into()));
        }

        if trimmed.starts_with("wss://") || trimmed.starts_with("https://") {
            return Err(CdpError::Other(
                "https:// および wss:// の CDP エンドポイントは未対応です。ローカルの http://127.0.0.1:<port> または ws://... を指定してください。"
                    .into(),
            ));
        }

        let ws_url = if trimmed.starts_with("ws://") {
            trimmed.to_string()
        } else if trimmed.starts_with("http://") {
            resolve_ws_url(trimmed).await?
        } else {
            return Err(CdpError::Other(format!(
                "CDP URL は http:// または ws:// で始めてください（受け取った値: {trimmed}）"
            )));
        };

        let (socket, _) = tokio_tungstenite::connect_async(&ws_url).await?;
        Ok(Self { socket, next_id: 1 })
    }

    /// 1 コマンドを送り、対応する応答を待つ。
    pub async fn call(&mut self, method: &str, params: Value) -> Result<Value, CdpError> {
        let id = self.next_id;
        self.next_id += 1;
        let payload = json!({ "id": id, "method": method, "params": params });
        self.socket
            .send(Message::Text(payload.to_string()))
            .await?;

        // 応答が来るまでイベントを読み飛ばす。上限を設けて無限待機を避ける。
        let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
        loop {
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            if remaining.is_zero() {
                return Err(CdpError::Other(format!(
                    "{method} の応答が 15 秒以内に返りませんでした"
                )));
            }
            let msg = match tokio::time::timeout(remaining, self.socket.next()).await {
                Ok(Some(Ok(m))) => m,
                Ok(Some(Err(e))) => return Err(CdpError::Ws(e)),
                Ok(None) => return Err(CdpError::Other("WebSocket が閉じられました".into())),
                Err(_) => {
                    return Err(CdpError::Other(format!(
                        "{method} の応答待ちがタイムアウトしました"
                    )))
                }
            };
            let text = match msg {
                Message::Text(t) => t,
                Message::Binary(b) => String::from_utf8_lossy(&b).into_owned(),
                _ => continue,
            };
            let value: Value = match serde_json::from_str(&text) {
                Ok(v) => v,
                Err(_) => continue,
            };
            if value.get("id").and_then(|v| v.as_u64()) != Some(id) {
                continue;
            }
            if let Some(err) = value.get("error") {
                return Err(CdpError::Protocol(err.to_string()));
            }
            return Ok(value.get("result").cloned().unwrap_or(Value::Null));
        }
    }

    /// すべての Cookie を取得する。
    ///
    /// ページターゲットでは `Network.getAllCookies` が使えるが、ブラウザレベル
    /// ターゲット（`/json/version` 経由）では `Storage.getCookies` が必要になる
    /// 場合がある。プロトコルエラー時は `Storage.getCookies` へフォールバックする。
    pub async fn all_cookies(&mut self) -> Result<Vec<Cookie>, CdpError> {
        let result = match self.call("Network.getAllCookies", json!({})).await {
            Ok(v) => v,
            Err(CdpError::Protocol(_)) => self.call("Storage.getCookies", json!({})).await?,
            Err(e) => return Err(e),
        };
        let raw = result
            .get("cookies")
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default();
        Ok(raw
            .into_iter()
            .filter_map(|v| {
                Some(Cookie {
                    name: v.get("name")?.as_str()?.to_string(),
                    value: v.get("value")?.as_str()?.to_string(),
                    domain: v
                        .get("domain")
                        .and_then(|d| d.as_str())
                        .unwrap_or("")
                        .to_string(),
                })
            })
            .collect())
    }

    /// 指定名の Cookie を探す（`terabox` 系ドメインを優先）。
    pub async fn find_cookie(&mut self, name: &str) -> Result<Option<String>, CdpError> {
        let cookies = self.all_cookies().await?;
        let hit = cookies
            .iter()
            .find(|c| c.name == name && c.domain.contains("terabox"))
            .or_else(|| cookies.iter().find(|c| c.name == name));
        Ok(hit.map(|c| c.value.clone()))
    }
}

/// Cookie 1 件。
#[derive(Debug, Clone)]
pub struct Cookie {
    pub name: String,
    pub value: String,
    pub domain: String,
}

/// URL がループバック宛てか判定する。
///
/// VM 内で `HTTP_PROXY` / `HTTPS_PROXY` が設定されていると、ローカルの
/// `127.0.0.1` 向け CDP リクエストまでプロキシ経由になりタイムアウトする。
/// ループバック宛ては明示的にプロキシを外す。
pub fn is_loopback_url(url: &str) -> bool {
    let Ok(parsed) = reqwest::Url::parse(url) else {
        return false;
    };
    matches!(
        parsed.host_str(),
        Some("127.0.0.1") | Some("localhost") | Some("::1") | Some("[::1]")
    )
}

fn cdp_http_client(base_url: &str, timeout: Duration) -> Result<reqwest::Client, CdpError> {
    let mut builder = reqwest::Client::builder().timeout(timeout);
    if is_loopback_url(base_url) {
        builder = builder.no_proxy();
    }
    Ok(builder.build()?)
}

async fn resolve_ws_url(base_url: &str) -> Result<String, CdpError> {
    let client = cdp_http_client(base_url, Duration::from_secs(5))?;
    let list_url = format!("{base_url}/json/list");

    let list_res = client.get(&list_url).send().await;
    if let Ok(resp) = list_res {
        if resp.status().is_success() {
            if let Ok(targets) = resp.json::<Vec<Value>>().await {
                let page = targets
                    .iter()
                    .find(|t| t.get("type").and_then(|v| v.as_str()) == Some("page"))
                    .or_else(|| targets.first());
                if let Some(ws) = page
                    .and_then(|t| t.get("webSocketDebuggerUrl"))
                    .and_then(|v| v.as_str())
                {
                    return Ok(ws.to_string());
                }
            }
        }
    }

    // `/json/list` が空、またはページターゲットに WS URL が無い場合は
    // `/json/version` のブラウザレベル WS URL を使う。
    let ver_url = format!("{base_url}/json/version");
    let resp = client
        .get(&ver_url)
        .send()
        .await
        .map_err(|e| CdpError::NotReachable(format!("{ver_url} へ接続できません: {e}")))?;
    if !resp.status().is_success() {
        return Err(CdpError::NotReachable(format!(
            "{ver_url} が HTTP {} を返しました",
            resp.status().as_u16()
        )));
    }
    let body: Value = resp.json().await?;
    body.get("webSocketDebuggerUrl")
        .and_then(|v| v.as_str())
        .map(ToString::to_string)
        .ok_or_else(|| CdpError::Other("webSocketDebuggerUrl がありません".into()))
}

/// CDP エンドポイントの疎通とブラウザ情報を診断する（`tbx doctor` 用）。
pub async fn probe_version(cdp_url: &str, timeout: Duration) -> Result<String, CdpError> {
    let trimmed = cdp_url.trim().trim_end_matches('/');
    if trimmed.starts_with("ws://") {
        let fut = tokio_tungstenite::connect_async(trimmed);
        match tokio::time::timeout(timeout, fut).await {
            Ok(Ok(_)) => return Ok(format!("{trimmed}（WebSocket 到達）")),
            Ok(Err(e)) => return Err(CdpError::Ws(e)),
            Err(_) => {
                return Err(CdpError::NotReachable(format!(
                    "{trimmed} がタイムアウトしました"
                )))
            }
        }
    }
    if !trimmed.starts_with("http://") {
        return Err(CdpError::Other(format!(
            "診断できる CDP URL は http:// または ws:// のみです: {trimmed}"
        )));
    }
    let client = cdp_http_client(trimmed, timeout)?;
    let url = format!("{trimmed}/json/version");
    let resp = client
        .get(&url)
        .send()
        .await
        .map_err(|e| CdpError::NotReachable(format!("{url}: {e}")))?;
    if !resp.status().is_success() {
        return Err(CdpError::NotReachable(format!(
            "{url} が HTTP {} を返しました",
            resp.status().as_u16()
        )));
    }
    let body: Value = resp.json().await?;
    let browser = body
        .get("Browser")
        .and_then(|v| v.as_str())
        .unwrap_or("Chromium (バージョン不明)");
    Ok(format!("{browser} ({trimmed})"))
}

/// デバッグポートが開くまで待つ。
///
/// ブラウザの起動には数百 ms〜数秒かかるため、上限付きでポーリングする。
pub async fn wait_for_port(port: u16, timeout: Duration) -> bool {
    let deadline = tokio::time::Instant::now() + timeout;
    let base = format!("http://127.0.0.1:{port}");
    let url = format!("{base}/json/version");
    let client = match cdp_http_client(&base, Duration::from_secs(2)) {
        Ok(c) => c,
        Err(_) => return false,
    };
    while tokio::time::Instant::now() < deadline {
        if let Ok(resp) = client.get(&url).send().await {
            if resp.status().is_success() {
                return true;
            }
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    #[test]
    fn loopback_detection() {
        assert!(is_loopback_url("http://127.0.0.1:9222"));
        assert!(is_loopback_url("http://localhost:9222/json/list"));
        assert!(is_loopback_url("http://[::1]:9222"));
        assert!(!is_loopback_url("http://192.168.1.10:9222"));
        assert!(!is_loopback_url("https://www.terabox.com"));
    }

    #[tokio::test]
    async fn rejects_https_and_wss_cdp_urls() {
        assert!(CdpSession::connect_url("https://127.0.0.1:9222").await.is_err());
        assert!(CdpSession::connect_url("wss://127.0.0.1:9222/devtools/page/1")
            .await
            .is_err());
        assert!(CdpSession::connect_url("127.0.0.1:9222").await.is_err());
    }

    /// モックの CDP WS サーバー（`fallback_to_storage` が true のときは
    /// `Network.getAllCookies` にエラーを返し、`Storage.getCookies` で Cookie を返す）。
    async fn spawn_mock_ws(fallback_to_storage: bool) -> (String, tokio::task::JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let handle = tokio::spawn(async move {
            if let Ok((stream, _)) = listener.accept().await {
                if let Ok(mut ws) = tokio_tungstenite::accept_async(stream).await {
                    while let Some(Ok(Message::Text(txt))) = ws.next().await {
                        let req: Value = serde_json::from_str(&txt).unwrap();
                        let id = req["id"].as_u64().unwrap();
                        let method = req["method"].as_str().unwrap();
                        let resp = if method == "Network.getAllCookies" && fallback_to_storage {
                            json!({
                                "id": id,
                                "error": { "code": -32601, "message": "'Network.getAllCookies' wasn't found" }
                            })
                        } else {
                            json!({
                                "id": id,
                                "result": {
                                    "cookies": [
                                        { "name": "ndus", "value": "other-domain", "domain": ".example.com" },
                                        { "name": "ndus", "value": "tb-cookie-val", "domain": ".terabox.com" }
                                    ]
                                }
                            })
                        };
                        let _ = ws.send(Message::Text(resp.to_string())).await;
                    }
                }
            }
        });
        (format!("ws://{addr}/devtools/browser/test"), handle)
    }

    /// `/json/list` と `/json/version` を返す最小 HTTP サーバー。
    async fn spawn_mock_http(
        list_body: String,
        version_body: String,
    ) -> (String, tokio::task::JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let handle = tokio::spawn(async move {
            for _ in 0..4 {
                let Ok((mut stream, _)) = listener.accept().await else {
                    break;
                };
                let mut buf = [0u8; 2048];
                let n = stream.read(&mut buf).await.unwrap_or(0);
                let req = String::from_utf8_lossy(&buf[..n]);
                let body = if req.contains("GET /json/list ") {
                    list_body.clone()
                } else {
                    version_body.clone()
                };
                let resp = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    body.len(),
                    body
                );
                let _ = stream.write_all(resp.as_bytes()).await;
            }
        });
        (format!("http://{addr}"), handle)
    }

    #[tokio::test]
    async fn connect_url_via_http_json_list_finds_terabox_cookie() {
        let (ws_url, _ws) = spawn_mock_ws(false).await;
        let list = json!([
            { "type": "page", "webSocketDebuggerUrl": ws_url }
        ])
        .to_string();
        let ver = json!({ "Browser": "HeadlessChrome/131.0", "webSocketDebuggerUrl": ws_url }).to_string();
        let (http_url, _http) = spawn_mock_http(list, ver).await;

        let mut session = CdpSession::connect_url(&http_url).await.unwrap();
        let ndus = session.find_cookie("ndus").await.unwrap();
        assert_eq!(ndus.as_deref(), Some("tb-cookie-val"));
    }

    #[tokio::test]
    async fn connect_url_falls_back_to_json_version_and_storage_cookies() {
        let (ws_url, _ws) = spawn_mock_ws(true).await;
        let list = "[]".to_string();
        let ver = json!({ "Browser": "HeadlessChrome/131.0", "webSocketDebuggerUrl": ws_url }).to_string();
        let (http_url, _http) = spawn_mock_http(list, ver).await;

        let info = probe_version(&http_url, Duration::from_secs(2)).await.unwrap();
        assert!(info.contains("HeadlessChrome/131.0"), "{info}");

        let mut session = CdpSession::connect_url(&http_url).await.unwrap();
        let ndus = session.find_cookie("ndus").await.unwrap();
        assert_eq!(ndus.as_deref(), Some("tb-cookie-val"));
    }

    #[tokio::test]
    async fn connect_url_direct_ws() {
        let (ws_url, _ws) = spawn_mock_ws(false).await;
        let mut session = CdpSession::connect_url(&ws_url).await.unwrap();
        let ndus = session.find_cookie("ndus").await.unwrap();
        assert_eq!(ndus.as_deref(), Some("tb-cookie-val"));
    }
}
