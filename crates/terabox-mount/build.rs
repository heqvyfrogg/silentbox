//! WinFsp 遅延リンク設定（実測で必須）。
//!
//! `winfsp` クレートは `winfsp-x64.dll` を遅延ロードする前提で設計されている。
//! この呼び出しを省くとリンク時に `delayimp` が解決できず、ビルドが通らないか
//! 起動直後に 1285（0x505）で落ちる。
//!
//! 既定ターゲット（`x86_64-pc-windows-gnu`）では `winfsp-sys` 自体が
//! `unsupported triple` で停止するため、ここでは MSVC ターゲットのときだけ
//! フラグを発行し、それ以外は何もしない（GNU ビルドを壊さない）。

fn main() {
    let env = std::env::var("CARGO_CFG_TARGET_ENV").unwrap_or_default();
    let feature_on = std::env::var("CARGO_FEATURE_WINFSP_BACKEND").is_ok();

    if !feature_on {
        return;
    }

    if env == "msvc" {
        println!("cargo:rustc-link-lib=dylib=delayimp");
        println!("cargo:rustc-link-arg=/DELAYLOAD:winfsp-x64.dll");
    } else {
        // GNU ターゲットでは winfsp-sys が build script の時点で失敗するため、
        // ここには到達しない。念のため明示的なメッセージだけ残す。
        println!("cargo:warning=WinFsp バックエンドは MSVC ターゲットでのみビルドできます");
    }
}
