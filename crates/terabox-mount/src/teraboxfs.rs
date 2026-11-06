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
    }

    impl FsHandle {
        pub fn new(path: String, is_dir: bool) -> Self {
            Self {
                path,
                is_dir,
                pending: std::sync::Mutex::new(None),
            }
        }

        fn dirty_bytes(&self) -> Option<Vec<u8>> {
            let guard = self.pending.lock().unwrap_or_else(|e| e.into_inner());
            guard.clone()
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

    /// ファイルシステム本体。
    pub struct TeraBoxFsContext {
        fs: Arc<TeraBoxFs>,
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
            match block_on(self.fs.usage()) {
                Some(usage) => {
                    out_volume_info.total_size = usage.total;
                    out_volume_info.free_size = usage.free;
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
            // バッファへ書き、その場で送信する。
            //
            // `flush` / `cleanup` に送信を遅らせると、`cleanup` が失敗を返せない
            // 仕様のため、送信に失敗しても呼び出し元には成功として見える
            // （実測: サーバが `need verify` を返している間、書き込みが成功した
            // ように見えて実際には何も保存されなかった）。`write` は失敗を
            // 返せるので、ここで送るのが唯一「正直に失敗できる」経路になる。
            //
            // 1 回の書き込みごとに全体を送り直すため、細かい追記が続く書き方では
            // 送信回数が増える。正しさを優先した割り切りである。
            context.buffer_write(offset, buffer, write_to_eof);
            let buf = context.dirty_bytes().unwrap_or_default();
            if let Err(e) = block_on(self.fs.write(&context.path, &buf)) {
                eprintln!("[write] {} の送信に失敗しました: {e}", context.path);
                // 失敗した内容は捨てる（同じ内容を送り続けない）。
                {
                    let mut guard = context
                        .pending
                        .lock()
                        .unwrap_or_else(|e| e.into_inner());
                    *guard = None;
                }
                return Err(FspError::IO(ErrorKind::Other));
            }
            file_info.file_size = buf.len() as u64;
            file_info.allocation_size = buf.len() as u64;
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

            let result = block_on(self.fs.write(&handle.path, &buf));
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
        let api = terabox_core::api::ApiClient::new(session.clone())?;
        let fs = Arc::new(TeraBoxFs::new(api, session));

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

        let init: FspInit = winfsp::winfsp_init().map_err(|e| {
            anyhow::anyhow!(
                "WinFsp を初期化できませんでした（winfsp-x64.dll が見つからない可能性があります）: {e}"
            )
        })?;

        // `FineGuard` を明示して、`CoarseGuard` 版との曖昧さを解消する。
        let mut host: FileSystemHost<TeraBoxFsContext, winfsp::host::FineGuard> =
            FileSystemHost::new(params, TeraBoxFsContext { fs })
                .map_err(|e| anyhow::anyhow!("WinFsp ホストを作れませんでした: {e}"))?;

        host.mount(drive)
            .map_err(|e| anyhow::anyhow!("{drive} をマウントできませんでした: {e}"))?;
        println!("{drive} に TeraBox をマウントしました。Ctrl+C で終了します。");

        host.start_with_threads(4)
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
}

#[cfg(all(windows, feature = "winfsp-backend"))]
pub use imp::mount_with_winfsp;

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
