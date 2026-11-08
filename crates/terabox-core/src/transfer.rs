//! 転送エンジン（分割ダウンロード / 分割アップロード）。
//!
//! ## ダウンロード
//! dlink に対して `Range` を指定し、`conn` 本の分割を並行に流す。各分割は
//! 出力ファイルの該当オフセットへ直接書き込み（`seek` + `write`）、台帳の
//! `written` を逐次更新する。再開時は `written` の続きから再開する。
//!
//! ## アップロード
//! `precreate` で `uploadid` を取り、パート単位で MD5 を計算して
//! `superfile2` へ送る。送信済みパートは台帳に記録し、再開時に飛ばす。
//!
//! ## 速度制御
//! 接続の開始を時間軸へ均等分散させ、数百 ms のジッターを入れる。無料枠の
//! 帯域制限下では、同時バーストで全接続が絞られるより分散した方が総量が伸びる。

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use futures::stream::{FuturesUnordered, StreamExt};
use reqwest::header::{CONTENT_RANGE, RANGE, USER_AGENT};
use reqwest::Client;
use tokio::fs::OpenOptions;
use tokio::io::{AsyncSeekExt, AsyncWriteExt};
use tokio::sync::mpsc;

use crate::api::ApiClient;
use crate::error::{ApiError, TbxError};
use crate::session::Session;
use crate::store::{Kind, Ledger, State};

/// 転送の進捗イベント。
#[derive(Debug, Clone)]
pub enum Progress {
    /// 転送全体の進捗。
    Overall {
        /// 完了バイト数。
        done: u64,
        /// 総バイト数。
        total: u64,
        /// 現在の速度（バイト/秒）。
        bytes_per_sec: f64,
        /// 分割ごとの状態。
        parts: Vec<PartProgress>,
    },
    /// 情報メッセージ（人間向け）。
    Note(String),
}

/// 分割 1 本の進捗。
#[derive(Debug, Clone)]
pub struct PartProgress {
    pub index: u32,
    pub written: u64,
    pub length: u64,
    pub state: State,
}

/// ダウンロード要求。
#[derive(Debug, Clone)]
pub struct DownloadRequest {
    /// リモートの絶対パス。
    pub remote_path: String,
    /// `fs_id`（分からなければ空）。
    pub fs_id: String,
    /// 保存先ファイルパス。
    pub local_path: PathBuf,
    /// 期待サイズ（不明なら `None`。サーバ応答から決める）。
    pub expected_size: Option<u64>,
    /// 分割接続数。
    pub connections: u8,
    /// 1 分割の目標サイズ（バイト）。
    pub part_size: u64,
}

/// ダウンロード結果。
#[derive(Debug, Clone)]
pub struct DownloadOutcome {
    pub local_path: PathBuf,
    pub bytes: u64,
    pub resumed_from: u64,
}

/// ダウンローダ。
pub struct Downloader {
    client: Client,
    cookie: String,
    session: Session,
}

impl Downloader {
    /// セッションから作る。UA はセッションのものをそのまま使う
    /// （dlink 取得時と一致させる必要がある）。
    pub fn new(session: &Session) -> Result<Self, TbxError> {
        let client = Client::builder()
            .timeout(Duration::from_secs(120))
            .redirect(reqwest::redirect::Policy::limited(5))
            .build()?;
        Ok(Self {
            client,
            cookie: format!("ndus={}; PANWEB=1", session.ndus),
            session: session.clone(),
        })
    }

    /// 既存の `Client` を使って作る。
    ///
    /// `new` は呼ぶたびに接続プールを作る。マウントの読み出しは 1 チャンクごとに
    /// `read_range` を呼ぶため、毎回作るとハンドシェイクとプールの初期化が
    /// 積み上がる。マウント側は 1 個を作って使い回す。
    pub fn with_client(session: &Session, client: Client) -> Self {
        Self {
            client,
            cookie: format!("ndus={}; PANWEB=1", session.ndus),
            session: session.clone(),
        }
    }

    /// dlink からファイルを取得する。`on_progress` は進捗のたびに呼ばれる。
    pub async fn download<F>(
        &self,
        dlink: &str,
        req: &DownloadRequest,
        ledger: &mut Ledger,
        mut on_progress: F,
    ) -> Result<DownloadOutcome, TbxError>
    where
        F: FnMut(Progress) + Send,
    {
        if let Some(parent) = req.local_path.parent() {
            std::fs::create_dir_all(parent)?;
        }

        let total = match req.expected_size {
            Some(n) if n > 0 => n,
            _ => self.probe_size(dlink).await.unwrap_or(0),
        };

        let local_display = req.local_path.display().to_string();
        let transfer_id = ledger.upsert_transfer(
            Kind::Download,
            &req.fs_id,
            &req.remote_path,
            &local_display,
            total,
            req.connections,
        )?;
        let mut parts = ledger.set_parts(transfer_id, total, req.connections, req.part_size)?;

        // 実ファイル長と台帳の食い違いを解消する（安全側 = 実ファイルを正とする）。
        let actual = std::fs::metadata(&req.local_path).map(|m| m.len()).unwrap_or(0);
        if actual < total {
            for part in parts.iter_mut() {
                if part.written > 0 {
                    // 分割の書き込みが実際のファイル長を超えていたら巻き戻す。
                    let end = part.offset + part.written;
                    if end > actual {
                        part.written = 0;
                        ledger.update_part(transfer_id, part.index, 0, State::Partial)?;
                    }
                }
            }
        }

        let resumed_from: u64 = parts.iter().map(|p| p.written).sum();

        if total == 0 {
            // 0 バイト: 空ファイルを作って完了。
            tokio::fs::write(&req.local_path, b"").await?;
            ledger.set_transfer_state(transfer_id, State::Done, 0)?;
            on_progress(Progress::Note("0 バイトのファイルを作成しました".into()));
            return Ok(DownloadOutcome {
                local_path: req.local_path.clone(),
                bytes: 0,
                resumed_from: 0,
            });
        }

        if resumed_from > 0 {
            on_progress(Progress::Note(format!(
                "前回の続きから再開します（{}/{} バイト）",
                resumed_from, total
            )));
        }

        let done_counter = Arc::new(AtomicU64::new(resumed_from));
        let started = Instant::now();

        // 未完了の分割だけを対象にする。
        let pending: Vec<(u32, u64, u64, u64)> = parts
            .iter()
            .filter(|p| p.state != State::Done || p.written < p.length)
            .map(|p| (p.index, p.offset, p.length, p.written))
            .collect();

        let (tx, mut rx) = mpsc::unbounded_channel::<(u32, u64, State)>();
        let mut futures = FuturesUnordered::new();

        let spread = spread_delay(pending.len());
        for (slot, (index, offset, length, written)) in pending.into_iter().enumerate() {
            let client = self.client.clone();
            let cookie = self.cookie.clone();
            let ua = self.session.user_agent.clone();
            let link = dlink.to_string();
            let path = req.local_path.clone();
            let tx = tx.clone();
            let delay = spread.get(slot).copied().unwrap_or(Duration::ZERO);
            let done_counter = Arc::clone(&done_counter);
            futures.push(tokio::spawn(async move {
                // 接続開始を分散させる。先頭は待たず、以降をずらす。
                if !delay.is_zero() {
                    tokio::time::sleep(delay).await;
                }
                let result = fetch_part(
                    &client,
                    &link,
                    &cookie,
                    &ua,
                    &path,
                    index,
                    offset,
                    length,
                    written,
                    tx.clone(),
                    done_counter,
                )
                .await;
                result
            }));
        }
        drop(tx);

        let mut last_emit = Instant::now();
        let mut failed: Vec<String> = Vec::new();
        let total_parts = futures.len();

        loop {
            tokio::select! {
                Some(msg) = rx.recv() => {
                    let (index, written, state) = msg;
                    let _ = ledger.update_part(transfer_id, index, written, state);
                    if last_emit.elapsed() >= Duration::from_millis(200) {
                        emit(&mut on_progress, &mut last_emit, &done_counter, total, ledger, transfer_id, started)?;
                    }
                }
                Some(joined) = futures.next() => {
                    match joined {
                        Ok(Ok(())) => {}
                        Ok(Err(e)) => failed.push(e.to_string()),
                        Err(e) => failed.push(format!("分割タスクが異常終了しました: {e}")),
                    }
                    if futures.is_empty() {
                        break;
                    }
                }
                else => break,
            }
        }

        // 残ったイベントを吐き切る。
        while let Ok((index, written, state)) = rx.try_recv() {
            let _ = ledger.update_part(transfer_id, index, written, state);
        }

        let parts_now = ledger.parts(transfer_id)?;
        let done_now: u64 = parts_now
            .iter()
            .map(|p| p.written.min(p.length))
            .sum();

        if !failed.is_empty() || done_now < total {
            ledger.set_transfer_state(transfer_id, State::Partial, done_now)?;
            emit(
                &mut on_progress,
                &mut last_emit,
                &done_counter,
                total,
                ledger,
                transfer_id,
                started,
            )?;
            let detail = if failed.is_empty() {
                "未完了の分割が残っています".to_string()
            } else {
                failed.join(" / ")
            };
            return Err(TbxError::Other(format!(
                "ダウンロードが完了しませんでした（{done_now}/{total} バイト・{detail}）。再実行すると続きから再開します。"
            )));
        }

        ledger.set_transfer_state(transfer_id, State::Done, total)?;
        emit(
            &mut on_progress,
            &mut last_emit,
            &done_counter,
            total,
            ledger,
            transfer_id,
            started,
        )?;
        on_progress(Progress::Note(format!(
            "{} 分割・{total_parts} 本で完了しました",
            total_parts
        )));

        Ok(DownloadOutcome {
            local_path: req.local_path.clone(),
            bytes: total,
            resumed_from,
        })
    }

    /// `HEAD` もしくは `Range: bytes=0-0` でサイズを測る。
    async fn probe_size(&self, dlink: &str) -> Option<u64> {
        let resp = self
            .client
            .get(dlink)
            .header(USER_AGENT, &self.session.user_agent)
            .header(RANGE, "bytes=0-0")
            .header(reqwest::header::COOKIE, &self.cookie)
            .send()
            .await
            .ok()?;
        if let Some(cr) = resp.headers().get(CONTENT_RANGE) {
            if let Ok(text) = cr.to_str() {
                // `bytes 0-0/12345`
                if let Some(slash) = text.rfind('/') {
                    if let Ok(n) = text[slash + 1..].trim().parse::<u64>() {
                        return Some(n);
                    }
                }
            }
        }
        resp.content_length()
    }

    /// 範囲読み出し（マウントの読み取りに使う）。
    ///
    /// 分割ダウンロードと違い、その場で指定範囲だけを取って返す。巨大ファイルの
    /// 一部分だけを見る用途（プレビュー、先頭ヘッダの確認）を想定する。
    pub async fn read_range(
        &self,
        dlink: &str,
        offset: u64,
        length: u64,
    ) -> Result<Vec<u8>, TbxError> {
        if length == 0 {
            return Ok(Vec::new());
        }
        let end = offset.saturating_add(length).saturating_sub(1);
        let resp = self
            .client
            .get(dlink)
            .header(USER_AGENT, &self.session.user_agent)
            .header(RANGE, format!("bytes={offset}-{end}"))
            .header(reqwest::header::COOKIE, &self.cookie)
            .send()
            .await?;

        let status = resp.status();
        if status == reqwest::StatusCode::FORBIDDEN {
            return Err(TbxError::Api(ApiError {
                endpoint: "dlink".into(),
                errno: -6,
                message: Some("HTTP 403（User-Agent か Cookie の不一致、または dlink 失効）".into()),
                request_id: None,
            }));
        }
        if !status.is_success() {
            return Err(TbxError::Other(format!(
                "範囲取得が HTTP {} で失敗しました",
                status.as_u16()
            )));
        }

        let bytes = resp.bytes().await?;
        // サーバが範囲を無視して全体を返した場合に備え、要求範囲へ切り詰める。
        if bytes.len() as u64 > length {
            let start = if status == reqwest::StatusCode::PARTIAL_CONTENT {
                0
            } else {
                offset as usize
            };
            let end = (start + length as usize).min(bytes.len());
            return Ok(bytes[start..end].to_vec());
        }
        Ok(bytes.to_vec())
    }
}

fn emit<F>(
    on_progress: &mut F,
    last_emit: &mut Instant,
    done: &Arc<AtomicU64>,
    total: u64,
    ledger: &Ledger,
    transfer_id: i64,
    started: Instant,
) -> Result<(), TbxError>
where
    F: FnMut(Progress),
{
    *last_emit = Instant::now();
    let done_now = done.load(Ordering::Relaxed).min(total);
    let secs = started.elapsed().as_secs_f64().max(0.001);
    let parts = ledger
        .parts(transfer_id)?
        .into_iter()
        .map(|p| PartProgress {
            index: p.index,
            written: p.written.min(p.length),
            length: p.length,
            state: p.state,
        })
        .collect();
    on_progress(Progress::Overall {
        done: done_now,
        total,
        bytes_per_sec: done_now as f64 / secs,
        parts,
    });
    Ok(())
}

/// 接続開始の分散スケジュール。先頭は 0、以降は等間隔 + ジッター。
fn spread_delay(count: usize) -> Vec<Duration> {
    if count <= 1 {
        return vec![Duration::ZERO];
    }
    use rand::Rng;
    let mut rng = rand::thread_rng();
    let window_ms = 900.0 / count as f64;
    (0..count)
        .map(|i| {
            if i == 0 {
                Duration::ZERO
            } else {
                let base = window_ms * i as f64;
                let jitter: f64 = rng.gen_range(0.0..200.0);
                Duration::from_millis((base + jitter) as u64)
            }
        })
        .collect()
}

/// 1 分割を取得して出力ファイルの該当位置へ書く。
#[allow(clippy::too_many_arguments)]
async fn fetch_part(
    client: &Client,
    dlink: &str,
    cookie: &str,
    user_agent: &str,
    path: &Path,
    index: u32,
    offset: u64,
    length: u64,
    already: u64,
    tx: mpsc::UnboundedSender<(u32, u64, State)>,
    done: Arc<AtomicU64>,
) -> Result<(), TbxError> {
    if length == 0 {
        let _ = tx.send((index, 0, State::Done));
        return Ok(());
    }
    let start = offset + already;
    let end = offset + length - 1;
    if start > end {
        let _ = tx.send((index, already, State::Done));
        return Ok(());
    }

    let resp = client
        .get(dlink)
        .header(USER_AGENT, user_agent)
        .header(RANGE, format!("bytes={start}-{end}"))
        .header(reqwest::header::COOKIE, cookie)
        .send()
        .await?;

    let status = resp.status();
    if status == reqwest::StatusCode::FORBIDDEN {
        return Err(TbxError::Api(ApiError {
            endpoint: "dlink".into(),
            errno: -6,
            message: Some("HTTP 403（User-Agent か Cookie の不一致、または dlink 失効）".into()),
            request_id: None,
        }));
    }
    if !status.is_success() {
        return Err(TbxError::Other(format!(
            "分割 {index} の取得が HTTP {} で失敗しました",
            status.as_u16()
        )));
    }

    let mut file = OpenOptions::new()
        .create(true)
        .write(true)
        .open(path)
        .await?;
    file.seek(std::io::SeekFrom::Start(start)).await?;

    let mut written = already;
    let mut stream = resp.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk?;
        file.write_all(&chunk).await?;
        let n = chunk.len() as u64;
        written += n;
        done.fetch_add(n, Ordering::Relaxed);
        let _ = tx.send((index, written, State::Partial));
    }
    file.flush().await?;

    if written < length {
        // サーバが途中で切った場合。状態は Partial のまま返し、呼び出し側が再試行する。
        let _ = tx.send((index, written, State::Partial));
        return Err(TbxError::Other(format!(
            "分割 {index} が途中で終わりました（{written}/{length} バイト）"
        )));
    }

    let _ = tx.send((index, length, State::Done));
    Ok(())
}

/// アップロード要求。
#[derive(Debug, Clone)]
pub struct UploadRequest {
    /// ローカルファイル。
    pub local_path: PathBuf,
    /// 保存先ディレクトリ（`/` 始まり）。
    pub remote_dir: String,
    /// 保存名。`None` ならローカルのファイル名。
    pub remote_name: Option<String>,
    /// パートサイズ（バイト）。TeraBox の推奨は 4MiB。
    pub part_size: u64,
}

/// アップロード結果。
#[derive(Debug, Clone)]
pub struct UploadOutcome {
    pub remote_path: String,
    pub bytes: u64,
    pub parts_uploaded: usize,
    pub deduplicated: bool,
}

/// アップローダ。
pub struct Uploader {
    api: ApiClient,
}

impl Uploader {
    pub fn new(api: ApiClient) -> Self {
        Self { api }
    }

    /// アップロードする。送信済みパートは台帳から飛ばす。
    pub async fn upload<F>(
        &mut self,
        req: &UploadRequest,
        ledger: &mut Ledger,
        mut on_progress: F,
    ) -> Result<UploadOutcome, TbxError>
    where
        F: FnMut(Progress) + Send,
    {
        let meta = std::fs::metadata(&req.local_path)?;
        let size = meta.len();
        let name = req
            .remote_name
            .clone()
            .unwrap_or_else(|| {
                req.local_path
                    .file_name()
                    .map(|s| s.to_string_lossy().into_owned())
                    .unwrap_or_else(|| "unnamed".to_string())
            });
        let remote_path = join_remote(&req.remote_dir, &name);
        let local_display = req.local_path.display().to_string();

        // パート分割と MD5 を先に計算する（送信しながら計算すると再開時に不整合になる）。
        let data = tokio::fs::read(&req.local_path).await?;
        let chunk = req.part_size.max(1) as usize;
        let mut md5s: Vec<String> = Vec::new();
        for piece in data.chunks(chunk) {
            use md5::{Digest, Md5};
            let mut h = Md5::new();
            h.update(piece);
            md5s.push(hex::encode(h.finalize()));
        }
        if md5s.is_empty() {
            // 0 バイトでも 1 パートとして扱う。
            use md5::{Digest, Md5};
            let mut h = Md5::new();
            h.update([]);
            md5s.push(hex::encode(h.finalize()));
        }

        let transfer_id = ledger.upsert_transfer(
            Kind::Upload,
            "",
            &remote_path,
            &local_display,
            size,
            1,
        )?;
        let parts = ledger.set_parts(transfer_id, size, 1, req.part_size)?;

        on_progress(Progress::Note(format!(
            "{} を {remote_path} へ送ります（{} パート）",
            req.local_path.display(),
            md5s.len()
        )));

        let pre = self.api.precreate(&remote_path, size, &md5s).await?;
        if pre.is_deduplicated() {
            ledger.set_transfer_state(transfer_id, State::Done, size)?;
            on_progress(Progress::Note(
                "同じ内容のファイルが既にあったため、送信を省きました".into(),
            ));
            return Ok(UploadOutcome {
                remote_path,
                bytes: size,
                parts_uploaded: 0,
                deduplicated: true,
            });
        }
        let upload_id = pre.upload_id.clone().ok_or_else(|| {
            TbxError::Other(
                "アップロード受付が uploadid を返しませんでした（サーバ側の応答形式変更を疑ってください）"
                    .into(),
            )
        })?;

        let mut uploaded = 0usize;
        let started = Instant::now();
        let total_parts = md5s.len();
        for (index, piece) in data.chunks(chunk).enumerate() {
            let part_row = parts
                .iter()
                .find(|p| p.index == index as u32)
                .cloned()
                .unwrap_or(crate::store::PartRow {
                    transfer_id,
                    index: index as u32,
                    offset: (index * chunk) as u64,
                    length: piece.len() as u64,
                    written: 0,
                    state: State::Pending,
                });
            if part_row.state == State::Done {
                on_progress(Progress::Note(format!(
                    "パート {} は送信済みのため飛ばします",
                    index + 1
                )));
                continue;
            }

            let offset = part_row.offset;
            let mut attempts = 0u32;
            loop {
                attempts += 1;
                match self
                    .api
                    .upload_part(&upload_id, &remote_path, index as u32, offset, bytes::Bytes::copy_from_slice(piece))
                    .await
                {
                    Ok(_) => break,
                    Err(e) => {
                        if attempts >= 3 {
                            ledger.set_transfer_state(transfer_id, State::Failed, offset)?;
                            return Err(TbxError::Other(format!(
                                "パート {} の送信に 3 回失敗しました: {e}。再実行すると続きから再開します。",
                                index + 1
                            )));
                        }
                        let backoff = Duration::from_millis(400 * 2u64.pow(attempts - 1));
                        on_progress(Progress::Note(format!(
                            "パート {} を再試行します（{} 回目・{} ms 待機）",
                            index + 1,
                            attempts,
                            backoff.as_millis()
                        )));
                        tokio::time::sleep(backoff).await;
                    }
                }
            }

            ledger.update_part(transfer_id, index as u32, piece.len() as u64, State::Done)?;
            uploaded += 1;
            let done = ((index + 1) * chunk).min(data.len()) as u64;
            let secs = started.elapsed().as_secs_f64().max(0.001);
            on_progress(Progress::Overall {
                done,
                total: size,
                bytes_per_sec: done as f64 / secs,
                parts: Vec::new(),
            });
        }

        // 0 バイトファイルは chunks() が空になるため、明示的に 1 パート扱いで確定する。
        if size == 0 {
            let _ = self
                .api
                .upload_part(&upload_id, &remote_path, 0, 0, bytes::Bytes::new())
                .await;
            uploaded += 1;
            ledger.update_part(transfer_id, 0, 0, State::Done)?;
        }

        self.api
            .create_file(&remote_path, size, &upload_id, &md5s)
            .await?;
        ledger.set_transfer_state(transfer_id, State::Done, size)?;
        on_progress(Progress::Note(format!(
            "アップロード完了: {remote_path}（{total_parts} パート中 {uploaded} パート送信）"
        )));

        Ok(UploadOutcome {
            remote_path,
            bytes: size,
            parts_uploaded: uploaded,
            deduplicated: false,
        })
    }
}

/// リモートパスを結合する。`dir` が `/` で終わっていなくても正しく繋ぐ。
pub fn join_remote(dir: &str, name: &str) -> String {
    let dir = dir.trim_end_matches('/');
    if dir.is_empty() {
        format!("/{name}")
    } else {
        format!("{dir}/{name}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn join_remote_normalizes_slashes() {
        assert_eq!(join_remote("/", "a.txt"), "/a.txt");
        assert_eq!(join_remote("/dir", "a.txt"), "/dir/a.txt");
        assert_eq!(join_remote("/dir/", "a.txt"), "/dir/a.txt");
        assert_eq!(join_remote("", "a.txt"), "/a.txt");
    }

    #[test]
    fn spread_delay_first_is_zero_and_size_matches() {
        let d = spread_delay(4);
        assert_eq!(d.len(), 4);
        assert_eq!(d[0], Duration::ZERO);
        assert!(d[1] > Duration::ZERO);
    }

    #[test]
    fn spread_delay_single_connection() {
        assert_eq!(spread_delay(1), vec![Duration::ZERO]);
        assert_eq!(spread_delay(0), vec![Duration::ZERO]);
    }
}
