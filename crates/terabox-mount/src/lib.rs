//! TeraBox のマウント実装。
//!
//! 3 つのバックエンドを持つ:
//! - **WinFsp**（Windows）: エクスプローラの仮想ドライブ。要 WinFsp ランタイム。
//!   ビルドは `--features winfsp-backend` + MSVC ターゲットが必須（実測）。
//! - **FUSE**（Linux / macOS）: 純 Rust の `fuser` で実装。libfuse 不要。
//!   ビルドは `--features fuse-backend`。実行時に `/dev/fuse` が要る。
//! - **WebDAV**: 全 OS 共通。Windows は `net use` でドライブレターを割り当て、
//!   Unix は接続コマンドを実行せず案内だけ出す（`davfs2` は root が要るため）。
//!
//! どれも共通の `RemoteFs` を経由するため、上位の CLI からは同じ操作で扱える。
//!
//! ## 実測した制約
//! - `windows-gnu` ターゲットでは `winfsp-sys` のビルドが失敗する
//!   （`unsupported triple x86_64-pc-windows-gnu`）。MSVC ターゲットが必要。
//! - WinFsp は遅延リンク必須。`build.rs` で
//!   `winfsp::build::winfsp_link_delayload()` を呼ぶ。
//! - 実行時に `winfsp-x64.dll` が見つからないと 1285 で終了する。
//!   WinFsp の `bin` を PATH に足すか、実行ファイルと同じ場所に DLL を置く。

pub mod remote;
pub mod runtime;
pub mod teraboxfs;
pub mod webdav;
pub mod winname;

#[cfg(all(unix, feature = "fuse-backend"))]
pub mod fuse_fs;

#[cfg(feature = "crypt")]
pub mod cryptfs;

#[cfg(any(all(unix, feature = "fuse-backend"), feature = "crypt"))]
use std::sync::Arc;

use terabox_core::Session;

/// WinFsp ランタイムを読み込めるか。
///
/// `winfsp_init` は「exe の隣の `winfsp-x64.dll`」→「レジストリ
/// `HKLM\SOFTWARE\WOW6432Node\WinFsp`（または `HKLM\SOFTWARE\WinFsp`）の
/// `InstallDir\bin\winfsp-x64.dll`」の順に探し、どちらも失敗すると初期化
/// できない。以前は `%ProgramFiles(x86)%` の固定パスだけを見ていたため、
/// DLL があっても `InstallDir` が未登録の環境で「利用可能」と誤報告し、
/// 実際のマウントは失敗した（実測）。
pub fn winfsp_available() -> bool {
    #[cfg(windows)]
    {
        winfsp_dll_candidates().iter().any(|p| p.exists())
    }
    #[cfg(not(windows))]
    {
        false
    }
}

/// `winfsp_init` が実際に試す DLL の位置を列挙する。
#[cfg(windows)]
pub fn winfsp_dll_candidates() -> Vec<std::path::PathBuf> {
    use std::path::PathBuf;

    let mut candidates: Vec<PathBuf> = Vec::new();

    // 1) exe の隣（`load_local_winfsp` が最初に試す場所）。
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            candidates.push(dir.join("winfsp-x64.dll"));
        }
    }

    // 2) レジストリの InstallDir（`get_system_winfsp` が読む場所）。
    //    32-bit インストーラは WOW6432Node 側、ARM64 は直下に書く。
    for subkey in [
        r"SOFTWARE\WOW6432Node\WinFsp",
        r"SOFTWARE\WinFsp",
    ] {
        if let Some(dir) = read_install_dir(subkey) {
            candidates.push(PathBuf::from(dir).join("bin").join("winfsp-x64.dll"));
        }
    }

    // 3) 既定のインストール先（InstallDir が未登録でも実体がある場合の保険）。
    for var in ["ProgramFiles(x86)", "ProgramFiles"] {
        if let Ok(pf) = std::env::var(var) {
            candidates.push(PathBuf::from(pf).join(r"WinFsp\bin\winfsp-x64.dll"));
        }
    }

    candidates
}

/// 解決済みの DLL を絶対パスで読み込む。
///
/// `winfsp_init` は内部で `LoadLibraryW("winfsp-x64.dll")` を呼ぶ。これは
/// 名前だけの読み込みなので、PATH やアプリのディレクトリに無いと失敗する。
/// 先に絶対パスで読み込んでおくと、その後の名前読み込みは読み込み済みの
/// モジュールを返すため、遅延ロードが解決できる。
///
/// 戻り値は「読み込めた DLL のパス」。プロセス終了までハンドルを保持する。
#[cfg(all(windows, feature = "winfsp-backend"))]
pub fn preload_winfsp_dll() -> Option<std::path::PathBuf> {
    use std::sync::OnceLock;

    static LOADED: OnceLock<Option<std::path::PathBuf>> = OnceLock::new();
    LOADED
        .get_or_init(|| {
            for candidate in winfsp_dll_candidates() {
                if !candidate.exists() {
                    continue;
                }
                // ライブラリはプロセス終了まで保持する（`Library` を落とすと
                // 読み込みが解除され、遅延ロードがまた失敗する）。
                match unsafe { libloading::Library::new(&candidate) } {
                    Ok(lib) => {
                        std::mem::forget(lib);
                        return Some(candidate);
                    }
                    Err(_) => continue,
                }
            }
            None
        })
        .clone()
}

/// `HKLM` の WinFsp サブキーから `InstallDir` を読む。無ければ `None`。
#[cfg(windows)]
fn read_install_dir(subkey: &str) -> Option<String> {
    let output = std::process::Command::new("reg")
        .args(["query", &format!(r"HKLM\{subkey}"), "/v", "InstallDir"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&output.stdout);
    // 出力例: "    InstallDir    REG_SZ    C:\Program Files (x86)\WinFsp"
    text.lines()
        .find(|l| l.contains("InstallDir"))
        .and_then(|l| l.split("REG_SZ").nth(1))
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
}

/// FUSE が使えるか。ビルドに含まれていて、`/dev/fuse` が開けることを確かめる。
pub fn fuse_available() -> bool {
    #[cfg(all(unix, feature = "fuse-backend"))]
    {
        // `/dev/fuse` の存在だけでは不十分だが、無ければ確実に失敗する。
        // 権限は実際のマウント時に判定される（推測で「使える」と言わない）。
        std::path::Path::new("/dev/fuse").exists()
    }
    #[cfg(not(all(unix, feature = "fuse-backend")))]
    {
        false
    }
}

/// マウントの調整値。既定値を持ち、設定から差し替えられる。
#[derive(Debug, Clone, Copy)]
pub struct MountTuning {
    /// 書き込みを溜めてから送る閾値（バイト）。0 は毎回送る。
    pub write_buffer: usize,
    /// 同時実行スレッド数。
    pub threads: usize,
    /// アップロードの分割サイズ（バイト）。
    pub part_size: u64,
}

impl Default for MountTuning {
    fn default() -> Self {
        Self {
            write_buffer: 8 * 1024 * 1024,
            threads: 8,
            part_size: 4 * 1024 * 1024,
        }
    }
}

/// WinFsp バックエンドでマウントする。
pub async fn mount_winfsp(session: Session, drive: &str) -> anyhow::Result<()> {
    mount_winfsp_tuned(session, drive, MountTuning::default()).await
}

/// 調整値を指定して WinFsp でマウントする。
pub async fn mount_winfsp_tuned(
    session: Session,
    drive: &str,
    tuning: MountTuning,
) -> anyhow::Result<()> {
    if !winfsp_available() {
        anyhow::bail!(
            "WinFsp がインストールされていません。`tbx mount {drive} --mode webdav` を使うか、https://winfsp.dev/ から WinFsp を導入してください。"
        );
    }

    #[cfg(all(windows, feature = "winfsp-backend"))]
    {
        let _ = &tuning;
        teraboxfs::mount_with_winfsp_tuned(session, drive, tuning).await
    }
    #[cfg(not(all(windows, feature = "winfsp-backend")))]
    {
        let _ = (session, drive, tuning);
        anyhow::bail!(
            "このビルドは WinFsp バックエンドを含んでいません。`--features winfsp-backend` と MSVC ターゲットでビルドしてください。`tbx mount {drive} --mode webdav` も使えます。"
        )
    }
}

/// FUSE バックエンドでマウントする。`mountpoint` はディレクトリ。
pub async fn mount_fuse(session: Session, mountpoint: &str) -> anyhow::Result<()> {
    mount_fuse_tuned(session, mountpoint, MountTuning::default()).await
}

/// 調整値を指定して FUSE でマウントする。
pub async fn mount_fuse_tuned(
    session: Session,
    mountpoint: &str,
    tuning: MountTuning,
) -> anyhow::Result<()> {
    #[cfg(all(unix, feature = "fuse-backend"))]
    {
        let api = terabox_core::api::ApiClient::new(session.clone())?;
        let fs = Arc::new(remote::TeraBoxFs::with_part_size(
            api,
            session,
            tuning.part_size,
        ));
        // 容量は `TeraBoxFs` だけが持つ。`RemoteFs` を広げずに済むよう、
        // 参照を持たせたクロージャで渡す。
        let usage_source = Arc::clone(&fs);
        let usage: Arc<dyn Fn() -> Option<(u64, u64)> + Send + Sync> = Arc::new(move || {
            let remote_api = Arc::clone(&usage_source);
            crate::runtime::block_on(async move { remote_api.usage().await })
                .map(|u| (u.total, u.free))
        });
        fuse_fs::mount(
            fs,
            std::path::Path::new(mountpoint),
            Some(usage),
            tuning.threads,
        )
        .await
    }
    #[cfg(not(all(unix, feature = "fuse-backend")))]
    {
        let _ = (session, mountpoint, tuning);
        anyhow::bail!(
            "このビルドは FUSE バックエンドを含んでいません。`--features fuse-backend` でビルドしてください。`tbx mount {mountpoint} --mode webdav` も使えます。"
        )
    }
}

/// WebDAV バックエンドでマウントする。
pub async fn mount_webdav(session: Session, drive: &str, port: u16) -> anyhow::Result<()> {
    webdav::serve_and_connect(session, drive, port).await
}

// ---- 暗号化（rclone crypt 互換）。`crypt` feature のときだけ存在する ----

/// 暗号化の可否を判定する材料。秘密（パスワード）はここに含めない。
#[cfg(feature = "crypt")]
pub struct CryptSetup {
    /// 復号に使う暗号器。
    pub cipher: terabox_core::crypt::Cipher,
}

#[cfg(feature = "crypt")]
impl std::fmt::Debug for CryptSetup {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CryptSetup").finish_non_exhaustive()
    }
}

/// 暗号化された `RemoteFs` を作る。
#[cfg(feature = "crypt")]
pub fn encrypt_fs(
    session: Session,
    cipher: terabox_core::crypt::Cipher,
) -> anyhow::Result<Arc<dyn terabox_core::vfs::RemoteFs + Send + Sync>> {
    let api = terabox_core::api::ApiClient::new(session.clone())?;
    let inner = Arc::new(remote::TeraBoxFs::new(api, session))
        as Arc<dyn terabox_core::vfs::RemoteFs + Send + Sync>;
    Ok(Arc::new(cryptfs::CryptFs::new(inner, cipher)))
}

/// 暗号化して WinFsp でマウントする。
///
/// WinFsp のホストは `RemoteFs` のトレイトオブジェクトを受けるので、
/// `CryptFs` を挟んだままドライブとして見せられる。容量だけは `RemoteFs` の
/// 契約に無いため、平文側の `TeraBoxFs` から取って渡す。
#[cfg(feature = "crypt")]
pub async fn mount_winfsp_encrypted(
    session: Session,
    drive: &str,
    cipher: terabox_core::crypt::Cipher,
    tuning: MountTuning,
) -> anyhow::Result<()> {
    if !winfsp_available() {
        anyhow::bail!(
            "WinFsp がインストールされていません。`--mode webdav` を使うか、https://winfsp.dev/ から WinFsp を導入してください。"
        );
    }

    #[cfg(all(windows, feature = "winfsp-backend"))]
    {
        let api = terabox_core::api::ApiClient::new(session.clone())?;
        let plain = Arc::new(remote::TeraBoxFs::with_part_size(
            api,
            session,
            tuning.part_size,
        ));

        // 容量は平文側の実装から取る（暗号化で総量は変わらない）。
        let usage_source = Arc::clone(&plain);
        let usage: Arc<dyn Fn() -> Option<(u64, u64)> + Send + Sync> = Arc::new(move || {
            let remote = Arc::clone(&usage_source);
            crate::runtime::block_on(async move { remote.usage().await }).map(|u| (u.total, u.free))
        });

        let encrypted = cryptfs::CryptFs::new(
            plain as Arc<dyn terabox_core::vfs::RemoteFs + Send + Sync>,
            cipher,
        );
        teraboxfs::mount_with_fs(Arc::new(encrypted), drive, Some(usage), tuning.write_buffer, tuning.threads).await
    }
    #[cfg(not(all(windows, feature = "winfsp-backend")))]
    {
        let _ = (session, drive, cipher, tuning);
        anyhow::bail!("このビルドは WinFsp バックエンドを含んでいません。");
    }
}

/// 暗号化して FUSE でマウントする。
#[cfg(feature = "crypt")]
pub async fn mount_fuse_encrypted(
    session: Session,
    mountpoint: &str,
    cipher: terabox_core::crypt::Cipher,
) -> anyhow::Result<()> {
    #[cfg(all(unix, feature = "fuse-backend"))]
    {
        let fs = encrypt_fs(session, cipher)?;
        fuse_fs::mount(fs, std::path::Path::new(mountpoint), None).await
    }
    #[cfg(not(all(unix, feature = "fuse-backend")))]
    {
        let _ = (session, mountpoint, cipher);
        anyhow::bail!("このビルドは FUSE バックエンドを含んでいません。");
    }
}

/// 暗号化して WebDAV でマウントする。
#[cfg(feature = "crypt")]
pub async fn mount_webdav_encrypted(
    session: Session,
    drive: &str,
    port: u16,
    cipher: terabox_core::crypt::Cipher,
) -> anyhow::Result<()> {
    let fs = encrypt_fs(session, cipher)?;
    webdav::serve_fs_and_connect(fs, drive, port).await
}
