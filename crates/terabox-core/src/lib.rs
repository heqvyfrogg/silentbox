//! TeraBox プロトコルクライアントの中核。
//!
//! このクレートは TUI にも Windows API にも依存しない。認証・API・転送・
//! 転送台帳・VFS 抽象だけを持ち、CLI / TUI / マウントから共有される。
//!
//! 設計方針:
//! - 秘密情報（`ndus` / `jsToken` / `bdstoken`）は `Debug` で必ず伏せる。
//! - サーバが返した数値・文言は捨てずに保持し、解釈は 1 箇所（`errno`）に集約する。
//! - 未検証の推測を事実として扱わない。`unverified` の印をドキュメントと
//!   コードコメントの両方に残す。

pub mod api;
pub mod bootstrap;
pub mod config;
pub mod error;
pub mod models;
pub mod percent;
pub mod session;
pub mod store;
pub mod transfer;
pub mod vfs;

// 暗号化バックエンド（rclone crypt 互換）。既定ではビルドに含めない。
#[cfg(feature = "crypt")]
pub mod crypt;
#[cfg(feature = "crypt")]
pub mod eme;

pub use bootstrap::Bootstrap;
pub use error::{ApiError, TbxError};
pub use models::{FsEntry, QuotaInfo, ShareInfo};
pub use session::Session;

/// このクライアントが名乗るアプリケーション ID（Web 版と同じ値）。
///
/// DEX 文字列から `app_id=250528`（Web/PCS）と `app_id=16892322`（アプリ）の
/// 2 系統を確認している。本クライアントはセッション Cookie 方式の Web API を
/// 使うため 250528 を採用する。
pub const APP_ID_WEB: &str = "250528";

/// アプリ版のアプリケーション ID。アップロード系の一部で使われるが、
/// 本クライアントでは既定で使わない（未検証のため）。
pub const APP_ID_APP: &str = "16892322";

/// 既定の User-Agent。
///
/// dlink 取得時と実ダウンロード時で UA が食い違うと 403 になることが
/// 実運用で知られているため、両方でこの定数だけを使う。
pub const DEFAULT_UA: &str =
    "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/131.0.0.0 Safari/537.36";

/// ブラウザ経由の Cookie として付与する固定値。
pub const PANWEB: &str = "1";
