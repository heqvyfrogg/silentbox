//! セッションの保存と読み出し。
//!
//! Windows 資格情報マネージャ（`keyring` 経由）だけを使う。設定ファイルには
//! 秘密情報を書かない。資格情報マネージャが使えない環境では、明示的な
//! メッセージを出して保存を諦める（平文ファイルへのフォールバックはしない）。

use terabox_core::Session;

const SERVICE: &str = "tbx.terabox";
const ACCOUNT: &str = "session";

/// 保存結果。
pub enum SaveOutcome {
    /// 保存した。
    Saved,
    /// 資格情報マネージャが使えず保存できなかった（理由付き）。
    Unavailable(String),
}

/// セッションを保存する。
pub fn save(session: &Session) -> SaveOutcome {
    let payload = match serde_json::to_string(session) {
        Ok(s) => s,
        Err(e) => return SaveOutcome::Unavailable(format!("セッションを直列化できません: {e}")),
    };
    match keyring::Entry::new(SERVICE, ACCOUNT) {
        Ok(entry) => match entry.set_password(&payload) {
            Ok(()) => SaveOutcome::Saved,
            Err(e) => SaveOutcome::Unavailable(format!(
                "資格情報マネージャへ保存できませんでした: {e}"
            )),
        },
        Err(e) => SaveOutcome::Unavailable(format!(
            "資格情報マネージャを開けませんでした: {e}"
        )),
    }
}

/// セッションを読み出す。
pub fn load() -> Result<Option<Session>, String> {
    let entry = keyring::Entry::new(SERVICE, ACCOUNT)
        .map_err(|e| format!("資格情報マネージャを開けませんでした: {e}"))?;
    match entry.get_password() {
        Ok(text) => {
            let session: Session = serde_json::from_str(&text)
                .map_err(|e| format!("保存されたセッションを読めませんでした: {e}"))?;
            Ok(Some(session))
        }
        Err(keyring::Error::NoEntry) => Ok(None),
        Err(e) => Err(format!("資格情報マネージャから読めませんでした: {e}")),
    }
}

/// セッションを削除する。
pub fn delete() -> Result<bool, String> {
    let entry = keyring::Entry::new(SERVICE, ACCOUNT)
        .map_err(|e| format!("資格情報マネージャを開けませんでした: {e}"))?;
    match entry.delete_credential() {
        Ok(()) => Ok(true),
        Err(keyring::Error::NoEntry) => Ok(false),
        Err(e) => Err(format!("削除できませんでした: {e}")),
    }
}
