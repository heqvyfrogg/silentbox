//! バイナリ側で WinFsp の遅延ロードを設定する。
//!
//! `rustc-link-arg` はそのクレート自身のリンクにしか効かず、rlib の依存
//! （`terabox-mount`）から最終バイナリへは伝播しない。実測: CI の Windows で
//! `tbx.exe --version` が 0xC0000135（DLL が見つからない）で落ちた。
//! WinFsp が未導入の環境でも起動できるよう、遅延ロードをここで発行する。
//!
//! 対象は Windows MSVC + `winfsp-backend` のときだけ。それ以外では何もしない。

fn main() {
    let os = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    let env = std::env::var("CARGO_CFG_TARGET_ENV").unwrap_or_default();
    let feature_on = std::env::var("CARGO_FEATURE_WINFSP_BACKEND").is_ok();

    if os != "windows" || !feature_on {
        return;
    }

    if env == "msvc" {
        println!("cargo:rustc-link-lib=dylib=delayimp");
        println!("cargo:rustc-link-arg=/DELAYLOAD:winfsp-x64.dll");
    } else {
        // GNU では `winfsp-sys` 自体がビルドできないため通常ここへ来ない。
        println!("cargo:warning=WinFsp バックエンドは MSVC ターゲットでのみビルドできます");
    }
}
