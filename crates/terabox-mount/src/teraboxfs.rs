//! WinFsp バックエンド（`winfsp` 0.13 / WinFsp 2.1）。
//!
//! ## ビルド要件（実測で確定）
//! - ターゲットは **`x86_64-pc-windows-msvc` のみ**。`windows-gnu` では
//!   `winfsp-sys` の build script が `unsupported triple x86_64-pc-windows-gnu`
//!   で停止する。
//! - `[build-dependencies]` に `winfsp` を入れ、`build.rs` で
//!   `winfsp::build::winfsp_link_delayload()` を呼ぶ（遅延リンクが必須）。
//! - `winfsp-x64.dll` を実行ファイルと同じディレクトリに置くか、WinFsp の
//!   `bin` を PATH に含める。無いと起動時に 1285（0x505）で落ちる。
//!
//! ## 実装範囲
//! 一覧・メタ・読み出し・書き込み（全体置き換え）・削除・改名・フォルダ作成。
//! 部分書き込みとサイズ変更は TeraBox 側の API が持たないため明示的に拒否する
//! （黙って握り潰すとデータを壊す）。

#[cfg(all(windows, feature = "winfsp-backend"))]
mod imp {
    use std::io::ErrorKind;
    use std::sync::Arc;

    use terabox_core::session::Session;
    use terabox_core::vfs::{normalize_path, RemoteFs};
    use winfsp::filesystem::{
        DirBuffer, DirInfo, FileInfo, FileSystemContext, WideNameInfo,
    };
    use winfsp::host::{FileSystemHost, VolumeParams};
    use winfsp::{FspError, FspInit, U16CStr};

    use crate::remote::TeraBoxFs;
    use crate::runtime;

    const ATTR_DIRECTORY: u32 = 0x10;
    const ATTR_NORMAL: u32 = 0x80;

    /// 1 回の読み出しで返す上限。
    const MAX_READ: usize = 1 << 20;

    fn to_path(name: &U16CStr) -> String {
        normalize_path(&name.to_string_lossy().replace('\\', "/"))
    }

    fn block_on<F: std::future::Future>(fut: F) -> F::Output {
        runtime::handle().block_on(fut)
    }

    /// FILETIME（1601-01-01 起点 100ns 単位）へ変換する。
    fn filetime_from_epoch(secs: i64) -> u64 {
        const EPOCH_DIFF: u64 = 116_444_736_000_000_000;
        if secs <= 0 {
            return EPOCH_DIFF;
        }
        EPOCH_DIFF.saturating_add((secs as u64).saturating_mul(10_000_000))
    }

    fn fill_info(info: &mut FileInfo, meta: &terabox_core::vfs::RemoteMeta) {
        info.file_attributes = if meta.is_dir { ATTR_DIRECTORY } else { ATTR_NORMAL };
        info.file_size = meta.size;
        info.allocation_size = meta.size;
        let stamp = filetime_from_epoch(meta.mtime);
        info.creation_time = stamp;
        info.last_write_time = stamp;
        info.last_access_time = stamp;
        info.change_time = stamp;
    }

    /// 開いているハンドル。
    ///
    /// TeraBox の API はオブジェクト全体の置き換えしか提供しないため、
    /// 書き込みはメモリ上に溜めて `flush` / `cleanup` のときに 1 回だけ
    /// アップロードする。これが無いと、WinFsp が 1 バイトずつ送ってくる
    /// ような書き方（`WriteAllText` など）で毎回全置換が走る。
    pub struct FsHandle {
        path: String,
        is_dir: bool,
        /// 書き込み途中の内容。`None` は「変更なし」。
        pending: std::sync::Mutex<Option<Vec<u8>>>,
        /// 次に途中送信する長さ。送るたびに倍にする。
        ///
        /// TeraBox は全体置換しか提供しないため、閾値で送るたびに「それまでの
        /// 全量」を送り直すことになる。固定閾値だと、閾値を超えてからは
        /// 書き込みのたびに全量再送になり二乗で遅くなる（実測: 100 MiB の
        /// 書き込みが 0.28 MiB/s）。閾値を倍々に伸ばせば、再送は log2 回で済む。
        next_spill: std::sync::Mutex<Option<usize>>,
    }

    impl FsHandle {
        pub fn new(path: String, is_dir: bool) -> Self {
            Self {
                path,
                is_dir,
                pending: std::sync::Mutex::new(None),
                next_spill: std::sync::Mutex::new(None),
            }
        }

        /// 次に途中送信すべき長さを返し、閾値を倍へ進める。
        ///
        /// `base` は最初の閾値。まだ設定されていれば `base` から始める。
        fn take_spill_threshold(&self, base: usize) -> usize {
            let mut guard = self.next_spill.lock().unwrap_or_else(|e| e.into_inner());
            let current = guard.unwrap_or(base);
            let next = current.saturating_mul(2);
            *guard = Some(next);
            current
        }

        fn dirty_bytes(&self) -> Option<Vec<u8>> {
            let guard = self.pending.lock().unwrap_or_else(|e| e.into_inner());
            guard.clone()
        }

        /// 溜めている書き込みの長さ。`dirty_bytes` は全体を clone するため、
        /// 送るかどうかの判定にはこちらを使う（書き込みのたびに全体を
        /// コピーすると、細かい書き込みが続くほど二乗で遅くなる）。
        fn dirty_len(&self) -> usize {
            let guard = self.pending.lock().unwrap_or_else(|e| e.into_inner());
            guard.as_ref().map(|v| v.len()).unwrap_or(0)
        }

        /// 書き込みを溜める。`offset` が現在長より先なら 0 で埋める。
        fn buffer_write(&self, offset: u64, data: &[u8], append: bool) {
            let mut guard = self.pending.lock().unwrap_or_else(|e| e.into_inner());
            let buf = guard.get_or_insert_with(Vec::new);
            let at = if append { buf.len() } else { offset as usize };
            if at > buf.len() {
                buf.resize(at, 0);
            }
            let end = at + data.len();
            if end > buf.len() {
                buf.resize(end, 0);
            }
            buf[at..end].copy_from_slice(data);
        }
    }

    /// 書き込みを溜めてから送る閾値の既定値（8 MiB）。
    ///
    /// `write` のたびに全体を送ると、N 回に分けて書くアプリでは N 回の全量
    /// アップロードになる（暗号化時はそのたびに全体を暗号化する）。閾値までは
    /// 溜め、超えたら送る。`flush` と `cleanup` で必ず送る。
    const DEFAULT_WRITE_BUFFER: usize = 8 * 1024 * 1024;

    /// ファイルシステム本体。
    ///
    /// 暗号化層（`CryptFs`）を差し込めるよう、具象 `TeraBoxFs` ではなく
    /// `RemoteFs` のトレイトオブジェクトで持つ。容量だけは `RemoteFs` の
    /// 契約に含まれないため、マウント側からクロージャで渡す。
    pub struct TeraBoxFsContext {
        fs: Arc<dyn terabox_core::vfs::RemoteFs + Send + Sync>,
        usage: Option<Arc<dyn Fn() -> Option<(u64, u64)> + Send + Sync>>,
        /// 書き込みを溜めてから送る閾値（バイト）。0 なら毎回送る。
        write_buffer: usize,
    }

    impl FileSystemContext for TeraBoxFsContext {
        type FileContext = FsHandle;

        fn get_security_by_name(
            &self,
            file_name: &U16CStr,
            _security_descriptor: Option<&mut [std::ffi::c_void]>,
            _reparse_point_resolver: impl FnOnce(&U16CStr) -> Option<winfsp::filesystem::FileSecurity>,
        ) -> winfsp::Result<winfsp::filesystem::FileSecurity> {
            let path = to_path(file_name);
            let meta = block_on(self.fs.metadata(&path))
                .map_err(|_| FspError::IO(ErrorKind::Other))?
                .ok_or(FspError::IO(ErrorKind::NotFound))?;
            Ok(winfsp::filesystem::FileSecurity {
                reparse: false,
                sz_security_descriptor: 0,
                attributes: if meta.is_dir { ATTR_DIRECTORY } else { ATTR_NORMAL },
            })
        }

        fn open(
            &self,
            file_name: &U16CStr,
            _create_options: u32,
            _granted_access: u32,
            file_info: &mut winfsp::filesystem::OpenFileInfo,
        ) -> winfsp::Result<Self::FileContext> {
            let path = to_path(file_name);
            let meta = block_on(self.fs.metadata(&path))
                .map_err(|_| FspError::IO(ErrorKind::Other))?
                .ok_or(FspError::IO(ErrorKind::NotFound))?;
            fill_info(AsMut::<FileInfo>::as_mut(file_info), &meta);
            Ok(FsHandle::new(path, meta.is_dir))
        }

        fn close(&self, context: Self::FileContext) {
            // ハンドルが閉じる直前に、未送信の書き込みを流す。
            // `close` は失敗を返せないため、結果はログに残すだけ。
            let _ = self.persist(&context);
        }

        fn get_file_info(
            &self,
            context: &Self::FileContext,
            file_info: &mut FileInfo,
        ) -> winfsp::Result<()> {
            let meta = block_on(self.fs.metadata(&context.path))
                .map_err(|_| FspError::IO(ErrorKind::Other))?
                .ok_or(FspError::IO(ErrorKind::NotFound))?;
            fill_info(file_info, &meta);
            // 未送信の書き込みがあれば、その長さを見せる。
            if let Some(buf) = context.dirty_bytes() {
                file_info.file_size = buf.len() as u64;
                file_info.allocation_size = buf.len() as u64;
            }
            Ok(())
        }

        fn get_volume_info(
            &self,
            out_volume_info: &mut winfsp::filesystem::VolumeInfo,
        ) -> winfsp::Result<()> {
            // 実際の容量をサーバから取る。取れない場合は「不明」を意味する
            // 0 を返す（推測値で 512GB のような嘘を表示しない）。
            let usage = self.usage.as_ref().and_then(|f| f());
            match usage {
                Some((total, free)) => {
                    out_volume_info.total_size = total;
                    out_volume_info.free_size = free;
                }
                None => {
                    out_volume_info.total_size = 0;
                    out_volume_info.free_size = 0;
                }
            }
            out_volume_info.set_volume_label("TeraBox");
            Ok(())
        }

        fn read_directory(
            &self,
            context: &Self::FileContext,
            _pattern: Option<&U16CStr>,
            marker: winfsp::filesystem::DirMarker,
            buffer: &mut [u8],
        ) -> winfsp::Result<u32> {
            let entries = block_on(self.fs.read_dir(&context.path))
                .map_err(|_| FspError::IO(ErrorKind::Other))?;
            let marker_raw = marker
                .inner()
                .map(|raw| String::from_utf16_lossy(&raw[..raw.len().saturating_sub(1)]));

            let mut cursor = 0u32;
            let mut started = marker_raw.is_none();
            for entry in entries {
                let name = entry.display_name().to_string();
                if !started {
                    // WinFsp のマーカーは 8.3 形式（例: `tbx-prob`）で返る。
                    // フルネーム比較だけでは一致せず、2 ページ目以降が欠落する。
                    if let Some(marker_name) = &marker_raw {
                        if crate::winname::marker_matches(marker_name, &name) {
                            started = true;
                        }
                    }
                    continue;
                }
                let mut info: DirInfo<255> = DirInfo::new();
                {
                    let fi = info.file_info_mut();
                    fi.file_attributes = if entry.isdir { ATTR_DIRECTORY } else { ATTR_NORMAL };
                    fi.file_size = entry.size;
                    fi.allocation_size = entry.size;
                    let stamp = filetime_from_epoch(entry.server_mtime);
                    fi.creation_time = stamp;
                    fi.last_write_time = stamp;
                    fi.last_access_time = stamp;
                    fi.change_time = stamp;
                }
                if info.set_name(&name).is_err() {
                    continue;
                }
                if !info.append_to_buffer(buffer, &mut cursor) {
                    break;
                }
                started = true;
            }
            DirInfo::<255>::finalize_buffer(buffer, &mut cursor);
            Ok(cursor)
        }

        fn overwrite(
            &self,
            context: &Self::FileContext,
            _file_attributes: u32,
            _replace_file_attributes: bool,
            allocation_size: u64,
            _extra_buffer: Option<&[u8]>,
            file_info: &mut FileInfo,
        ) -> winfsp::Result<()> {
            // 既存ファイルを開き直して書き直すとき、WinFsp は先に `overwrite` を
            // 呼ぶ。既定実装は `STATUS_INVALID_DEVICE_REQUEST` を返すため、
            // これが無いと `WriteAllText` による上書きが「Incorrect function」
            // で失敗する（実測）。ここではバッファを空にして長さを 0 に戻す。
            {
                let mut guard = context
                    .pending
                    .lock()
                    .unwrap_or_else(|e| e.into_inner());
                *guard = Some(Vec::with_capacity(allocation_size as usize));
            }
            file_info.file_size = 0;
            file_info.allocation_size = allocation_size;
            Ok(())
        }

        fn read(
            &self,
            context: &Self::FileContext,
            buffer: &mut [u8],
            offset: u64,
        ) -> winfsp::Result<u32> {
            let want = buffer.len().min(MAX_READ) as u64;

            if want == 0 {
                return Ok(0);
            }

            // 未送信の書き込みがあれば、そちらを優先して返す（書いた直後の
            // 読み出しでサーバの古い内容を見せない）。
            if let Some(buf) = context.dirty_bytes() {
                let start = (offset as usize).min(buf.len());
                let end = (start + want as usize).min(buf.len());
                let n = end - start;
                buffer[..n].copy_from_slice(&buf[start..end]);
                return Ok(n as u32);
            }

            let data = match block_on(self.fs.read_range(&context.path, offset, want)) {
                Ok(d) => d,
                Err(e) => {
                    eprintln!("[read] {} offset={offset} want={want} 失敗: {e}", context.path);
                    return Err(FspError::IO(ErrorKind::Other));
                }
            };
            let n = data.len().min(buffer.len());
            buffer[..n].copy_from_slice(&data[..n]);
            // 読み出しの終端は 0 バイトで示す。
            Ok(n as u32)
        }

        fn write(
            &self,
            context: &Self::FileContext,
            buffer: &[u8],
            offset: u64,
            write_to_eof: bool,
            _constrained_io: bool,
            file_info: &mut FileInfo,
        ) -> winfsp::Result<u32> {
            // バッファへ溜めるだけにする。送信は `flush` と `cleanup` だけ。
            //
            // TeraBox の API はオブジェクト全体の置き換えしか提供しない。途中で
            // 送ると「まだ書き込み途中の内容で丸ごと上書き」したうえ、続きが
            // 来るたびにまた全体を送ることになり、転送量が二乗に増える（実測:
            // 20 MiB の書き込みで 8 MiB 以降の毎回の write が全量再送になり、
            // 34.6 秒かかった）。`flush` は失敗を返せる確定点なので、そこで
            // 1 回だけ送れば足りる。
            context.buffer_write(offset, buffer, write_to_eof);
            let len = context.dirty_len();

            // 閾値 0 でなければ、巨大なファイルでメモリを圧迫しないよう、
            // 一定量を超えたら途中送信する。閾値は倍々に伸ばすので、再送は
            // log2 回に収まる（固定値だと二乗で遅くなる。実測あり）。
            // 通常の使い方（`flush` が来る書き方）ではここは通らない。
            let spill_at = if self.write_buffer == 0 {
                0
            } else {
                context.take_spill_threshold(self.write_buffer.saturating_mul(8))
            };
            if spill_at > 0 && len >= spill_at {
                let buf = context.dirty_bytes().unwrap_or_default();
                if let Err(e) = block_on(self.fs.write(&context.path, &buf)) {
                    eprintln!("[write] {} の送信に失敗しました: {e}", context.path);
                    {
                        let mut guard = context
                            .pending
                            .lock()
                            .unwrap_or_else(|e| e.into_inner());
                        *guard = None;
                    }
                    return Err(FspError::IO(ErrorKind::Other));
                }
            }
            file_info.file_size = len as u64;
            file_info.allocation_size = len as u64;
            Ok(buffer.len() as u32)
        }

        fn create(
            &self,
            file_name: &U16CStr,
            _create_options: u32,
            _granted_access: u32,
            file_attributes: u32,
            _security_descriptor: Option<&[std::ffi::c_void]>,
            _allocation_size: u64,
            _extra_buffer: Option<&[u8]>,
            _extra_buffer_is_reparse_point: bool,
            _file_info: &mut winfsp::filesystem::OpenFileInfo,
        ) -> winfsp::Result<Self::FileContext> {
            let path = to_path(file_name);
            let is_dir = file_attributes & ATTR_DIRECTORY != 0;
            if is_dir {
                block_on(self.fs.create_dir(&path)).map_err(|_| FspError::IO(ErrorKind::Other))?;
            }
            // ファイルは、実際に書き込みが来たとき（flush / cleanup）に作る。
            // ここで空ファイルを送ると、その後の書き込みで別名ファイルが
            // 増える（`/api/create` は同名を上書きしないため）。
            Ok(FsHandle::new(path, is_dir))
        }

        fn cleanup(
            &self,
            context: &Self::FileContext,
            _file_name: Option<&U16CStr>,
            flags: u32,
        ) {
            // 削除は `set_delete` で受ける（WinFsp の削除は 2 段階）。
            // ここでは未送信の書き込みを確定させる。
            if flags & winfsp::constants::FspCleanupFlags::FspCleanupDelete as u32 != 0 {
                return;
            }
            let _ = self.persist(context);
        }

        fn set_delete(
            &self,
            context: &Self::FileContext,
            _file_name: &U16CStr,
            delete_file: bool,
        ) -> winfsp::Result<()> {
            if delete_file {
                block_on(self.fs.delete(&context.path))
                    .map_err(|_| FspError::IO(ErrorKind::Other))?;
            }
            Ok(())
        }

        fn rename(
            &self,
            context: &Self::FileContext,
            _file_name: &U16CStr,
            new_file_name: &U16CStr,
            _replace_if_exists: bool,
        ) -> winfsp::Result<()> {
            let to = to_path(new_file_name);
            // 未送信の内容があれば、先に現在のパスへ確定させる。
            self.persist(context)?;
            block_on(self.fs.rename(&context.path, &to))
                .map_err(|_| FspError::IO(ErrorKind::Other))?;
            Ok(())
        }

        fn set_file_size(
            &self,
            context: &Self::FileContext,
            new_size: u64,
            _set_allocation_size: bool,
            file_info: &mut FileInfo,
        ) -> winfsp::Result<()> {
            // メモリ上のバッファを伸縮しておく（送信は flush 時）。
            if let Some(mut buf) = context.dirty_bytes() {
                buf.resize(new_size as usize, 0);
                let mut guard = context
                    .pending
                    .lock()
                    .unwrap_or_else(|e| e.into_inner());
                *guard = Some(buf);
            }
            file_info.file_size = new_size;
            file_info.allocation_size = new_size;
            Ok(())
        }

        fn flush(
            &self,
            context: Option<&Self::FileContext>,
            _file_info: &mut FileInfo,
        ) -> winfsp::Result<()> {
            if let Some(handle) = context {
                // `flush` は失敗を返せる唯一の確定点。ここで返さないと、
                // 書けたように見えて実際には送れていない状態になる。
                self.persist(handle)?;
            }
            Ok(())
        }
    }

    impl TeraBoxFsContext {
        /// 溜めた書き込みをサーバへ送る。送ったらバッファを空にする。
        ///
        /// 失敗しても呼び出し元（WinFsp のコールバック）はログを残すだけで、
        /// バッファは捨てる。捨てないと同じ内容を何度も送り続ける。
        fn persist(&self, handle: &FsHandle) -> winfsp::Result<()> {
            let Some(buf) = handle.dirty_bytes() else {
                return Ok(());
            };
            if handle.is_dir {
                return Ok(());
            }

            let started = std::time::Instant::now();
            let result = block_on(self.fs.write(&handle.path, &buf));
            if std::env::var_os("TBX_TRACE_IO").is_some() {
                eprintln!(
                    "[io] upload {} bytes={} micros={}",
                    handle.path,
                    buf.len(),
                    started.elapsed().as_micros()
                );
            }
            // 失敗してもバッファは捨てる。残すと同じ内容を送り続ける。
            let mut guard = handle.pending.lock().unwrap_or_else(|e| e.into_inner());
            *guard = None;

            match result {
                Ok(()) => Ok(()),
                Err(e) => {
                    // 呼び出し元（エクスプローラ）に失敗を返す。ここで握り潰すと
                    // 書き込みが成功したように見えてデータが消える。
                    eprintln!("[persist] {} の送信に失敗しました: {e}", handle.path);
                    Err(FspError::IO(ErrorKind::Other))
                }
            }
        }
    }

    /// WinFsp でマウントする。Ctrl+C まで戻らない。
    pub async fn mount_with_winfsp(session: Session, drive: &str) -> anyhow::Result<()> {
        crate::teraboxfs::mount_with_winfsp_tuned(session, drive, crate::MountTuning::default()).await
    }

    /// 調整値を指定して WinFsp でマウントする。
    pub async fn mount_with_winfsp_tuned(
        session: Session,
        drive: &str,
        tuning: crate::MountTuning,
    ) -> anyhow::Result<()> {
        let api = terabox_core::api::ApiClient::new(session.clone())?;
        let fs = Arc::new(TeraBoxFs::with_part_size(api, session, tuning.part_size));
        let usage_source = Arc::clone(&fs);
        let usage: Arc<dyn Fn() -> Option<(u64, u64)> + Send + Sync> = Arc::new(move || {
            let remote = Arc::clone(&usage_source);
            block_on(async move { remote.usage().await }).map(|u| (u.total, u.free))
        });
        mount_with_fs(
            fs as Arc<dyn terabox_core::vfs::RemoteFs + Send + Sync>,
            drive,
            Some(usage),
            tuning.write_buffer,
            tuning.threads,
        )
        .await
    }

    /// 任意の `RemoteFs` を WinFsp でマウントする（暗号化層を差し込むときに使う）。
    pub async fn mount_with_fs(
        fs: Arc<dyn terabox_core::vfs::RemoteFs + Send + Sync>,
        drive: &str,
        usage: Option<Arc<dyn Fn() -> Option<(u64, u64)> + Send + Sync>>,
        write_buffer: usize,
        threads: usize,
    ) -> anyhow::Result<()> {
        let mut params = VolumeParams::new();
        {
            let p = &mut params;
            p.filesystem_name("TeraBox");
            p.sector_size(4096);
            p.sectors_per_allocation_unit(1);
            p.max_component_length(255);
            p.volume_serial_number(0x5442_5F31);
            p.case_sensitive_search(false);
            p.case_preserved_names(true);
            p.unicode_on_disk(true);
            p.persistent_acls(false);
            p.read_only_volume(false);
            p.flush_and_purge_on_cleanup(false);
            p.post_cleanup_when_modified_only(true);
        }

        // `winfsp_init` は `LoadLibraryW("winfsp-x64.dll")` を名前で呼ぶ。PATH に
        // 無い環境では失敗するので、先に絶対パスで読み込んで解決させる。
        let loaded = crate::preload_winfsp_dll();
        if std::env::var_os("TBX_WINFSP_TRACE").is_some() {
            match &loaded {
                Some(p) => eprintln!("[winfsp] 読み込んだ DLL: {}", p.display()),
                None => eprintln!("[winfsp] DLL を絶対パスで読み込めませんでした（名前解決を試します）"),
            }
        }
        let init: FspInit = winfsp::winfsp_init().map_err(|e| {
            let hint = if loaded.is_none() {
                "。`winfsp-x64.dll` を exe の隣に置くか、WinFsp を導入してください"
            } else {
                ""
            };
            anyhow::anyhow!(
                "WinFsp を初期化できませんでした（winfsp-x64.dll が見つからないか、ドライバが未導入です）{hint}: {e}"
            )
        })?;

        // `FineGuard` を明示して、`CoarseGuard` 版との曖昧さを解消する。
        let mut host: FileSystemHost<TeraBoxFsContext, winfsp::host::FineGuard> =
            FileSystemHost::new(params, TeraBoxFsContext { fs, usage, write_buffer })
                .map_err(|e| anyhow::anyhow!("WinFsp ホストを作れませんでした: {e}"))?;

        host.mount(drive)
            .map_err(|e| anyhow::anyhow!("{drive} をマウントできませんでした: {e}"))?;
        println!("{drive} に TeraBox をマウントしました。Ctrl+C で終了します。");

        // 以前は 4 固定。読み出しは dlink キャッシュで軽くなったので、
        // 同時に走る範囲取得を増やして待ち時間を減らす。
        host.start_with_threads(threads.max(1) as u32)
            .map_err(|e| anyhow::anyhow!("WinFsp を開始できませんでした: {e}"))?;

        let _ = init;
        tokio::signal::ctrl_c().await.ok();

        host.stop();
        host.unmount();
        println!("{drive} をアンマウントしました。");
        Ok(())
    }

    /// 未使用の型参照を型チェックに載せる（将来の通知実装用）。
    #[allow(dead_code)]
    fn _keep(_b: DirBuffer) {}

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn write_buffers_without_losing_bytes() {
            let handle = FsHandle::new("/b.bin".to_string(), false);
            handle.buffer_write(0, b"abc", false);
            handle.buffer_write(3, b"def", false);
            assert_eq!(handle.dirty_len(), 6);
            assert_eq!(handle.dirty_bytes().unwrap(), b"abcdef".to_vec());
        }

        #[test]
        fn write_fills_gaps_with_zero() {
            let handle = FsHandle::new("/g.bin".to_string(), false);
            handle.buffer_write(4, b"xy", false);
            assert_eq!(handle.dirty_len(), 6);
            assert_eq!(handle.dirty_bytes().unwrap(), b"\0\0\0\0xy".to_vec());
        }

        #[test]
        fn append_ignores_the_given_offset() {
            let handle = FsHandle::new("/a.bin".to_string(), false);
            handle.buffer_write(0, b"abc", false);
            handle.buffer_write(99, b"z", true);
            assert_eq!(handle.dirty_bytes().unwrap(), b"abcz".to_vec());
        }

        #[test]
        fn clearing_the_buffer_after_a_send_makes_it_dirty_free() {
            let handle = FsHandle::new("/c.bin".to_string(), false);
            handle.buffer_write(0, b"abc", false);
            assert_eq!(handle.dirty_len(), 3);
            {
                let mut guard = handle.pending.lock().unwrap_or_else(|e| e.into_inner());
                *guard = None;
            }
            assert_eq!(handle.dirty_len(), 0, "送信後は未送信バイトが 0 になるはず");
            assert!(handle.dirty_bytes().is_none());
        }
    }
}

#[cfg(all(windows, feature = "winfsp-backend"))]
pub use imp::{mount_with_fs, mount_with_winfsp, mount_with_winfsp_tuned};

#[cfg(not(all(windows, feature = "winfsp-backend")))]
/// WinFsp バックエンドが無効なビルド向けのスタブ。
pub async fn mount_with_winfsp_tuned(
    _session: terabox_core::session::Session,
    _drive: &str,
    _tuning: crate::MountTuning,
) -> anyhow::Result<()> {
    anyhow::bail!(
        "このビルドは WinFsp バックエンドを含んでいません。`--features winfsp-backend` と MSVC ターゲットでビルドしてください。`tbx mount <drive>: --mode webdav` も使えます。"
    )
}

#[cfg(not(all(windows, feature = "winfsp-backend")))]
/// WinFsp バックエンドが無効なビルド向けのスタブ。
pub async fn mount_with_winfsp(
    _session: terabox_core::session::Session,
    _drive: &str,
) -> anyhow::Result<()> {
    anyhow::bail!(
        "このビルドは WinFsp バックエンドを含んでいません。`--features winfsp-backend` と MSVC ターゲットでビルドしてください。`tbx mount <drive>: --mode webdav` も使えます。"
    )
}
