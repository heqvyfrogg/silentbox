//! 転送エンジンの統合試験（ローカルに立てた範囲対応スタブ HTTP サーバを使う）。
//!
//! 実ネットワークや実アカウントは使わない。ここで確かめるのは:
//! - 分割ダウンロードが全バイトを過不足なく書けること
//! - 途中で切れたあと、再実行で続きから再開できること（レジューム）
//! - サーバが範囲を無視した場合でも要求範囲に切り詰めること（`read_range`）
//!
//! スタブは「テスト用の合成物」であり、TeraBox の実応答を模したものではない。

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use terabox_core::session::Session;
use terabox_core::store::Ledger;
use terabox_core::transfer::{DownloadRequest, Downloader, Progress};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

/// `Range` 付き GET に応答する最小サーバ。
///
/// - 1 回目の接続では前半だけ送って切断する（中断の再現）。
/// - 以降は要求された範囲を正しく返す。
async fn spawn_stub(payload: Vec<u8>, cut_first: bool) -> (String, Arc<AtomicUsize>) {
    let listener = TcpListener::bind(("127.0.0.1", 0)).await.expect("bind");
    let addr = listener.local_addr().expect("addr");
    let hits = Arc::new(AtomicUsize::new(0));
    let hits_server = Arc::clone(&hits);
    let payload = Arc::new(payload);
    let total = payload.len();

    tokio::spawn(async move {
        loop {
            let (mut sock, _) = match listener.accept().await {
                Ok(v) => v,
                Err(_) => break,
            };
            let payload = Arc::clone(&payload);
            let hits = Arc::clone(&hits_server);
            tokio::spawn(async move {
                let mut buf = vec![0u8; 4096];
                let n = match sock.read(&mut buf).await {
                    Ok(n) if n > 0 => n,
                    _ => return,
                };
                let req = String::from_utf8_lossy(&buf[..n]).to_string();
                let idx = hits.fetch_add(1, Ordering::SeqCst);

                let (start, end) = match parse_range(&req, total) {
                    Some(v) => v,
                    None => (0, total.saturating_sub(1)),
                };

                let want = if start > end { 0 } else { end - start + 1 };
                // 最初の接続は半分で切る（中断の再現）。
                let cut = cut_first && idx == 0 && want > 4;
                let send_len = if cut { want / 2 } else { want };

                let status = if req.contains("Range:") || req.contains("range:") {
                    "206 Partial Content"
                } else {
                    "200 OK"
                };

                let header = format!(
                    "HTTP/1.1 {status}\r\nContent-Length: {send_len}\r\nContent-Range: bytes {start}-{}-{total}\r\nConnection: close\r\n\r\n",
                    start.saturating_add(send_len).saturating_sub(1)
                );
                if sock.write_all(header.as_bytes()).await.is_err() {
                    return;
                }
                let body = &payload[start..start + send_len];
                let _ = sock.write_all(body).await;
                let _ = sock.flush().await;
                if cut {
                    let _ = sock.shutdown().await;
                }
            });
        }
    });

    (format!("http://{addr}/file.bin"), hits)
}

fn parse_range(req: &str, total: usize) -> Option<(usize, usize)> {
    let lower = req.to_ascii_lowercase();
    let pos = lower.find("range: bytes=")?;
    let rest = &req[pos + "range: bytes=".len()..];
    let line: String = rest.lines().next()?.to_string();
    let (a, b) = line.split_once('-')?;
    let start: usize = a.trim().parse().ok()?;
    let end: usize = if b.trim().is_empty() {
        total.saturating_sub(1)
    } else {
        b.trim().parse().ok()?
    };
    Some((start, end.min(total.saturating_sub(1))))
}

fn session() -> Session {
    Session {
        ndus: "test".to_string(),
        js_token: "test".to_string(),
        bdstoken: None,
        uk: Some("1".to_string()),
        user_agent: terabox_core::DEFAULT_UA.to_string(),
        acquired_at: chrono::Utc::now().timestamp(),
    }
}

fn payload(n: usize) -> Vec<u8> {
    (0..n).map(|i| (i % 251) as u8).collect()
}

#[tokio::test]
async fn split_download_writes_every_byte() {
    let data = payload(300_000);
    let (url, _hits) = spawn_stub(data.clone(), false).await;
    let dir = tempfile::tempdir().expect("tmp");
    let dest = dir.path().join("out.bin");

    let downloader = Downloader::new(&session()).expect("downloader");
    let mut ledger = Ledger::open_in_memory().expect("ledger");
    let req = DownloadRequest {
        remote_path: "/file.bin".to_string(),
        fs_id: "1".to_string(),
        local_path: dest.clone(),
        expected_size: Some(data.len() as u64),
        connections: 4,
        part_size: 64 * 1024,
    };

    let mut saw_progress = false;
    let outcome = downloader
        .download(&url, &req, &mut ledger, |p| {
            if matches!(p, Progress::Overall { .. }) {
                saw_progress = true;
            }
        })
        .await
        .expect("download");

    assert_eq!(outcome.bytes, data.len() as u64);
    assert_eq!(outcome.resumed_from, 0);
    assert!(saw_progress, "進捗イベントが一度も出ていません");

    let written = std::fs::read(&dest).expect("read");
    assert_eq!(written.len(), data.len());
    assert_eq!(written, data, "内容が一致しません");
}

#[tokio::test]
async fn interrupted_download_resumes_from_ledger() {
    let data = payload(200_000);
    let (url, _hits) = spawn_stub(data.clone(), true).await;
    let dir = tempfile::tempdir().expect("tmp");
    let dest = dir.path().join("resume.bin");

    let downloader = Downloader::new(&session()).expect("downloader");
    let mut ledger = Ledger::open_in_memory().expect("ledger");
    let req = DownloadRequest {
        remote_path: "/file.bin".to_string(),
        fs_id: "1".to_string(),
        local_path: dest.clone(),
        expected_size: Some(data.len() as u64),
        connections: 2,
        part_size: 64 * 1024,
    };

    // 1 回目は途中で切れて失敗するはず。
    let first = downloader
        .download(&url, &req, &mut ledger, |_| {})
        .await;
    assert!(first.is_err(), "最初の実行が成功してしまいました（中断の再現に失敗）");
    let after_first = std::fs::metadata(&dest).map(|m| m.len()).unwrap_or(0);
    println!("1 回目で書けたバイト数: {after_first}");
    assert!(after_first > 0, "1 回目で 1 バイトも書けていません");

    // 2 回目は続きから。スタブは今回から正しく返す。
    let outcome = downloader
        .download(&url, &req, &mut ledger, |_| {})
        .await
        .expect("resume download");
    assert_eq!(outcome.bytes, data.len() as u64);
    println!("再開時に再利用したバイト数: {}", outcome.resumed_from);

    let written = std::fs::read(&dest).expect("read");
    assert_eq!(written.len(), data.len());
    assert_eq!(written, data, "再開後の内容が一致しません");
}

#[tokio::test]
async fn read_range_returns_only_requested_window() {
    let data = payload(50_000);
    let (url, _hits) = spawn_stub(data.clone(), false).await;
    let downloader = Downloader::new(&session()).expect("downloader");

    let slice = downloader
        .read_range(&url, 1000, 500)
        .await
        .expect("read_range");
    assert_eq!(slice.len(), 500);
    assert_eq!(slice, &data[1000..1500]);
}
