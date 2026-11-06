//! マウント用の共有 Tokio ランタイム。
//!
//! WinFsp のコールバックは専用スレッドから同期呼び出しされる。コールバックごとに
//! ランタイムを作るとスレッドが枯渇するため、1 つの multi-thread ランタイムを
//! プロセスで共有し、`block_on` で待つ。

use std::sync::OnceLock;

use tokio::runtime::{Handle, Runtime};

static RUNTIME: OnceLock<Runtime> = OnceLock::new();

/// 遅延初期化した共有ランタイムのハンドル。
pub fn handle() -> Handle {
    RUNTIME
        .get_or_init(|| {
            tokio::runtime::Builder::new_multi_thread()
                .worker_threads(4)
                .enable_all()
                .thread_name("tbx-mount")
                .build()
                .expect("マウント用ランタイムを作成できませんでした")
        })
        .handle()
        .clone()
}
