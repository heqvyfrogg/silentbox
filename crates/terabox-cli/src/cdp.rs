//! Chrome DevTools Protocol の最小クライアント。
//!
//! 目的は 1 つだけ: 実際のブラウザでログインしてもらい、その `ndus` Cookie を
//! 読むこと。専用のユーザーデータディレクトリを使うので、利用者の通常の
//! ブラウザプロファイルには触れない。
//!
//! なぜ WebView2 埋め込みではなく外部ブラウザか:
//! - TeraBox のログインは Google / 電話番号 / メールの多段認証を含み、
//!   埋め込み WebView ではポリシー上弾かれる経路がある。
//! - 既にインストール済みの Chrome / Edge を使えば追加ランタイムが不要。
//! - Cookie の取得は CDP の `Network.getAllCookies` で行える。

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
    /// ブラウザのデバッグポートへ接続する。
    ///
    /// `/json/list` からページターゲットを 1 つ選び、その WebSocket へ繋ぐ。
    pub async fn connect(port: u16) -> Result<Self, CdpError> {
        let url = format!("http://127.0.0.1:{port}/json/list");
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(5))
            .build()?;
        let targets: Vec<Value> = client
            .get(&url)
            .send()
            .await
            .map_err(|e| CdpError::NotReachable(format!("{url} へ接続できません: {e}")))?
            .json()
            .await?;

        // `type == "page"` を優先する。無ければ先頭。
        let page = targets
            .iter()
            .find(|t| t.get("type").and_then(|v| v.as_str()) == Some("page"))
            .or_else(|| targets.first())
            .ok_or_else(|| CdpError::NotReachable("ブラウザにページがありません".into()))?;

        let ws_url = page
            .get("webSocketDebuggerUrl")
            .and_then(|v| v.as_str())
            .ok_or_else(|| CdpError::Other("webSocketDebuggerUrl がありません".into()))?;

        let (socket, _) = tokio_tungstenite::connect_async(ws_url).await?;
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
    pub async fn all_cookies(&mut self) -> Result<Vec<Cookie>, CdpError> {
        let result = self.call("Network.getAllCookies", json!({})).await?;
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

/// デバッグポートが開くまで待つ。
///
/// ブラウザの起動には数百 ms〜数秒かかるため、上限付きでポーリングする。
pub async fn wait_for_port(port: u16, timeout: Duration) -> bool {
    let deadline = tokio::time::Instant::now() + timeout;
    let url = format!("http://127.0.0.1:{port}/json/version");
    let client = match reqwest::Client::builder()
        .timeout(Duration::from_secs(2))
        .build()
    {
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
