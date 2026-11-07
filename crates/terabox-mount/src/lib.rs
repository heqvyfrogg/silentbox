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

/// WinFsp ランタイムが導入されているか。
///
/// 既定のインストール先 `%ProgramFiles(x86)%\WinFsp\bin\winfsp-x64.dll` を見る。
pub fn winfsp_available() -> bool {
    #[cfg(windows)]
    {
        use std::path::PathBuf;
        let mut candidates: Vec<PathBuf> = Vec::new();
        if let Ok(pf) = std::env::var("ProgramFiles(x86)") {
            candidates.push(PathBuf::from(pf).join(r"WinFsp\bin\winfsp-x64.dll"));
        }
        if let Ok(pf) = std::env::var("ProgramFiles") {
            candidates.push(PathBuf::from(pf).join(r"WinFsp\bin\winfsp-x64.dll"));
        }
        candidates.iter().any(|p| p.exists())
    }
    #[cfg(not(windows))]
    {
        false
    }
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

/// WinFsp バックエンドでマウントする。
pub async fn mount_winfsp(session: Session, drive: &str) -> anyhow::Result<()> {
    if !winfsp_available() {
        anyhow::bail!(
            "WinFsp がインストールされていません。`tbx mount {drive} --mode webdav` を使うか、https://winfsp.dev/ から WinFsp を導入してください。"
        );
    }

    #[cfg(all(windows, feature = "winfsp-backend"))]
    {
        teraboxfs::mount_with_winfsp(session, drive).await
    }
    #[cfg(not(all(windows, feature = "winfsp-backend")))]
    {
        let _ = session;
        let _ = drive;
        anyhow::bail!(
            "このビルドは WinFsp バックエンドを含んでいません。`--features winfsp-backend` と MSVC ターゲットでビルドしてください。`tbx mount {drive} --mode webdav` も使えます。"
        )
    }
}

/// FUSE バックエンドでマウントする。`mountpoint` はディレクトリ。
pub async fn mount_fuse(session: Session, mountpoint: &str) -> anyhow::Result<()> {
    #[cfg(all(unix, feature = "fuse-backend"))]
    {
        let api = terabox_core::api::ApiClient::new(session.clone())?;
        let fs = Arc::new(remote::TeraBoxFs::new(api, session));
        // 容量は `TeraBoxFs` だけが持つ。`RemoteFs` を広げずに済むよう、
        // 参照を持たせたクロージャで渡す。
        let usage_source = Arc::clone(&fs);
        let usage: Arc<dyn Fn() -> Option<(u64, u64)> + Send + Sync> = Arc::new(move || {
            let remote_api = Arc::clone(&usage_source);
            crate::runtime::block_on(async move { remote_api.usage().await })
                .map(|u| (u.total, u.free))
        });
        fuse_fs::mount(fs, std::path::Path::new(mountpoint), Some(usage)).await
    }
    #[cfg(not(all(unix, feature = "fuse-backend")))]
    {
        let _ = session;
        let _ = mountpoint;
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
/// WinFsp のホストは具象 `TeraBoxFs` を要求するため、現時点では暗号化層を
/// 差し込めない。Windows で暗号化を使う場合は `--mode webdav`（または
/// `--mode fuse` を含むビルド）を使う。**黙って平文でマウントしない**よう、
/// 明示的に失敗させる。
#[cfg(feature = "crypt")]
pub async fn mount_winfsp_encrypted(
    _session: Session,
    _drive: &str,
    _cipher: terabox_core::crypt::Cipher,
) -> anyhow::Result<()> {
    anyhow::bail!(
        "暗号化と WinFsp の組み合わせは未対応です。`--mode webdav` で暗号化マウントしてください（WebDAV でも中身は暗号化されます）。"
    )
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
