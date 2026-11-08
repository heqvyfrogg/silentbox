//! `RemoteFs` の TeraBox 実装。
//!
//! マウントの両バックエンド（WinFsp / WebDAV）はこれ 1 つを共有する。
//! 一覧は親ディレクトリの `/api/list` を引き、パス→エントリの対応を短期
//! キャッシュする（エクスプローラは同じディレクトリを繰り返し読むため）。

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use terabox_core::api::ApiClient;
use terabox_core::error::TbxError;
use terabox_core::models::FsEntry;
use terabox_core::session::Session;
use terabox_core::transfer::Downloader;
use terabox_core::vfs::{base_name, normalize_path, parent_path, RemoteFs, RemoteMeta};

/// キャッシュの有効期間。短すぎると API を叩きすぎ、長すぎると更新が見えない。
const CACHE_TTL: Duration = Duration::from_secs(15);

/// 容量情報のキャッシュ有効期間。
///
/// 残量は書き込みで変わるが、毎回 `/api/quota` を叩くと重い。30 秒で取り替える。
const QUOTA_TTL: Duration = Duration::from_secs(30);

/// dlink のキャッシュ有効期間。
///
/// dlink は `/rest/2.0/pcs/file` を 1 往復して得る署名付き URL。マウントの
/// 読み出しは 1 チャンクごとに `read_range` を呼ぶので、毎回取り直すと
/// HTTP の往復が倍になる。dlink は失効し得るため、403 が返ったら捨てて
/// 1 回だけ取り直す。
const DLINK_TTL: Duration = Duration::from_secs(15 * 60);

/// `TBX_TRACE_IO=1` のとき、計測用の 1 行を stderr へ出す。
///
/// 速度改善を体感ではなく回数と時間で示すための仕掛け。通常は無効。
fn trace_io(kind: &str, path: &str, offset: u64, bytes: u64, micros: u128, extra: &str) {
    if std::env::var_os("TBX_TRACE_IO").is_none() {
        return;
    }
    eprintln!(
        "[io] {kind} {path} offset={offset} bytes={bytes} micros={micros} {extra}"
    );
}

/// 平文パスごとの dlink とサイズ。
struct DlinkEntry {
    dlink: String,
    size: u64,
    fetched: Instant,
}

struct CacheEntry {
    entries: Vec<FsEntry>,
    fetched: Instant,
}

/// 1 回分の容量情報。
#[derive(Debug, Clone, Copy)]
pub struct VolumeUsage {
    /// 総容量（バイト）。
    pub total: u64,
    /// 残量（バイト）。
    pub free: u64,
}

/// TeraBox を `RemoteFs` として使うアダプタ。
pub struct TeraBoxFs {
    api: ApiClient,
    cache: Mutex<HashMap<String, CacheEntry>>,
    /// 容量の短期キャッシュ。`None` は「まだ取れていない」。
    quota: Mutex<Option<(VolumeUsage, Instant)>>,
    /// dlink のキャッシュ（平文パス -> 署名付き URL）。
    dlinks: Mutex<HashMap<String, DlinkEntry>>,
    /// 読み出し用の HTTP クライアント。1 個を作って使い回す。
    downloader: Downloader,
    /// 書き込みで使う分割サイズ。
    part_size: u64,
}

impl TeraBoxFs {
    pub fn new(api: ApiClient, session: Session) -> Self {
        Self::with_part_size(api, session, 4 * 1024 * 1024)
    }

    /// 分割サイズを指定して作る。
    ///
    /// `part_size` を大きくすると、大きいファイルのアップロードでパート数が
    /// 減り、`precreate` とパート送信の往復が減る。
    pub fn with_part_size(api: ApiClient, session: Session, part_size: u64) -> Self {
        // 読み出し用のクライアントは 1 個だけ作る（`read_range` ごとに作らない）。
        let downloader = Downloader::new(&session).expect("ダウンロード用クライアントを作れませんでした");
        Self {
            api,
            cache: Mutex::new(HashMap::new()),
            quota: Mutex::new(None),
            dlinks: Mutex::new(HashMap::new()),
            downloader,
            part_size,
        }
    }

    /// dlink を取り出す（キャッシュ経由）。サイズが変わっていたら取り直す。
    async fn dlink_for(&self, path: &str, size: u64) -> Result<String, TbxError> {
        {
            let cache = self.dlinks.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(hit) = cache.get(path) {
                if hit.fetched.elapsed() < DLINK_TTL && hit.size == size {
                    trace_io("dlink", path, 0, 0, 0, "cache=hit");
                    return Ok(hit.dlink.clone());
                }
            }
        }
        let started = Instant::now();
        let dlink = self.api.dlink(path).await?;
        trace_io("dlink", path, 0, 0, started.elapsed().as_micros(), "cache=miss");
        {
            let mut cache = self.dlinks.lock().unwrap_or_else(|e| e.into_inner());
            cache.insert(
                path.to_string(),
                DlinkEntry {
                    dlink: dlink.clone(),
                    size,
                    fetched: Instant::now(),
                },
            );
        }
        Ok(dlink)
    }

    /// キャッシュした dlink を捨てる（403 のとき）。
    fn forget_dlink(&self, path: &str) {
        let mut cache = self.dlinks.lock().unwrap_or_else(|e| e.into_inner());
        cache.remove(path);
    }

    /// サーバの容量と残量を返す。取れなければ `None`（推測値で埋めない）。
    pub async fn usage(&self) -> Option<VolumeUsage> {
        {
            let guard = self.quota.lock().unwrap_or_else(|e| e.into_inner());
            if let Some((usage, at)) = &*guard {
                if at.elapsed() < QUOTA_TTL {
                    return Some(*usage);
                }
            }
        }

        let quota = match self.api.quota().await {
            Ok(q) => q,
            Err(_) => return None,
        };
        let total = quota.total_bytes?;
        let free = quota.free_bytes()?;
        if total == 0 {
            return None;
        }

        let usage = VolumeUsage { total, free };
        let mut guard = self.quota.lock().unwrap_or_else(|e| e.into_inner());
        *guard = Some((usage, Instant::now()));
        Some(usage)
    }

    /// 容量キャッシュを捨てる（書き込み・削除の後に呼ぶ）。
    pub fn invalidate_usage(&self) {
        let mut guard = self.quota.lock().unwrap_or_else(|e| e.into_inner());
        *guard = None;
    }

    /// ディレクトリ一覧を取得する（キャッシュ経由）。
    async fn entries(&self, dir: &str) -> Result<Vec<FsEntry>, TbxError> {
        let key = normalize_path(dir);
        {
            let cache = self.cache.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(hit) = cache.get(&key) {
                if hit.fetched.elapsed() < CACHE_TTL {
                    return Ok(hit.entries.clone());
                }
            }
        }
        let resp = self.api.list(&key, 1, 2000, "name", false).await?;
        let entries = resp.list;
        {
            let mut cache = self.cache.lock().unwrap_or_else(|e| e.into_inner());
            cache.insert(
                key,
                CacheEntry {
                    entries: entries.clone(),
                    fetched: Instant::now(),
                },
            );
        }
        Ok(entries)
    }

    /// パス 1 件のエントリを探す。
    async fn find(&self, path: &str) -> Result<Option<FsEntry>, TbxError> {
        let p = normalize_path(path);
        if p == "/" {
            return Ok(Some(FsEntry {
                fs_id: "0".to_string(),
                server_filename: String::new(),
                path: "/".to_string(),
                isdir: true,
                size: 0,
                server_mtime: 0,
                server_ctime: 0,
                md5: String::new(),
                thumbs: None,
                category: None,
            }));
        }
        let parent = parent_path(&p);
        let name = base_name(&p);
        let list = self.entries(&parent).await?;
        Ok(list.into_iter().find(|e| e.display_name() == name))
    }
}

#[async_trait]
impl RemoteFs for TeraBoxFs {
    async fn read_dir(&self, path: &str) -> Result<Vec<FsEntry>, TbxError> {
        self.entries(path).await
    }

    async fn metadata(&self, path: &str) -> Result<Option<RemoteMeta>, TbxError> {
        Ok(self.find(path).await?.as_ref().map(RemoteMeta::from_entry))
    }

    async fn read(&self, path: &str) -> Result<Vec<u8>, TbxError> {
        let meta = self
            .metadata(path)
            .await?
            .ok_or_else(|| TbxError::Other(format!("{path} が見つかりません")))?;
        if meta.is_dir {
            return Err(TbxError::Other(format!("{path} はフォルダです")));
        }
        self.read_range(path, 0, meta.size).await
    }

    async fn read_range(&self, path: &str, offset: u64, length: u64) -> Result<Vec<u8>, TbxError> {
        let meta = self
            .metadata(path)
            .await?
            .ok_or_else(|| TbxError::Other(format!("{path} が見つかりません")))?;
        if meta.is_dir {
            return Err(TbxError::Other(format!("{path} はフォルダです")));
        }

        // 終端を超える要求は空を返す。dlink サーバは範囲外の `Range` に
        // HTTP 400 を返し、WinFsp 側が「I/O device error」になる（実測）。
        // WinFsp は EOF 確認のために size ちょうどの offset で read を投げる。
        if offset >= meta.size {
            return Ok(Vec::new());
        }
        let want = length.min(meta.size - offset);
        if want == 0 {
            return Ok(Vec::new());
        }

        let dlink = self.dlink_for(&meta.path, meta.size).await?;
        let started = Instant::now();
        let first = self.downloader.read_range(&dlink, offset, want).await;

        match first {
            Ok(data) => {
                trace_io(
                    "range",
                    &meta.path,
                    offset,
                    data.len() as u64,
                    started.elapsed().as_micros(),
                    "",
                );
                Ok(data)
            }
            Err(e) => {
                // dlink は失効し得る。403（errno -6）が返ったらキャッシュを捨てて
                // 1 回だけ取り直す。それでも失敗したらそのまま返す（無限ループに
                // しない）。
                if matches!(&e, TbxError::Api(err) if err.errno == -6) {
                    self.forget_dlink(&meta.path);
                    let dlink = self.dlink_for(&meta.path, meta.size).await?;
                    self.downloader
                        .read_range(&dlink, offset, want)
                        .await
                        .map_err(|e| TbxError::Other(e.to_string()))
                } else {
                    Err(TbxError::Other(e.to_string()))
                }
            }
        }
    }

    async fn write(&self, path: &str, data: &[u8]) -> Result<(), TbxError> {
        let normalized = normalize_path(path);

        // 既存ファイルがあれば先に消す。`/api/create` は同名パスを上書きせず
        // `name_20261106_234426.bin` のように別名で作る（実測）。削除してから
        // 送ると同じパスに置ける（実測で確認済み）。
        if self.find(&normalized).await?.is_some() {
            self.api.delete(&[normalized.clone()]).await?;
            // 削除の反映を待つ。直後に create すると、まだ残っているものとして
            // 別名が付くことがある。
            tokio::time::sleep(std::time::Duration::from_millis(400)).await;
        }

        // 一時ファイルへ書いてからアップロードする（部分アップロードを避ける）。
        let tmp = tempfile::NamedTempFile::new()?;
        std::fs::write(tmp.path(), data)?;
        let mut uploader = terabox_core::transfer::Uploader::new(self.api.clone());
        let mut ledger = terabox_core::store::Ledger::open(&terabox_core::config::ledger_path())?;
        let req = terabox_core::transfer::UploadRequest {
            local_path: tmp.path().to_path_buf(),
            remote_dir: parent_path(&normalized),
            remote_name: Some(base_name(&normalized)),
            part_size: self.part_size,
        };
        let result = uploader
            .upload(&req, &mut ledger, |_| {})
            .await
            .map(|_| ())
            .map_err(|e| TbxError::Other(e.to_string()));
        // 書いた後は親ディレクトリのキャッシュと容量を捨てる（削除・作成と同じ理由）。
        self.invalidate(&parent_path(&normalized));
        self.invalidate_usage();
        result
    }

    async fn delete(&self, path: &str) -> Result<(), TbxError> {
        let result = self.api.delete(&[normalize_path(path)]).await.map(|_| ());
        // 削除後は親の一覧キャッシュを捨てる。捨てないと、マウント直後の
        // 列挙で消したはずの項目が最大 TTL 秒だけ残る（実測で発生）。
        self.invalidate(&parent_path(path));
        self.invalidate_usage();
        result
    }

    async fn rename(&self, from: &str, to: &str) -> Result<(), TbxError> {
        let from_n = normalize_path(from);
        let to_n = normalize_path(to);
        if from_n == to_n {
            return Ok(());
        }

        // `/api/filemanager?opera=rename` は、どのパラメータ形でも `errno 2`
        // （param err）を返した（実測: filelist を配列/オブジェクト、newname を
        // 名前/絶対パス、async あり/なしの 6 通りを試した）。仕様を特定できて
        // いないため推測で実装せず、読み出し→再アップロード→削除で代替する。
        let meta = self
            .metadata(&from_n)
            .await?
            .ok_or_else(|| TbxError::Other(format!("{from_n} が見つかりません")))?;

        if meta.is_dir {
            return Err(TbxError::Other(
                "フォルダの名前変更は未対応です（API の仕様が未確定）".to_string(),
            ));
        }

        let data = self.read(&from_n).await?;
        self.write(&to_n, &data).await?;
        self.api.delete(&[from_n.clone()]).await?;
        self.invalidate(&parent_path(&from_n));
        self.invalidate(&parent_path(&to_n));
        Ok(())
    }

    async fn create_dir(&self, path: &str) -> Result<(), TbxError> {
        let normalized = normalize_path(path);
        let result = self.api.create_dir(&normalized).await.map(|_| ());
        self.invalidate(&parent_path(&normalized));
        result
    }
}

/// 一覧キャッシュを明示的に捨てる（アップロード後など）。
impl TeraBoxFs {
    pub fn invalidate(&self, dir: &str) {
        let mut cache = self.cache.lock().unwrap_or_else(|e| e.into_inner());
        cache.remove(&normalize_path(dir));
    }
}
