//! 既に開いているブラウザ（`--remote-debugging-port=9333`）から Cookie を読み、
//! セッションを取り直して資格情報マネージャへ保存する。
//!
//! `tbx login` は自分でブラウザを起動して自動で閉じるため、本人確認のように
//! 時間のかかる操作には向かない。こちらは開いたままのブラウザを使う。

use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use terabox_core::bootstrap::Bootstrap;
use terabox_core::session::Session;

const SERVICE: &str = "tbx.terabox";
const ACCOUNT: &str = "session";
const PORT: u16 = 9333;

async fn cdp_call(
    socket: &mut tokio_tungstenite::WebSocketStream<
        tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
    >,
    next_id: &mut u64,
    method: &str,
    params: Value,
) -> Result<Value, String> {
    let id = *next_id;
    *next_id += 1;
    let payload = json!({ "id": id, "method": method, "params": params });
    socket
        .send(tokio_tungstenite::tungstenite::Message::Text(payload.to_string()))
        .await
        .map_err(|e| e.to_string())?;

    let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
    loop {
        let remain = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remain.is_zero() {
            return Err(format!("{method} の応答待ちがタイムアウトしました"));
        }
        let msg = match tokio::time::timeout(remain, socket.next()).await {
            Ok(Some(Ok(m))) => m,
            Ok(Some(Err(e))) => return Err(e.to_string()),
            Ok(None) => return Err("WebSocket が閉じられました".into()),
            Err(_) => return Err(format!("{method} がタイムアウトしました")),
        };
        let text = match msg {
            tokio_tungstenite::tungstenite::Message::Text(t) => t,
            tokio_tungstenite::tungstenite::Message::Binary(b) => {
                String::from_utf8_lossy(&b).into_owned()
            }
            _ => continue,
        };
        let v: Value = match serde_json::from_str(&text) {
            Ok(v) => v,
            Err(_) => continue,
        };
        if v.get("id").and_then(|x| x.as_u64()) != Some(id) {
            continue;
        }
        if let Some(err) = v.get("error") {
            return Err(err.to_string());
        }
        return Ok(v.get("result").cloned().unwrap_or(Value::Null));
    }
}

#[tokio::main]
async fn main() {
    println!("=== CDP に接続 (127.0.0.1:{PORT}) ===");
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(10))
        .build()
        .expect("client");
    let targets: Vec<Value> = client
        .get(format!("http://127.0.0.1:{PORT}/json/list"))
        .send()
        .await
        .expect("json/list に接続できません。ブラウザが起動しているか確認してください")
        .json()
        .await
        .expect("json");

    let page = targets
        .iter()
        .find(|t| t.get("type").and_then(|v| v.as_str()) == Some("page"))
        .expect("ページがありません");
    let ws_url = page
        .get("webSocketDebuggerUrl")
        .and_then(|v| v.as_str())
        .expect("webSocketDebuggerUrl がありません")
        .to_string();
    println!("対象ページ: {}", page.get("url").and_then(|v| v.as_str()).unwrap_or("?"));

    let (mut socket, _) = tokio_tungstenite::connect_async(&ws_url)
        .await
        .expect("WebSocket 接続に失敗しました");
    let mut next_id = 1u64;

    let cookies = cdp_call(&mut socket, &mut next_id, "Network.getAllCookies", json!({}))
        .await
        .expect("Cookie を読めません");
    let ndus = cookies
        .get("cookies")
        .and_then(|v| v.as_array())
        .and_then(|arr| {
            arr.iter()
                .find(|c| {
                    c.get("name").and_then(|n| n.as_str()) == Some("ndus")
                        && c.get("domain")
                            .and_then(|d| d.as_str())
                            .map(|d| d.contains("terabox"))
                            .unwrap_or(false)
                })
                .or_else(|| {
                    arr.iter().find(|c| c.get("name").and_then(|n| n.as_str()) == Some("ndus"))
                })
        })
        .and_then(|c| c.get("value").and_then(|v| v.as_str()))
        .map(|s| s.to_string());

    let Some(ndus) = ndus else {
        println!("ndus が見つかりません。ログインが完了していない可能性があります。");
        return;
    };
    println!("ndus を取得しました（長さ {} 文字）", ndus.len());

    // 同じ Cookie で /main を読み直して jsToken / bdstoken を取り直す。
    let resp = client
        .get("https://www.terabox.com/main")
        .header(reqwest::header::USER_AGENT, terabox_core::DEFAULT_UA)
        .header(
            reqwest::header::COOKIE,
            format!("ndus={ndus}; PANWEB=1"),
        )
        .send()
        .await
        .expect("/main の取得に失敗しました");
    let status = resp.status();
    let body = resp.text().await.unwrap_or_default();
    println!("/main: HTTP {status} / {} バイト", body.len());

    let bootstrap = match Bootstrap::parse(&body) {
        Ok(b) => b,
        Err(e) => {
            println!("トークンを取り出せませんでした: {e}");
            return;
        }
    };
    println!(
        "jsToken 長 = {:?} / bdstoken = {:?} / uk = {:?} / logged_in = {}",
        bootstrap.js_token.as_deref().map(|s| s.len()),
        bootstrap.bdstoken.as_deref().map(|s| s.len()),
        bootstrap.uk.as_deref(),
        bootstrap.logged_in
    );

    if !bootstrap.logged_in {
        println!("ログイン済みと確認できませんでした。ブラウザでログインを完了してください。");
        return;
    }

    let session = Session::from_bootstrap(&ndus, &bootstrap, terabox_core::DEFAULT_UA)
        .expect("セッションを組み立てられませんでした");
    println!("アカウント: {}", session.account_label());

    let entry = keyring::Entry::new(SERVICE, ACCOUNT).expect("keyring");
    match serde_json::to_string(&session) {
        Ok(text) => match entry.set_password(&text) {
            Ok(()) => println!("資格情報マネージャへ保存しました"),
            Err(e) => println!("保存に失敗しました: {e}"),
        },
        Err(e) => println!("直列化に失敗しました: {e}"),
    }
}
