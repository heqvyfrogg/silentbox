//! エラー型。サーバが返した数値と文言を捨てずに保持する。

use std::fmt;

/// このクレートの共通エラー。
#[derive(Debug, thiserror::Error)]
pub enum TbxError {
    #[error("HTTP 通信に失敗しました: {0}")]
    Http(#[from] reqwest::Error),

    #[error("JSON の解釈に失敗しました: {0}")]
    Json(#[from] serde_json::Error),

    #[error("入出力に失敗しました: {0}")]
    Io(#[from] std::io::Error),

    #[error("転送台帳（SQLite）の操作に失敗しました: {0}")]
    Ledger(#[from] rusqlite::Error),

    #[error("{0}")]
    Api(#[from] ApiError),

    #[error("保存済みセッションがありません。`tbx login` を実行してください。")]
    NoSession,

    #[error("セッションが失効しています。`tbx login` で再ログインしてください。")]
    SessionExpired,

    #[error("初期化ページから {field} を取り出せませんでした（HTML 構造の変更を疑ってください）")]
    BootstrapFieldMissing { field: &'static str },

    #[error("{0}")]
    Other(String),
}

/// サーバが返した業務エラー。
///
/// `errno` は TeraBox の生値、`message` はサーバの `errmsg`/`show_msg` 原文。
/// 表示用の日本語説明は `display_message()` が返す。生値は詳細表示に残す。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApiError {
    pub endpoint: String,
    pub errno: i64,
    pub message: Option<String>,
    pub request_id: Option<String>,
}

impl ApiError {
    /// 表示用の説明。未知の errno は「未分類」と明示し、生値を併記する。
    pub fn display_message(&self) -> String {
        let known = match self.errno {
            -6 => Some("ログインしていません（セッションが無効です）"),
            2 => Some("指定したファイルまたはフォルダが見つかりません"),
            -9 => Some("ファイルまたはフォルダが存在しません"),
            12 => Some("同じ名前の項目が既に存在します"),
            // 実測: ゴミ箱の復元で、復元先に同名の項目があるとき返る。
            -8 => Some("復元先に同じ名前の項目が既にあります"),
            -10 => Some("容量が不足しています"),
            105 => Some("共有リンクが正しくないか、既に無効になっています"),
            140 => Some("共有リンクの有効期限が切れている可能性があります"),
            // 実測: `shorturlinfo?shorturl=1test` が返す。サーバ文言は "dragdrop"。
            // 意味は特定できていないため、リンク不正の一種として扱う。
            460030 => Some("共有リンクを解決できません（リンクが不正か、形式が違います）"),
            // 実測: 一定量の書き込み後にサーバが返す。文言は "need verify"。
            // セッションは有効なまま（読み取りは通る）なので、書き込み系だけが
            // 検証を要求される状態。再ログインで解除されるかは未検証。
            4000023 => Some(
                "サーバが追加の本人確認を要求しています（書き込みが一時的に拒否されています）",
            ),
            450016 => Some(
                "サーバが追加の本人確認を要求しています（削除が一時的に拒否されています）",
            ),
            31066 => Some("ファイル名が規則に反しています"),
            31355 => Some("アップロードの受付に失敗しました（サーバ側の一時的な問題の可能性）"),
            _ => None,
        };
        match known {
            Some(text) => format!("{text}（errno {}）", self.errno),
            None => format!("サーバがエラーを返しました（errno {}・未分類）", self.errno),
        }
    }

    /// セッション無効を表すか。
    pub fn is_not_logged_in(&self) -> bool {
        self.errno == -6
    }
}

impl fmt::Display for ApiError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} [{}]", self.display_message(), self.endpoint)?;
        if let Some(msg) = &self.message {
            if !msg.is_empty() {
                write!(f, " サーバ文言: {msg}")?;
            }
        }
        if let Some(id) = &self.request_id {
            write!(f, " request_id={id}")?;
        }
        Ok(())
    }
}

impl std::error::Error for ApiError {}

#[cfg(test)]
mod tests {
    use super::*;

    fn err(errno: i64) -> ApiError {
        ApiError {
            endpoint: "/api/list".into(),
            errno,
            message: None,
            request_id: None,
        }
    }

    #[test]
    fn known_errno_is_described() {
        let text = err(-6).display_message();
        assert!(text.contains("ログインしていません"));
        assert!(text.contains("-6"));
    }

    #[test]
    fn unknown_errno_stays_visible() {
        let text = err(987654).display_message();
        assert!(text.contains("未分類"));
        assert!(text.contains("987654"));
    }

    #[test]
    fn not_logged_in_detection() {
        assert!(err(-6).is_not_logged_in());
        assert!(!err(105).is_not_logged_in());
    }
}
