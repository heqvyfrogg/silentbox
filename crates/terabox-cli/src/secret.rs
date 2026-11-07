//! セッション実体の保存。
//!
//! 既定は OS の資格情報ストア（Windows は資格情報マネージャ、Linux は
//! Secret Service、macOS は Keychain）。資格情報ストアが使えない環境
//! （ヘッドレス Linux・musl ビルドなど）では、`--store file` を明示した
//! ときだけ `tbx_home()/secrets/<name>.json` へ 0600 で書く。
//!
//! 平文保存は既定にしない。明示されていない限り、資格情報ストアが使えなければ
//! 理由を返して諦める（黙って平文に落とさない）。

use std::path::{Path, PathBuf};

use terabox_core::Session;

/// 資格情報ストアのサービス名。
const SERVICE: &str = "tbx.terabox";

/// 旧版が使っていたアカウント名。移行でだけ読む。
const LEGACY_ACCOUNT: &str = "session";

/// このビルドが資格情報ストアを持っているか。
///
/// musl では D-Bus の静的リンクが現実的でないため積んでいない。実行時に
/// 判定して、無い場合は `--store file` を案内する。
#[allow(dead_code)]
pub const HAS_KEYRING: bool =
    cfg!(any(target_os = "windows", target_os = "macos", target_env = "gnu"));

/// 保存先の種類。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StoreKind {
    /// OS の資格情報ストア。
    Keyring,
    /// `tbx_home()/secrets/<name>.json`（0600）。
    File,
}

impl StoreKind {
    /// 文字列から解釈する。
    pub fn parse(value: &str) -> Result<Self, String> {
        match value {
            "keyring" => Ok(Self::Keyring),
            "file" => Ok(Self::File),
            other => Err(format!(
                "保存先は keyring か file を指定してください（受け取った値: {other}）"
            )),
        }
    }

    /// 表示用。
    pub fn label(self) -> &'static str {
        match self {
            Self::Keyring => "keyring",
            Self::File => "file",
        }
    }
}

/// 保存に失敗した理由。
#[derive(Debug)]
pub enum StoreError {
    /// 資格情報ストアが使えない（環境の問題）。
    Unavailable(String),
    /// 読み書きの失敗。
    Failed(String),
}

impl std::fmt::Display for StoreError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unavailable(m) => write!(f, "保存先が使えません: {m}"),
            Self::Failed(m) => write!(f, "保存に失敗しました: {m}"),
        }
    }
}

/// 秘密の置き場所を決める。
pub fn secret_dir(home: &Path) -> PathBuf {
    home.join("secrets")
}

/// 平文保存のファイルパス。
pub fn secret_file(home: &Path, account: &str) -> PathBuf {
    secret_dir(home).join(format!("{account}.json"))
}

/// セッションを保存する。
pub fn save(
    home: &Path,
    account: &str,
    kind: StoreKind,
    session: &Session,
) -> Result<(), StoreError> {
    let payload = serde_json::to_string(session)
        .map_err(|e| StoreError::Failed(format!("セッションを直列化できません: {e}")))?;
    match kind {
        StoreKind::Keyring => keyring_save(account, &payload),
        StoreKind::File => write_secret_file(home, account, &payload),
    }
}

/// セッションを読み出す。無ければ `Ok(None)`。
pub fn load(home: &Path, account: &str, kind: StoreKind) -> Result<Option<Session>, String> {
    let payload: Option<String> = match kind {
        StoreKind::Keyring => keyring_load(account)?,
        StoreKind::File => {
            let path = secret_file(home, account);
            if !path.exists() {
                return Ok(None);
            }
            Some(read_secret_file(&path)?)
        }
    };
    let Some(payload) = payload else {
        return Ok(None);
    };
    let session: Session = serde_json::from_str(&payload)
        .map_err(|e| format!("保存されたセッションを読めません: {e}"))?;
    Ok(Some(session))
}

/// 保存済みの種類を推定する（`--store` を省いたとき用）。
///
/// ファイルがあり、権限が適切なら file、無ければ keyring。両方ある場合は
/// 明示が必要なので keyring を優先する（既定の置き場所を変えない）。
pub fn detect_kind(home: &Path, account: &str) -> StoreKind {
    if secret_file(home, account).exists() {
        StoreKind::File
    } else {
        StoreKind::Keyring
    }
}

/// セッションを削除する。戻り値は「何か消したか」。
pub fn delete(home: &Path, account: &str, kind: StoreKind) -> Result<bool, String> {
    match kind {
        StoreKind::Keyring => keyring_delete(account),
        StoreKind::File => {
            let path = secret_file(home, account);
            if !path.exists() {
                return Ok(false);
            }
            std::fs::remove_file(&path)
                .map_err(|e| format!("{} を消せません: {e}", path.display()))?;
            Ok(true)
        }
    }
}

/// 旧版の `session` エントリが残っているか。
pub fn has_legacy() -> bool {
    matches!(keyring_load_raw(LEGACY_ACCOUNT), Ok(Some(_)))
}

/// 旧版のセッションを読む。
pub fn load_legacy() -> Result<Option<Session>, String> {
    let Some(payload) = keyring_load_raw(LEGACY_ACCOUNT)? else {
        return Ok(None);
    };
    let session: Session =
        serde_json::from_str(&payload).map_err(|e| format!("旧セッションを読めません: {e}"))?;
    Ok(Some(session))
}

/// 旧版のエントリを消す。
pub fn delete_legacy() -> Result<(), String> {
    keyring_delete_raw(LEGACY_ACCOUNT).map(|_| ())
}

/// 旧版はアカウント名を接頭辞なしで保存していた。読むときだけ接頭辞を付けない。
#[cfg(any(target_os = "windows", target_os = "macos", target_env = "gnu"))]
fn keyring_load_raw(account: &str) -> Result<Option<String>, String> {
    let entry = keyring::Entry::new(SERVICE, account)
        .map_err(|e| format!("資格情報ストアを開けません: {e}"))?;
    match entry.get_password() {
        Ok(text) => Ok(Some(text)),
        Err(keyring::Error::NoEntry) => Ok(None),
        Err(e) => Err(format!("資格情報ストアから読めません: {e}")),
    }
}

#[cfg(any(target_os = "windows", target_os = "macos", target_env = "gnu"))]
fn keyring_delete_raw(account: &str) -> Result<bool, String> {
    let entry = keyring::Entry::new(SERVICE, account)
        .map_err(|e| format!("資格情報ストアを開けません: {e}"))?;
    match entry.delete_credential() {
        Ok(()) => Ok(true),
        Err(keyring::Error::NoEntry) => Ok(false),
        Err(e) => Err(format!("削除できませんでした: {e}")),
    }
}

#[cfg(not(any(target_os = "windows", target_os = "macos", target_env = "gnu")))]
fn keyring_load_raw(_account: &str) -> Result<Option<String>, String> {
    Ok(None)
}

#[cfg(not(any(target_os = "windows", target_os = "macos", target_env = "gnu")))]
fn keyring_delete_raw(_account: &str) -> Result<bool, String> {
    Ok(false)
}

// ---- 資格情報ストア（ビルドに含まれる場合だけ実体を持つ） ----

#[cfg(any(target_os = "windows", target_os = "macos", target_env = "gnu"))]
fn keyring_save(account: &str, payload: &str) -> Result<(), StoreError> {
    let entry = keyring::Entry::new(SERVICE, &format!("account:{account}"))
        .map_err(|e| StoreError::Unavailable(format!("資格情報ストアを開けません: {e}")))?;
    entry
        .set_password(payload)
        .map_err(|e| StoreError::Unavailable(format!("資格情報ストアへ書けません: {e}")))
}

#[cfg(any(target_os = "windows", target_os = "macos", target_env = "gnu"))]
fn keyring_load(account: &str) -> Result<Option<String>, String> {
    let entry = keyring::Entry::new(SERVICE, &format!("account:{account}"))
        .map_err(|e| format!("資格情報ストアを開けません: {e}"))?;
    match entry.get_password() {
        Ok(text) => Ok(Some(text)),
        Err(keyring::Error::NoEntry) => Ok(None),
        Err(e) => Err(format!("資格情報ストアから読めません: {e}")),
    }
}

#[cfg(any(target_os = "windows", target_os = "macos", target_env = "gnu"))]
fn keyring_delete(account: &str) -> Result<bool, String> {
    let entry = keyring::Entry::new(SERVICE, &format!("account:{account}"))
        .map_err(|e| format!("資格情報ストアを開けません: {e}"))?;
    match entry.delete_credential() {
        Ok(()) => Ok(true),
        Err(keyring::Error::NoEntry) => Ok(false),
        Err(e) => Err(format!("削除できませんでした: {e}")),
    }
}

#[cfg(not(any(target_os = "windows", target_os = "macos", target_env = "gnu")))]
fn keyring_save(_account: &str, _payload: &str) -> Result<(), StoreError> {
    Err(StoreError::Unavailable(
        "このビルドは資格情報ストアを含みません。`--store file` を指定してください。".to_string(),
    ))
}

#[cfg(not(any(target_os = "windows", target_os = "macos", target_env = "gnu")))]
fn keyring_load(_account: &str) -> Result<Option<String>, String> {
    Err("このビルドは資格情報ストアを含みません。`--store file` を指定してください。".to_string())
}

#[cfg(not(any(target_os = "windows", target_os = "macos", target_env = "gnu")))]
fn keyring_delete(_account: &str) -> Result<bool, String> {
    Err("このビルドは資格情報ストアを含みません。`--store file` を指定してください。".to_string())
}

// ---- 平文ファイル（明示時のみ） ----

/// 0600 で秘密ファイルを書く。
fn write_secret_file(home: &Path, account: &str, payload: &str) -> Result<(), StoreError> {
    let dir = secret_dir(home);
    std::fs::create_dir_all(&dir)
        .map_err(|e| StoreError::Failed(format!("{} を作れません: {e}", dir.display())))?;
    let path = secret_file(home, account);
    std::fs::write(&path, payload)
        .map_err(|e| StoreError::Failed(format!("{} へ書けません: {e}", path.display())))?;
    restrict_permissions(&path)
        .map_err(|e| StoreError::Failed(format!("{} の権限を絞れません: {e}", path.display())))
}

/// 権限を絞る（Unix は 0600、Windows は ACL 既定に任せる）。
fn restrict_permissions(path: &Path) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = std::fs::metadata(path)?.permissions();
        perms.set_mode(0o600);
        std::fs::set_permissions(path, perms)?;
    }
    #[cfg(not(unix))]
    {
        let _ = path;
    }
    Ok(())
}

/// 秘密ファイルを読む。権限が緩ければ読まずに拒否する。
fn read_secret_file(path: &Path) -> Result<String, String> {
    check_permissions(path)?;
    std::fs::read_to_string(path).map_err(|e| format!("{} を読めません: {e}", path.display()))
}

/// 他人が読める権限になっていないか確認する（Unix のみ意味を持つ）。
fn check_permissions(path: &Path) -> Result<(), String> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(path)
            .map_err(|e| format!("{} の権限を読めません: {e}", path.display()))?
            .permissions()
            .mode();
        if mode & 0o077 != 0 {
            return Err(format!(
                "{} の権限が緩すぎます（{:o}）。`chmod 600 {}` を実行してください。",
                path.display(),
                mode & 0o777,
                path.display()
            ));
        }
    }
    #[cfg(not(unix))]
    {
        let _ = path;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_home(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "tbx-secret-test-{}-{tag}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn dummy_session() -> Session {
        Session {
            ndus: "dummy-ndus".into(),
            js_token: "dummy-token".into(),
            bdstoken: Some("dummy-bd".into()),
            uk: Some("uk-1".into()),
            user_agent: "test-agent".into(),
            acquired_at: 123,
        }
    }

    #[test]
    fn store_kind_parsing() {
        assert_eq!(StoreKind::parse("keyring").unwrap(), StoreKind::Keyring);
        assert_eq!(StoreKind::parse("file").unwrap(), StoreKind::File);
        assert!(StoreKind::parse("plain").is_err());
    }

    #[test]
    fn file_store_roundtrip() {
        let home = temp_home("file-roundtrip");
        let session = dummy_session();
        save(&home, "work", StoreKind::File, &session).unwrap();
        let back = load(&home, "work", StoreKind::File).unwrap().unwrap();
        assert_eq!(back.uk.as_deref(), Some("uk-1"));
        assert_eq!(back.ndus, "dummy-ndus");
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn missing_file_returns_none() {
        let home = temp_home("file-missing");
        assert!(load(&home, "nobody", StoreKind::File).unwrap().is_none());
        let _ = std::fs::remove_dir_all(&home);
    }

    #[cfg(unix)]
    #[test]
    fn file_store_sets_0600() {
        use std::os::unix::fs::PermissionsExt;
        let home = temp_home("file-mode");
        save(&home, "work", StoreKind::File, &dummy_session()).unwrap();
        let mode = std::fs::metadata(secret_file(&home, "work"))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600, "0600 でなければならない");
        let _ = std::fs::remove_dir_all(&home);
    }

    #[cfg(unix)]
    #[test]
    fn loose_permissions_are_rejected() {
        use std::os::unix::fs::PermissionsExt;
        let home = temp_home("file-loose");
        save(&home, "work", StoreKind::File, &dummy_session()).unwrap();
        let path = secret_file(&home, "work");
        let mut perms = std::fs::metadata(&path).unwrap().permissions();
        perms.set_mode(0o644);
        std::fs::set_permissions(&path, perms).unwrap();
        assert!(load(&home, "work", StoreKind::File).is_err());
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn detect_prefers_file_when_present() {
        let home = temp_home("detect");
        assert_eq!(detect_kind(&home, "work"), StoreKind::Keyring);
        save(&home, "work", StoreKind::File, &dummy_session()).unwrap();
        assert_eq!(detect_kind(&home, "work"), StoreKind::File);
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn file_delete_reports_nothing_to_do() {
        let home = temp_home("file-delete");
        assert!(!delete(&home, "work", StoreKind::File).unwrap());
        save(&home, "work", StoreKind::File, &dummy_session()).unwrap();
        assert!(delete(&home, "work", StoreKind::File).unwrap());
        let _ = std::fs::remove_dir_all(&home);
    }
}
