//! セッション実体の保存。
//!
//! 既定（`auto`）は OS の資格情報ストア（Windows は資格情報マネージャ、Linux は
//! Secret Service、macOS は Keychain）を優先する。ヘッドレス Linux（D-Bus 不在）や
//! musl ビルドなど資格情報ストアが使えない環境では、`tbx_home()/secrets/<name>.json`
//! へ 0600 で自動フォールバックし、呼び出し元へその理由を返す。
//!
//! `--store keyring` を明示した場合はフォールバックせず失敗させる。

use std::path::{Path, PathBuf};

use terabox_core::Session;

/// 資格情報ストアのサービス名。
#[allow(dead_code)]
const SERVICE: &str = "tbx.terabox";

/// 旧版が使っていたアカウント名。移行でだけ読む。
const LEGACY_ACCOUNT: &str = "session";

/// このビルドが資格情報ストアを持っているか。
///
/// musl では D-Bus の静的リンクが現実的でないため積んでいない。
#[allow(dead_code)]
pub const HAS_KEYRING: bool =
    cfg!(any(target_os = "windows", target_os = "macos", target_env = "gnu"));

/// 保存先の希望（CLI `--store` / `TBX_STORE`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StorePreference {
    /// 利用可能なら OS 資格情報ストア、使えなければ file（0600）へ自動フォールバック。
    Auto,
    /// OS 資格情報ストアを強制（使えなければエラー）。
    Keyring,
    /// ファイル保存（0600）を強制。
    File,
}

impl StorePreference {
    /// 文字列から解釈する。
    pub fn parse(value: &str) -> Result<Self, String> {
        match value.trim() {
            "auto" => Ok(Self::Auto),
            "keyring" => Ok(Self::Keyring),
            "file" => Ok(Self::File),
            other => Err(format!(
                "保存先は auto / keyring / file のいずれかを指定してください（受け取った値: {other}）"
            )),
        }
    }
}

/// 実際に使われる保存先の種類。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StoreKind {
    /// OS の資格情報ストア。
    Keyring,
    /// `tbx_home()/secrets/<name>.json`（0600）。
    File,
}

impl StoreKind {
    /// 文字列から解釈する。
    #[allow(dead_code)]
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

/// 保存の結果。自動フォールバックの有無と呼び出し元への説明を持つ。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SaveOutcome {
    pub kind: StoreKind,
    pub fell_back: bool,
    pub fallback_reason: Option<String>,
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

/// 資格情報ストアが使えない明らかな理由があれば返す（事前判定）。
pub fn keyring_availability_issue() -> Option<String> {
    if !HAS_KEYRING {
        return Some("このビルドは OS 資格情報ストアを含みません（musl ビルドなど）".into());
    }
    #[cfg(all(unix, not(target_os = "macos"), target_env = "gnu"))]
    {
        let dbus = std::env::var("DBUS_SESSION_BUS_ADDRESS").ok();
        let runtime_dir = std::env::var("XDG_RUNTIME_DIR").ok();
        let has_runtime_bus = runtime_dir
            .as_deref()
            .filter(|v| !v.trim().is_empty())
            .map(|dir| Path::new(dir).join("bus").exists())
            .unwrap_or(false);
        return linux_dbus_issue(dbus.as_deref(), has_runtime_bus);
    }
    #[cfg(not(all(unix, not(target_os = "macos"), target_env = "gnu")))]
    {
        None
    }
}

/// Linux での D-Bus セッションバス有無を判定する純関数（テスト用）。
#[allow(dead_code)]
pub fn linux_dbus_issue(dbus_addr: Option<&str>, has_runtime_bus: bool) -> Option<String> {
    let has_env = dbus_addr.map(|v| !v.trim().is_empty()).unwrap_or(false);
    if has_env || has_runtime_bus {
        None
    } else {
        Some(
            "D-Bus セッションバス（DBUS_SESSION_BUS_ADDRESS）が見つからないため Secret Service を利用できません"
                .into(),
        )
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

/// セッションを保存する（指定種別そのまま）。
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

/// 希望（`auto` / `keyring` / `file`）に従ってセッションを保存する。
///
/// `Auto` では資格情報ストアが使えない場合に 0600 の file ストアへ自動フォールバックし、
/// `SaveOutcome` にフォールバックした事実と理由を入れて返す。
pub fn save_with_preference(
    home: &Path,
    account: &str,
    pref: StorePreference,
    session: &Session,
) -> Result<SaveOutcome, StoreError> {
    save_with_preference_inner(home, account, pref, session, keyring_availability_issue())
}

fn save_with_preference_inner(
    home: &Path,
    account: &str,
    pref: StorePreference,
    session: &Session,
    precheck_issue: Option<String>,
) -> Result<SaveOutcome, StoreError> {
    match pref {
        StorePreference::File => {
            save(home, account, StoreKind::File, session)?;
            Ok(SaveOutcome {
                kind: StoreKind::File,
                fell_back: false,
                fallback_reason: None,
            })
        }
        StorePreference::Keyring => {
            if let Some(reason) = precheck_issue {
                return Err(StoreError::Unavailable(format!(
                    "{reason}。`--store file` または環境変数 `TBX_STORE=file` を指定してください。"
                )));
            }
            save(home, account, StoreKind::Keyring, session).map_err(|e| match e {
                StoreError::Unavailable(m) => StoreError::Unavailable(format!(
                    "{m}。`--store file` または環境変数 `TBX_STORE=file` を指定してください。"
                )),
                other => other,
            })?;
            Ok(SaveOutcome {
                kind: StoreKind::Keyring,
                fell_back: false,
                fallback_reason: None,
            })
        }
        StorePreference::Auto => {
            if let Some(reason) = precheck_issue {
                save(home, account, StoreKind::File, session)?;
                return Ok(SaveOutcome {
                    kind: StoreKind::File,
                    fell_back: true,
                    fallback_reason: Some(reason),
                });
            }
            match save(home, account, StoreKind::Keyring, session) {
                Ok(()) => Ok(SaveOutcome {
                    kind: StoreKind::Keyring,
                    fell_back: false,
                    fallback_reason: None,
                }),
                Err(StoreError::Unavailable(reason)) => {
                    save(home, account, StoreKind::File, session)?;
                    Ok(SaveOutcome {
                        kind: StoreKind::File,
                        fell_back: true,
                        fallback_reason: Some(reason),
                    })
                }
                Err(other) => Err(other),
            }
        }
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
/// 1. 秘密ファイルが存在すれば `File`。
/// 2. 環境変数 `TBX_STORE=file` が指定されていれば `File`。
/// 3. 資格情報ストアが使えない環境（musl や D-Bus 不在）なら `File`
///    （未ログイン時に「ストアが無い」ではなく「未ログイン」を正しく返すため）。
/// 4. それ以外は `Keyring`。
pub fn detect_kind(home: &Path, account: &str) -> StoreKind {
    if secret_file(home, account).exists() {
        return StoreKind::File;
    }
    if matches!(
        std::env::var("TBX_STORE").ok().as_deref().map(str::trim),
        Some("file")
    ) {
        return StoreKind::File;
    }
    if keyring_availability_issue().is_some() {
        return StoreKind::File;
    }
    StoreKind::Keyring
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
    if keyring_availability_issue().is_some() {
        return false;
    }
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

// ---- 平文ファイル（0600 原子置換） ----

/// 0600 で秘密ファイルを原子的に書く。
///
/// Unix では `secrets/` を 0700 にし、作成時点から 0600 の一時ファイルへ書いてから
/// rename で置き換える（権限露出ウィンドウと中断時の半端な JSON を防ぐ）。
fn write_secret_file(home: &Path, account: &str, payload: &str) -> Result<(), StoreError> {
    use std::io::Write;

    let dir = secret_dir(home);
    std::fs::create_dir_all(&dir)
        .map_err(|e| StoreError::Failed(format!("{} を作れません: {e}", dir.display())))?;
    restrict_dir_permissions(&dir)
        .map_err(|e| StoreError::Failed(format!("{} の権限を絞れません: {e}", dir.display())))?;

    let path = secret_file(home, account);
    let tmp = dir.join(format!(".{account}.json.tmp.{}", std::process::id()));

    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }

    let mut file = opts
        .open(&tmp)
        .map_err(|e| StoreError::Failed(format!("{} を開けません: {e}", tmp.display())))?;
    file.write_all(payload.as_bytes())
        .and_then(|_| file.sync_all())
        .map_err(|e| {
            let _ = std::fs::remove_file(&tmp);
            StoreError::Failed(format!("{} へ書けません: {e}", tmp.display()))
        })?;
    drop(file);

    restrict_permissions(&tmp)
        .map_err(|e| StoreError::Failed(format!("{} の権限を絞れません: {e}", tmp.display())))?;

    std::fs::rename(&tmp, &path).map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        StoreError::Failed(format!("{} へ配置できません: {e}", path.display()))
    })
}

/// ディレクトリの権限を絞る（Unix は 0700）。
fn restrict_dir_permissions(path: &Path) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = std::fs::metadata(path)?.permissions();
        perms.set_mode(0o700);
        std::fs::set_permissions(path, perms)?;
    }
    #[cfg(not(unix))]
    {
        let _ = path;
    }
    Ok(())
}

/// ファイルの権限を絞る（Unix は 0600、Windows は ACL 既定に任せる）。
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
pub fn check_permissions(path: &Path) -> Result<(), String> {
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
    fn store_kind_and_preference_parsing() {
        assert_eq!(StoreKind::parse("keyring").unwrap(), StoreKind::Keyring);
        assert_eq!(StoreKind::parse("file").unwrap(), StoreKind::File);
        assert!(StoreKind::parse("plain").is_err());

        assert_eq!(StorePreference::parse("auto").unwrap(), StorePreference::Auto);
        assert_eq!(
            StorePreference::parse("keyring").unwrap(),
            StorePreference::Keyring
        );
        assert_eq!(StorePreference::parse("file").unwrap(), StorePreference::File);
        assert!(StorePreference::parse("unknown").is_err());
    }

    #[test]
    fn file_store_roundtrip_and_overwrite() {
        let home = temp_home("file-roundtrip");
        let mut session = dummy_session();
        save(&home, "work", StoreKind::File, &session).unwrap();
        // 上書き（原子置換）が Windows でも失敗しないことを確認。
        session.uk = Some("uk-2".into());
        save(&home, "work", StoreKind::File, &session).unwrap();
        let back = load(&home, "work", StoreKind::File).unwrap().unwrap();
        assert_eq!(back.uk.as_deref(), Some("uk-2"));
        assert_eq!(back.ndus, "dummy-ndus");
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn auto_preference_falls_back_to_file_when_keyring_unavailable() {
        let home = temp_home("auto-fallback");
        let outcome = save_with_preference_inner(
            &home,
            "srv",
            StorePreference::Auto,
            &dummy_session(),
            Some("D-Bus なし".into()),
        )
        .unwrap();
        assert_eq!(outcome.kind, StoreKind::File);
        assert!(outcome.fell_back);
        assert_eq!(outcome.fallback_reason.as_deref(), Some("D-Bus なし"));
        let back = load(&home, "srv", StoreKind::File).unwrap().unwrap();
        assert_eq!(back.ndus, "dummy-ndus");
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn explicit_keyring_does_not_fall_back() {
        let home = temp_home("explicit-keyring");
        let err = save_with_preference_inner(
            &home,
            "srv",
            StorePreference::Keyring,
            &dummy_session(),
            Some("D-Bus なし".into()),
        )
        .unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("--store file"), "{msg}");
        assert!(!secret_file(&home, "srv").exists());
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn linux_dbus_detection_checks_env_and_runtime_socket() {
        assert!(linux_dbus_issue(None, false).is_some());
        assert!(linux_dbus_issue(Some("  "), false).is_some());
        assert!(linux_dbus_issue(Some("unix:path=/run/user/1000/bus"), false).is_none());
        assert!(linux_dbus_issue(None, true).is_none());
    }

    #[test]
    fn missing_file_returns_none() {
        let home = temp_home("file-missing");
        assert!(load(&home, "nobody", StoreKind::File).unwrap().is_none());
        let _ = std::fs::remove_dir_all(&home);
    }

    #[cfg(unix)]
    #[test]
    fn file_store_sets_0600_and_dir_0700() {
        use std::os::unix::fs::PermissionsExt;
        let home = temp_home("file-mode");
        save(&home, "work", StoreKind::File, &dummy_session()).unwrap();
        let dir_mode = std::fs::metadata(secret_dir(&home))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(dir_mode & 0o777, 0o700, "ディレクトリは 0700 でなければならない");
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
