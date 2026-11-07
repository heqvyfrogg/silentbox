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

#[cfg(all(unix, feature = "fuse-backend"))]
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
