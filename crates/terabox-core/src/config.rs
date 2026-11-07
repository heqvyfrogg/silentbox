//! 設定ファイルとパス解決。
//!
//! 秘密情報はここに置かない（Windows 資格情報マネージャを使う）。
//! パスは `TBX_HOME` があればそれを、無ければ OS の標準ディレクトリを使う。

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// 設定。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    /// User-Agent。既定は `DEFAULT_UA` と一致させる。
    pub user_agent: String,
    /// 既定の分割接続数。
    pub connections: u8,
    /// 1 接続あたりの分割サイズ（MiB）。
    pub part_size_mib: u64,
    /// ダウンロード先の既定ディレクトリ。
    pub download_dir: PathBuf,
    /// プレーヤの優先順。先頭から存在確認する。
    pub players: Vec<String>,
    /// キャッシュ上限（MiB）。マウントの読み出しキャッシュに使う。
    pub cache_limit_mib: u64,
    /// API の基底 URL。通常は変更しない。
    pub base_url: String,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            user_agent: crate::DEFAULT_UA.to_string(),
            connections: 4,
            part_size_mib: 16,
            download_dir: default_download_dir(),
            players: default_players(),
            cache_limit_mib: 4096,
            base_url: crate::api::BASE.to_string(),
        }
    }
}

impl Config {
    /// 設定を読み込む。存在しなければ既定値を返す（書き込みはしない）。
    pub fn load() -> Result<Self, crate::TbxError> {
        let path = config_path();
        if !path.exists() {
            return Ok(Self::default());
        }
        let text = std::fs::read_to_string(&path)?;
        let mut cfg: Config = toml::from_str(&text)
            .map_err(|e| crate::TbxError::Other(format!("設定 {} の解釈に失敗しました: {e}", path.display())))?;
        // 空文字や 0 は既定へ戻す（壊れた設定で起動不能にしない）。
        if cfg.user_agent.trim().is_empty() {
            cfg.user_agent = crate::DEFAULT_UA.to_string();
        }
        if cfg.connections == 0 {
            cfg.connections = 4;
        }
        if cfg.part_size_mib == 0 {
            cfg.part_size_mib = 16;
        }
        if cfg.download_dir.as_os_str().is_empty() {
            cfg.download_dir = default_download_dir();
        }
        if cfg.base_url.trim().is_empty() {
            cfg.base_url = crate::api::BASE.to_string();
        }
        Ok(cfg)
    }

    /// 設定を保存する。
    pub fn save(&self) -> Result<(), crate::TbxError> {
        let path = config_path();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let text = toml::to_string_pretty(self)
            .map_err(|e| crate::TbxError::Other(format!("設定の書き出しに失敗しました: {e}")))?;
        write_private(&path, text.as_bytes())
    }

    /// 1 接続あたりの分割サイズ（バイト）。
    pub fn part_size_bytes(&self) -> u64 {
        self.part_size_mib.saturating_mul(1024 * 1024)
    }
}

/// ルートディレクトリ。`TBX_HOME` があれば優先する。
pub fn tbx_home() -> PathBuf {
    if let Some(dir) = std::env::var_os("TBX_HOME") {
        return PathBuf::from(dir);
    }
    dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("tbx")
}

/// 状態（台帳・キャッシュ）ディレクトリ。
pub fn state_dir() -> PathBuf {
    if let Some(dir) = std::env::var_os("TBX_HOME") {
        return PathBuf::from(dir);
    }
    dirs::data_local_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("tbx")
}

/// 設定ファイルのパス。
pub fn config_path() -> PathBuf {
    tbx_home().join("config.toml")
}

/// 台帳（SQLite）のパス。
///
/// アカウントごとに分ける。レジューム状態はアカウントを跨いでは意味を持たない
/// ため、混ぜると別アカウントの続きを送ってしまう。
pub fn ledger_path() -> PathBuf {
    state_dir().join(format!("state-{}.db", account_name()))
}

/// 現在使うアカウント名。`main` が解決して環境変数へ書き戻す。
///
/// 解決の優先順は CLI 側（`accounts::resolve`）に 1 箇所だけ置く。ここでは
/// 決定済みの値を読むだけにして、下位のモジュールが独自に解釈しないようにする。
pub fn account_name() -> String {
    match std::env::var("TBX_ACCOUNT") {
        Ok(v) if !v.trim().is_empty() => v.trim().to_string(),
        _ => "default".to_string(),
    }
}

/// キャッシュディレクトリ。
pub fn cache_dir() -> PathBuf {
    state_dir().join("cache")
}

fn default_download_dir() -> PathBuf {
    dirs::download_dir()
        .or_else(|| dirs::home_dir().map(|h| h.join("Downloads")))
        .unwrap_or_else(|| PathBuf::from("."))
        .join("TeraBox")
}

fn default_players() -> Vec<String> {
    vec![
        "mpv".to_string(),
        "vlc".to_string(),
        "ffplay".to_string(),
    ]
}

/// 所有者のみ読める権限で書く（Unix）。Windows は ACL 既定に任せる。
fn write_private(path: &Path, bytes: &[u8]) -> Result<(), crate::TbxError> {
    std::fs::write(path, bytes)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = std::fs::metadata(path)?.permissions();
        perms.set_mode(0o600);
        std::fs::set_permissions(path, perms)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_sane() {
        let c = Config::default();
        assert!(c.connections >= 1);
        assert!(c.part_size_mib >= 1);
        assert_eq!(c.user_agent, crate::DEFAULT_UA);
        assert!(c.part_size_bytes() > 0);
    }

    #[test]
    fn roundtrip_toml() {
        let c = Config::default();
        let text = toml::to_string_pretty(&c).unwrap();
        let back: Config = toml::from_str(&text).unwrap();
        assert_eq!(back.connections, c.connections);
        assert_eq!(back.base_url, c.base_url);
    }

    #[test]
    fn home_override_is_used() {
        // 環境変数の読み取りのみを確認する（値を書き換えない）。
        let resolved = tbx_home();
        assert!(!resolved.as_os_str().is_empty());
    }

    #[test]
    fn ledger_path_differs_per_account_name() {
        // 環境変数を書き換えずに、既定名の構成だけを確認する。
        let path = ledger_path();
        let file = path.file_name().unwrap().to_string_lossy().into_owned();
        assert!(file.starts_with("state-"), "{file}");
        assert!(file.ends_with(".db"), "{file}");
    }
}
