//! 暗号化された VFS。`RemoteFs` を包み、パスと内容を透過的に暗号化する。
//!
//! 上位（WinFsp / FUSE / WebDAV）は平文のパスと平文の内容だけを見る。
//! 下位（TeraBox）は暗号化された名前と暗号文だけを見る。この層が両者の
//! 変換を担う。
//!
//! ## 一覧の扱い
//! 親の一覧には暗号化された名前が並ぶ。復号できない名前（平文のまま置かれた
//! ファイルや、別の鍵で作られたもの）は**一覧から落とす**。落とさずに出すと、
//! 平文の名前がそのまま見えて「暗号化されている」という前提が崩れる。
//! 落とした件数は stderr に 1 行だけ出す（黙って消さない）。

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use terabox_core::crypt::Cipher;
use terabox_core::models::FsEntry;
use terabox_core::vfs::{normalize_path, parent_path, RemoteFs, RemoteMeta};
use terabox_core::TbxError;

/// 暗号ヘッダのキャッシュ有効期間。
///
/// ヘッダは magic と nonce（32 バイト）で、ファイル作成時に決まり以後変わらない。
/// 読み出しのたびに 32 バイトを取りに行くと HTTP の往復が 1 回増える。
const HEADER_TTL: Duration = Duration::from_secs(10 * 60);

/// 平文パスごとの暗号ヘッダ。
struct HeaderEntry {
    header: Vec<u8>,
    /// キャッシュした時点の暗号文サイズ。サイズが変われば作り直しとみなす。
    sealed_size: u64,
    fetched: Instant,
}

/// 暗号化層。
pub struct CryptFs {
    inner: Arc<dyn RemoteFs + Send + Sync>,
    cipher: Arc<Cipher>,
    /// 暗号ヘッダのキャッシュ（平文パス -> ヘッダ）。
    headers: Mutex<HashMap<String, HeaderEntry>>,
}

impl CryptFs {
    /// 包む。
    pub fn new(inner: Arc<dyn RemoteFs + Send + Sync>, cipher: Cipher) -> Self {
        Self {
            inner,
            cipher: Arc::new(cipher),
            headers: Mutex::new(HashMap::new()),
        }
    }

    /// 暗号ヘッダを取得する（キャッシュ経由）。
    ///
    /// `inner_size` は暗号文全体のサイズ。ファイルが作り直されるとサイズが
    /// 変わるので、それを見てキャッシュを捨てる。
    async fn header_for(
        &self,
        remote: &str,
        plain: &str,
        inner_size: u64,
    ) -> Result<Vec<u8>, TbxError> {
        {
            let cache = self.headers.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(hit) = cache.get(plain) {
                if hit.fetched.elapsed() < HEADER_TTL && hit.sealed_size == inner_size {
                    return Ok(hit.header.clone());
                }
            }
        }
        let header = self
            .inner
            .read_range(remote, 0, terabox_core::crypt::HEADER_SIZE as u64)
            .await?;
        {
            let mut cache = self.headers.lock().unwrap_or_else(|e| e.into_inner());
            cache.insert(
                plain.to_string(),
                HeaderEntry {
                    header: header.clone(),
                    sealed_size: inner_size,
                    fetched: Instant::now(),
                },
            );
        }
        Ok(header)
    }

    /// ヘッダのキャッシュを捨てる（復号の認証に失敗したとき）。
    fn forget_header(&self, plain: &str) {
        let mut cache = self.headers.lock().unwrap_or_else(|e| e.into_inner());
        cache.remove(plain);
    }

    /// 平文パスを暗号化パスへ。`is_dir` で最後のセグメントの扱いが変わる。
    pub fn to_remote(&self, plain: &str, is_dir: bool) -> String {
        let normalized = normalize_path(plain);
        if normalized == "/" {
            return "/".to_string();
        }
        let trimmed = normalized.trim_start_matches('/');
        let encrypted = self.cipher.encrypt_path(trimmed, is_dir);
        format!("/{encrypted}")
    }

    /// 暗号化パスを平文パスへ。
    pub fn to_plain(&self, remote: &str, is_dir: bool) -> Result<String, TbxError> {
        let normalized = normalize_path(remote);
        if normalized == "/" {
            return Ok("/".to_string());
        }
        let trimmed = normalized.trim_start_matches('/');
        let plain = self.cipher.decrypt_path(trimmed, is_dir)?;
        Ok(format!("/{plain}"))
    }
}

/// 復号後のサイズを返す。暗号化されていないファイルはそのままのサイズ。
fn decrypted_size(cipher: &Cipher, size: u64) -> u64 {
    cipher.decrypted_size(size).unwrap_or(size)
}

#[async_trait::async_trait]
impl RemoteFs for CryptFs {
    async fn read_dir(&self, path: &str) -> Result<Vec<FsEntry>, TbxError> {
        let remote = self.to_remote(path, true);
        let entries = self.inner.read_dir(&remote).await?;

        let mut out = Vec::with_capacity(entries.len());
        let mut skipped = 0usize;
        for mut entry in entries {
            let plain = match self.to_plain(&entry.path, entry.isdir) {
                Ok(p) => p,
                Err(_) => {
                    skipped += 1;
                    continue;
                }
            };
            // 平文名に戻して上位へ渡す。
            entry.server_filename = plain
                .rsplit('/')
                .next()
                .unwrap_or_default()
                .to_string();
            entry.path = plain;
            if !entry.isdir {
                entry.size = decrypted_size(&self.cipher, entry.size);
            }
            out.push(entry);
        }
        if skipped > 0 {
            eprintln!(
                "[crypt] {} は復号できない名前の項目を {} 件除外しました（平文か別の鍵で作られたものです）",
                path, skipped
            );
        }
        Ok(out)
    }

    async fn metadata(&self, path: &str) -> Result<Option<RemoteMeta>, TbxError> {
        // ファイルかディレクトリか分からないので、両方の名前で照会する。
        // ファイル名 → ディレクトリ名の順に試す（実体は片方しか存在しない）。
        for is_dir in [false, true] {
            let remote = self.to_remote(path, is_dir);
            if let Some(meta) = self.inner.metadata(&remote).await? {
                let mut meta = meta;
                meta.path = path.to_string();
                if !meta.is_dir {
                    meta.size = decrypted_size(&self.cipher, meta.size);
                }
                return Ok(Some(meta));
            }
        }
        Ok(None)
    }

    async fn read(&self, path: &str) -> Result<Vec<u8>, TbxError> {
        let remote = self.to_remote(path, false);
        let sealed = self.inner.read(&remote).await?;
        self.cipher.decrypt_data(&sealed)
    }

    async fn read_range(&self, path: &str, offset: u64, length: u64) -> Result<Vec<u8>, TbxError> {
        if length == 0 {
            return Ok(Vec::new());
        }
        let remote = self.to_remote(path, false);

        // 暗号文のサイズは内側のメタ情報から取る（ヘッダキャッシュの判定に使う）。
        // ファイルが作り直されるとサイズが変わるので、それを見て捨てる。
        let inner_size = self.inner.metadata(&remote).await?.map(|m| m.size).unwrap_or(0);

        // ヘッダ（magic + nonce、32 バイト）は 1 回だけ取ってキャッシュする。
        // ファイル作成時に決まり以後変わらないため、読み出しのたびに取りに
        // 行くと HTTP の往復が 1 回増えるだけになる。
        let mut header = self.header_for(&remote, path, inner_size).await?;

        let (start, end) = self.cipher.range_plan(offset, length);
        let want = end.saturating_sub(start);
        let mut body = self.inner.read_range(&remote, start, want).await?;

        match self
            .cipher
            .decrypt_range_from(offset, length, &header, &body)
        {
            Ok(data) => Ok(data),
            Err(first_error) => {
                // ヘッダが古い（ファイルが作り直された）可能性がある。キャッシュを
                // 捨てて取り直し、もう一度だけ試す。それでも失敗したら元のエラーを
                // 返す（無限ループにしない）。
                self.forget_header(path);
                header = self
                    .inner
                    .read_range(&remote, 0, terabox_core::crypt::HEADER_SIZE as u64)
                    .await?;
                body = self.inner.read_range(&remote, start, want).await?;
                self.cipher
                    .decrypt_range_from(offset, length, &header, &body)
                    .map_err(|_| first_error)
            }
        }
    }

    async fn write(&self, path: &str, data: &[u8]) -> Result<(), TbxError> {
        let remote = self.to_remote(path, false);
        let sealed = self.cipher.encrypt_data(data);
        // 作り直すと nonce が変わる。古いヘッダを握り続けないよう捨てる。
        self.forget_header(path);
        self.inner.write(&remote, &sealed).await
    }

    async fn delete(&self, path: &str) -> Result<(), TbxError> {
        self.forget_header(path);
        // ファイル名とディレクトリ名の両方を試す。
        let mut last_error = None;
        for is_dir in [false, true] {
            let remote = self.to_remote(path, is_dir);
            match self.inner.delete(&remote).await {
                Ok(()) => return Ok(()),
                Err(e) => last_error = Some(e),
            }
        }
        Err(last_error.unwrap_or_else(|| {
            TbxError::Other(format!("{path} を削除できませんでした"))
        }))
    }

    async fn rename(&self, from: &str, to: &str) -> Result<(), TbxError> {
        self.forget_header(from);
        self.forget_header(to);
        // ディレクトリかファイルか分からないので、両方の組み合わせを試す。
        let mut last_error = None;
        for is_dir in [false, true] {
            let from_remote = self.to_remote(from, is_dir);
            let to_remote = self.to_remote(to, is_dir);
            match self.inner.rename(&from_remote, &to_remote).await {
                Ok(()) => return Ok(()),
                Err(e) => last_error = Some(e),
            }
        }
        Err(last_error.unwrap_or_else(|| {
            TbxError::Other(format!("{from} を {to} へ改名できませんでした"))
        }))
    }

    async fn create_dir(&self, path: &str) -> Result<(), TbxError> {
        let remote = self.to_remote(path, true);
        self.inner.create_dir(&remote).await
    }
}

/// 親ディレクトリのパスを平文で返す（マウント側の助け）。
pub fn plain_parent(path: &str) -> String {
    parent_path(&normalize_path(path))
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use std::collections::HashMap;
    use std::sync::Mutex;
    use terabox_core::crypt::CryptConfig;

    /// メモリ上の偽リモート。暗号化された名前と中身だけを持つ。
    struct FakeRemote {
        entries: Mutex<HashMap<String, (Vec<u8>, bool)>>,
        /// `read_range` が呼ばれた回数（ヘッダ取得の重複を数える）。
        reads: std::sync::atomic::AtomicUsize,
    }

    impl FakeRemote {
        fn new() -> Self {
            let mut map = HashMap::new();
            map.insert("/".to_string(), (Vec::new(), true));
            Self {
                entries: Mutex::new(map),
                reads: std::sync::atomic::AtomicUsize::new(0),
            }
        }

        fn read_count(&self) -> usize {
            self.reads.load(std::sync::atomic::Ordering::SeqCst)
        }

        fn reset_reads(&self) {
            self.reads
                .store(0, std::sync::atomic::Ordering::SeqCst);
        }
    }

    #[async_trait]
    impl RemoteFs for FakeRemote {
        async fn read_dir(&self, path: &str) -> Result<Vec<FsEntry>, TbxError> {
            let map = self.entries.lock().unwrap();
            let dir = normalize_path(path);
            let mut out = Vec::new();
            for (p, (data, is_dir)) in map.iter() {
                if *p == dir {
                    continue;
                }
                if parent_path(p) != dir {
                    continue;
                }
                out.push(FsEntry {
                    fs_id: p.clone(),
                    server_filename: p.rsplit('/').next().unwrap_or_default().to_string(),
                    path: p.clone(),
                    isdir: *is_dir,
                    size: if *is_dir { 0 } else { data.len() as u64 },
                    server_mtime: 0,
                    server_ctime: 0,
                    md5: String::new(),
                    thumbs: None,
                    category: None,
                });
            }
            Ok(out)
        }

        async fn metadata(&self, path: &str) -> Result<Option<RemoteMeta>, TbxError> {
            let map = self.entries.lock().unwrap();
            Ok(map.get(&normalize_path(path)).map(|(data, is_dir)| RemoteMeta {
                path: normalize_path(path),
                size: if *is_dir { 0 } else { data.len() as u64 },
                is_dir: *is_dir,
                mtime: 0,
                fs_id: path.to_string(),
            }))
        }

        async fn read(&self, path: &str) -> Result<Vec<u8>, TbxError> {
            let map = self.entries.lock().unwrap();
            map.get(&normalize_path(path))
                .map(|(d, _)| d.clone())
                .ok_or_else(|| TbxError::Other("無い".into()))
        }

        async fn read_range(
            &self,
            path: &str,
            offset: u64,
            length: u64,
        ) -> Result<Vec<u8>, TbxError> {
            self.reads
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            let data = self.read(path).await?;
            let start = (offset as usize).min(data.len());
            let end = (start + length as usize).min(data.len());
            Ok(data[start..end].to_vec())
        }

        async fn write(&self, path: &str, data: &[u8]) -> Result<(), TbxError> {
            let mut map = self.entries.lock().unwrap();
            map.insert(normalize_path(path), (data.to_vec(), false));
            Ok(())
        }

        async fn delete(&self, path: &str) -> Result<(), TbxError> {
            let mut map = self.entries.lock().unwrap();
            map.remove(&normalize_path(path));
            Ok(())
        }

        async fn rename(&self, from: &str, to: &str) -> Result<(), TbxError> {
            let mut map = self.entries.lock().unwrap();
            if let Some(v) = map.remove(&normalize_path(from)) {
                map.insert(normalize_path(to), v);
            }
            Ok(())
        }

        async fn create_dir(&self, path: &str) -> Result<(), TbxError> {
            let mut map = self.entries.lock().unwrap();
            map.insert(normalize_path(path), (Vec::new(), true));
            Ok(())
        }
    }

    fn fs() -> CryptFs {
        let inner = Arc::new(FakeRemote::new()) as Arc<dyn RemoteFs + Send + Sync>;
        let cipher =
            Cipher::new("password", "salt", &CryptConfig::default()).expect("鍵を作れる");
        CryptFs::new(inner, cipher)
    }

    #[tokio::test]
    async fn write_stores_only_ciphertext() {
        let fs = fs();
        fs.write("/secret.txt", b"plaintext content").await.unwrap();

        // 下位を直接見ると、暗号化された名前と暗号文しか無い。
        let remote = fs.to_remote("/secret.txt", false);
        let raw = fs.inner.read(&remote).await.unwrap();
        assert_eq!(&raw[..8], b"RCLONE\x00\x00", "magic が無い");
        assert!(
            !raw.windows(17).any(|w| w == b"plaintext content"),
            "平文がそのまま保存されている"
        );
        let name = remote.trim_start_matches('/');
        assert!(
            !name.contains("secret"),
            "平文の名前がリモートに残っている: {name}"
        );
    }

    #[tokio::test]
    async fn read_returns_plaintext() {
        let fs = fs();
        fs.write("/a.bin", b"hello").await.unwrap();
        assert_eq!(fs.read("/a.bin").await.unwrap(), b"hello");
    }

    #[tokio::test]
    async fn list_shows_plain_names_and_size() {
        let fs = fs();
        fs.write("/notes/today.txt", b"1234567890").await.unwrap();
        let entries = fs.read_dir("/notes").await.unwrap();
        assert_eq!(entries.len(), 1, "1 件のはず");
        assert_eq!(entries[0].server_filename, "today.txt");
        assert_eq!(entries[0].path, "/notes/today.txt");
        assert_eq!(entries[0].size, 10, "復号後のサイズでなければならない");
    }

    #[tokio::test]
    async fn plain_files_are_hidden_from_listing() {
        let fs = fs();
        // 下位へ直接、平文の名前で置く（移行前のファイルを模す）。
        fs.inner.write("/plain.txt", b"not encrypted").await.unwrap();
        let entries = fs.read_dir("/").await.unwrap();
        assert!(
            entries.iter().all(|e| e.server_filename != "plain.txt"),
            "暗号化されていないファイルが一覧に出ている"
        );

        // 暗号化されたファイルは出る。
        fs.write("/enc.txt", b"encrypted").await.unwrap();
        let entries = fs.read_dir("/").await.unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].server_filename, "enc.txt");
    }

    #[tokio::test]
    async fn read_range_matches_full_read() {
        let fs = fs();
        let plain: Vec<u8> = (0..200_000u32).map(|i| (i % 251) as u8).collect();
        fs.write("/big.bin", &plain).await.unwrap();

        for (offset, length) in [(0u64, 100u64), (65530, 200), (130_000, 70_000)] {
            let got = fs.read_range("/big.bin", offset, length).await.unwrap();
            let start = offset as usize;
            let end = (start + length as usize).min(plain.len());
            assert_eq!(got, plain[start..end].to_vec(), "offset={offset}");
        }
    }

    #[tokio::test]
    async fn header_is_read_once_and_cached() {
        let inner = Arc::new(FakeRemote::new());
        let cipher = Cipher::new("password", "salt", &CryptConfig::default()).expect("鍵");
        let fs = CryptFs::new(
            Arc::clone(&inner) as Arc<dyn RemoteFs + Send + Sync>,
            cipher,
        );
        fs.write("/h.bin", b"0123456789").await.unwrap();

        // 1 回目の読み出しはヘッダ + 本文で 2 回。
        inner.reset_reads();
        fs.read_range("/h.bin", 2, 4).await.unwrap();
        let first = inner.read_count();
        assert_eq!(first, 2, "初回はヘッダと本文で 2 回のはず: {first}");

        // 2 回目以降はヘッダがキャッシュされ、本文の 1 回だけで済む。
        inner.reset_reads();
        fs.read_range("/h.bin", 5, 3).await.unwrap();
        let second = inner.read_count();
        assert_eq!(second, 1, "ヘッダはキャッシュされ 1 回だけになるはず: {second}");
    }

    #[tokio::test]
    async fn header_cache_is_dropped_when_file_is_rewritten() {
        let inner = Arc::new(FakeRemote::new());
        let cipher = Cipher::new("password", "salt", &CryptConfig::default()).expect("鍵");
        let fs = CryptFs::new(
            Arc::clone(&inner) as Arc<dyn RemoteFs + Send + Sync>,
            cipher,
        );
        fs.write("/r.bin", &vec![1u8; 1000]).await.unwrap();
        let before = fs.read_range("/r.bin", 0, 10).await.unwrap();

        // サイズを変えて作り直す。ヘッダキャッシュはサイズの変化で捨てられる。
        fs.write("/r.bin", &vec![2u8; 2000]).await.unwrap();
        let after = fs.read_range("/r.bin", 0, 10).await.unwrap();
        assert_eq!(after, vec![2u8; 10], "作り直した内容が返るはず");
        assert_ne!(before, after, "以前の内容と区別できるはず");
    }

    #[tokio::test]
    async fn metadata_reports_decrypted_size() {
        let fs = fs();
        fs.write("/m.bin", b"abc").await.unwrap();
        let meta = fs.metadata("/m.bin").await.unwrap().expect("メタ情報");
        assert_eq!(meta.size, 3, "復号後のサイズでなければならない");
        assert!(!meta.is_dir);
        assert_eq!(meta.path, "/m.bin");
    }

    #[tokio::test]
    async fn delete_removes_both_name_shapes() {
        let fs = fs();
        fs.write("/d.bin", b"x").await.unwrap();
        fs.delete("/d.bin").await.unwrap();
        assert!(fs.metadata("/d.bin").await.unwrap().is_none());
    }

    #[tokio::test]
    async fn create_dir_roundtrips_through_listing() {
        let fs = fs();
        fs.create_dir("/sub").await.unwrap();
        let entries = fs.read_dir("/").await.unwrap();
        assert!(entries.iter().any(|e| e.server_filename == "sub" && e.isdir));
    }

    #[tokio::test]
    async fn rename_changes_only_the_name() {
        let fs = fs();
        fs.write("/old.txt", b"content").await.unwrap();
        fs.rename("/old.txt", "/new.txt").await.unwrap();
        assert_eq!(fs.read("/new.txt").await.unwrap(), b"content");
        assert!(fs.metadata("/old.txt").await.unwrap().is_none());
    }
}
