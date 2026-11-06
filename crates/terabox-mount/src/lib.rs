//! Windows 向けのマウント実装。
//!
//! 2 つのバックエンドを持つ:
//! - **WinFsp**: エクスプローラの仮想ドライブ。要 WinFsp ランタイム（MSI）。
//!   ビルドは `--features winfsp-backend` + MSVC ターゲットが必須（実測）。
//! - **WebDAV**: インストーラ不要。`net use` でドライブレターを割り当てる。
//!
//! どちらも共通の `RemoteFs` を経由するため、上位の CLI からは同じ操作で扱える。
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

/// WebDAV バックエンドでマウントする。
pub async fn mount_webdav(session: Session, drive: &str, port: u16) -> anyhow::Result<()> {
    webdav::serve_and_connect(session, drive, port).await
}
