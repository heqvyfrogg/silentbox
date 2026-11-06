use std::time::Instant;

use tokio::task::JoinSet;
use terabox_core::api::ApiClient;
use terabox_core::bootstrap::Bootstrap;
use terabox_core::session::Session;

fn load_session() -> Session {
    let entry = keyring::Entry::new("tbx.terabox", "session").expect("keyring");
    let text = entry.get_password().expect("session がありません");
    serde_json::from_str(&text).expect("session を読めません")
}

/// 1 接続で範囲を取り切る。`conn` 本を同時に走らせて合計スループットを測る。
async fn measure(
    client: &reqwest::Client,
    dlink: &str,
    cookie: &str,
    ua: &str,
    size: u64,
    conn: usize,
) -> (u64, f64) {
    let start = Instant::now();
    let per = size / conn as u64;

    let mut tasks = JoinSet::new();
    for i in 0..conn {
        let from = per * i as u64;
        let to = if i + 1 == conn { size - 1 } else { per * (i + 1) as u64 - 1 };
        let client = client.clone();
        let dlink = dlink.to_string();
        let cookie = cookie.to_string();
        let ua = ua.to_string();
        tasks.spawn(async move {
            let resp = client
                .get(&dlink)
                .header(reqwest::header::USER_AGENT, ua)
                .header(reqwest::header::COOKIE, cookie)
                .header(reqwest::header::RANGE, format!("bytes={from}-{to}"))
                .send()
                .await;
            match resp {
                Ok(r) => {
                    let status = r.status();
                    match r.bytes().await {
                        Ok(b) => (status.as_u16(), b.len()),
                        Err(_) => (status.as_u16(), 0),
                    }
                }
                Err(_) => (0, 0),
            }
        });
    }

    let mut total = 0u64;
    let mut statuses = Vec::new();
    while let Some(j) = tasks.join_next().await {
        if let Ok((status, len)) = j {
            total += len as u64;
            statuses.push(status);
        }
    }
    let secs = start.elapsed().as_secs_f64();
    let mbps = (total as f64 / 1_048_576.0) / secs;
    (total, mbps)
}

#[tokio::main]
async fn main() {
    let session = load_session();
    let api = ApiClient::new(session.clone()).expect("api");

    // 大きめのファイルを探す（無ければ最大のものを使う）。
    let resp = api.list("/", 1, 1000, "size", true).await.expect("list");
    let target = resp
        .list
        .iter()
        .filter(|e| !e.isdir)
        .max_by_key(|e| e.size)
        .expect("ファイルがありません");
    println!(
        "対象: {}  {} bytes ({:.2} MiB)",
        target.path,
        target.size,
        target.size as f64 / 1_048_576.0
    );

    let dlink = api.dlink(&target.path).await.expect("dlink");
    println!("dlink 取得済み（長さ {} 文字）", dlink.len());

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(120))
        .build()
        .expect("client");
    let cookie = format!("ndus={}; PANWEB=1", session.ndus);

    let mut results = Vec::new();
    for conn in [1usize, 2, 4, 8] {
        // 毎回 1 回分を捨てて温める（初回の TLS ハンドシェイクを除外）。
        let _ = measure(&client, &dlink, &cookie, &session.user_agent, target.size, 1).await;
        tokio::time::sleep(std::time::Duration::from_millis(600)).await;

        let (bytes, mbps) = measure(
            &client,
            &dlink,
            &cookie,
            &session.user_agent,
            target.size,
            conn,
        )
        .await;
        println!(
            "conn={conn:>2}  {bytes} bytes  {mbps:.2} MiB/s  ({}% 取得)",
            bytes * 100 / target.size.max(1)
        );
        results.push((conn, mbps));
        tokio::time::sleep(std::time::Duration::from_millis(800)).await;
    }

    println!("\n--- まとめ ---");
    let base = results
        .iter()
        .find(|(c, _)| *c == 1)
        .map(|(_, m)| *m)
        .unwrap_or(0.0);
    for (conn, mbps) in results {
        let ratio = if base > 0.0 { mbps / base } else { 0.0 };
        println!("conn={conn:>2}  {mbps:.2} MiB/s  対 1 接続比 {ratio:.2}x");
    }

    // 追加: PCS ホスト経由の単発 GET も測る（www との差を見る）。
    match api.upload_host().await {
        Ok(host) => {
            let pcs_dlink = dlink.replace(
                dlink.split("://").nth(1).and_then(|s| s.split('/').next()).unwrap_or(""),
                &host,
            );
            if pcs_dlink != dlink {
                let (bytes, mbps) = measure(&client, &pcs_dlink, &cookie, &session.user_agent, target.size, 1).await;
                println!("\nPCS ホスト({host}) 経由 conn=1: {bytes} bytes  {mbps:.2} MiB/s");
            } else {
                println!("\nPCS ホスト({host}) への置換は不要（同じホスト）");
            }
        }
        Err(e) => println!("\nPCS ホスト取得失敗: {e}"),
    }
    let _ = Bootstrap::parse("<html></html>");
}
