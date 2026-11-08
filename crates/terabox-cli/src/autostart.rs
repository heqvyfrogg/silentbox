//! 起動時の自動実行。
//!
//! Windows は HKCU の `Run` キー、Linux は `systemd --user` のユニットを使う。
//! どちらも**現在のユーザー単位**で、管理者権限を要求しない。
//!
//! ## 安全側の設計
//! - 既定は無効。`enable` を明示したときだけ登録する。
//! - 登録内容は「マウントだけ」。ファイル転送やログインは自動で走らせない。
//! - 失敗したときに黙って続行しない。登録できなければ理由を返す。
//! - `disable` は自分が書いた登録だけを消す。他者の設定を触らない。

use std::path::PathBuf;

/// 自動実行の登録名。
const ENTRY_NAME: &str = "silentbox";

/// 状態。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum State {
    /// 登録されている。
    Enabled {
        /// 登録されているコマンド列。
        command: String,
    },
    /// 登録されていない。
    Disabled,
    /// 確認できなかった（理由付き）。
    Unknown(String),
}

impl State {
    /// 有効か。
    #[allow(dead_code)]
    pub fn is_enabled(&self) -> bool {
        matches!(self, State::Enabled { .. })
    }
}

/// 自動実行で使う既定のマウント先。
pub fn default_target() -> String {
    if cfg!(windows) {
        "T:".to_string()
    } else {
        let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("."));
        home.join("tbx").display().to_string()
    }
}

/// 登録するコマンド列（実行ファイルの絶対パス + `mount <target>`）。
///
/// `crypt` が真なら `--crypt` を付ける。暗号化して運用している利用者は、
/// 再起動後も暗号化したままマウントされてほしい（平文で生えてしまうと
/// 意図と違う）。
fn command_line(exe: &std::path::Path, target: &str, crypt: bool) -> String {
    if crypt {
        format!("\"{}\" mount {} --crypt", exe.display(), target)
    } else {
        format!("\"{}\" mount {}", exe.display(), target)
    }
}

// ---- Windows ----

#[cfg(windows)]
fn run_key() -> std::io::Result<winreg::RegKey> {
    use winreg::enums::HKEY_CURRENT_USER;
    winreg::RegKey::predef(HKEY_CURRENT_USER)
        .open_subkey_with_flags(
            r"Software\Microsoft\Windows\CurrentVersion\Run",
            winreg::enums::KEY_READ | winreg::enums::KEY_WRITE,
        )
        .map_err(|e| std::io::Error::from_raw_os_error(e.raw_os_error().unwrap_or(0)))
}

/// 現在の状態を読む。
pub fn status() -> State {
    #[cfg(windows)]
    {
        let key = match run_key() {
            Ok(k) => k,
            // Run キー自体が無い環境は「未登録」として扱う。
            Err(e) if e.raw_os_error() == Some(2) => return State::Disabled,
            Err(e) => return State::Unknown(e.to_string()),
        };

        match key.get_value::<String, _>(ENTRY_NAME) {
            Ok(command) => State::Enabled { command },
            // `winreg` は「値が無い」を OS エラー 2 で返す。`ErrorKind::NotFound`
            // へは変換されないため、生の errno で判定する（実測: 変換すると
            // 未登録を「確認できない」と誤報告した）。
            Err(e) if e.raw_os_error() == Some(2) => State::Disabled,
            Err(e) => State::Unknown(e.to_string()),
        }
    }
    #[cfg(not(windows))]
    {
        let Some(path) = unit_path() else {
            return State::Unknown("ホームディレクトリを特定できません".to_string());
        };
        if !path.exists() {
            return State::Disabled;
        }
        match std::fs::read_to_string(&path) {
            Ok(text) => {
                let command = text
                    .lines()
                    .find_map(|l| l.strip_prefix("ExecStart="))
                    .unwrap_or("")
                    .trim()
                    .to_string();
                State::Enabled {
                    command: command.split("--").next().unwrap_or("").trim().to_string(),
                }
            }
            Err(e) => State::Unknown(format!("{} を読めません: {e}", path.display())),
        }
    }
}

// ---- Linux（systemd --user） ----

/// ユニットの置き場所。
///
/// `XDG_CONFIG_HOME` を読むが、**`systemd --user` は起動時に読んだ値を
/// 固定して持つ**。プロセスごとに `XDG_CONFIG_HOME` を変えて実行しても、
/// 既に動いている systemd はその場所を見ない（実測: `daemon-reload` 後も
/// `Unit silentbox.service does not exist`）。そのため、独自の値を渡された
/// 場合は「systemd からは見えないかもしれない」と警告する。
#[cfg(not(windows))]
fn unit_path() -> Option<PathBuf> {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| dirs::home_dir().map(|h| h.join(".config")))?;
    Some(base.join("systemd/user/silentbox.service"))
}

/// 独自の `XDG_CONFIG_HOME` を使っているか（systemd が見落とす可能性の判定）。
#[cfg(not(windows))]
fn uses_custom_config_home() -> bool {
    std::env::var_os("XDG_CONFIG_HOME").is_some()
}

#[cfg(not(windows))]
fn unit_text(exe: &std::path::Path, target: &str, crypt: bool) -> String {
    // `Type=simple` で常駐させ、落ちたら 5 秒後に再起動する。無限再起動で
    // ログを埋めないよう `StartLimitBurst` を切る。
    format!(
        "[Unit]\n\
         Description=silentbox (tbx) mount\n\
         After=network-online.target\n\
         StartLimitIntervalSec=300\n\
         StartLimitBurst=5\n\
         \n\
         [Service]\n\
         Type=simple\n\
         ExecStart=\"{}\" mount {} --mode fuse{crypt}\n\
         Restart=on-failure\n\
         RestartSec=5\n\
         \n\
         [Install]\n\
         WantedBy=default.target\n",
        exe.display(),
        target,
        if crypt { " --crypt" } else { "" }
    )
}

// ---- 公開 API ----

/// 自動実行を登録する。既存の登録は上書きする（自分が書いたものだけ）。
pub fn enable(target: &str, crypt: bool) -> Result<String, String> {
    let exe = std::env::current_exe()
        .map_err(|e| format!("実行ファイルの位置を特定できません: {e}"))?;
    let command = command_line(&exe, target, crypt);

    #[cfg(windows)]
    {
        let key = run_key().map_err(|e| e.to_string())?;
        key.set_value(ENTRY_NAME, &command)
            .map_err(|e| format!("Run キーへ登録できません: {e}"))?;
        Ok(command)
    }
    #[cfg(not(windows))]
    {
        let path = unit_path().ok_or_else(|| "ホームディレクトリを特定できません".to_string())?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("{} を作れません: {e}", parent.display()))?;
        }
        let text = unit_text(&exe, target, crypt);
        std::fs::write(&path, &text).map_err(|e| format!("{} へ書けません: {e}", path.display()))?;

        // 新しく書いたユニットは systemd にまだ見えていない。`daemon-reload`
        // を先に実行しないと `enable` が「Unit does not exist」で失敗する
        // （実測）。
        let _ = std::process::Command::new("systemctl")
            .args(["--user", "daemon-reload"])
            .output();

        // 有効化は systemctl が要る。失敗しても登録自体は残るので、
        // 呼び出し側が「有効化コマンド」を表示できるようにメッセージを返す。
        if uses_custom_config_home() {
            eprintln!(
                "注意: 独自の XDG_CONFIG_HOME を設定しています。systemd --user は起動時の値を使うため、この場所のユニットを認識しない可能性があります。"
            );
        }

        let status = std::process::Command::new("systemctl")
            .args(["--user", "enable", "--now", "silentbox.service"])
            .output();
        match status {
            Ok(out) if out.status.success() => Ok(format!("{command}（systemd で有効化済み）")),
            Ok(out) => {
                let err = String::from_utf8_lossy(&out.stderr);
                Err(format!(
                    "ユニットは書きましたが有効化に失敗しました: {}\n手動で `systemctl --user enable --now silentbox.service` を実行してください。",
                    err.trim()
                ))
            }
            Err(e) => Err(format!(
                "ユニットは書きましたが systemctl を実行できません: {e}\n手動で `systemctl --user enable --now silentbox.service` を実行してください。"
            )),
        }
    }
}

/// 自動実行の登録を外す。
pub fn disable() -> Result<bool, String> {
    #[cfg(windows)]
    {
        let key = run_key().map_err(|e| e.to_string())?;
        match key.delete_value(ENTRY_NAME) {
            Ok(()) => Ok(true),
            Err(e) if e.raw_os_error() == Some(2) => Ok(false),
            Err(e) => Err(format!("Run キーから外せません: {e}")),
        }
    }
    #[cfg(not(windows))]
    {
        let Some(path) = unit_path() else {
            return Err("ホームディレクトリを特定できません".to_string());
        };
        let _ = std::process::Command::new("systemctl")
            .args(["--user", "disable", "--now", "silentbox.service"])
            .output();
        if !path.exists() {
            return Ok(false);
        }
        std::fs::remove_file(&path).map_err(|e| format!("{} を消せません: {e}", path.display()))?;
        Ok(true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_target_is_os_specific() {
        let t = default_target();
        if cfg!(windows) {
            assert!(t.contains(':'), "Windows はドライブレターのはず: {t}");
        } else {
            assert!(t.contains('/'), "Unix はパスのはず: {t}");
        }
    }

    #[test]
    fn command_line_quotes_the_path() {
        let exe = std::path::Path::new("/tmp/with space/tbx");
        let cmd = command_line(exe, "T:", false);
        assert!(cmd.starts_with('"'), "パスは引用符で囲む: {cmd}");
        assert!(cmd.contains("mount T:"), "{cmd}");
        assert!(!cmd.contains("--crypt"), "指定が無ければ付けない: {cmd}");
    }

    #[test]
    fn command_line_carries_the_crypt_flag() {
        // 暗号化して運用している利用者は、再起動後も暗号化でマウントされないと
        // 平文で生えてしまう。
        let exe = std::path::Path::new("/tmp/tbx");
        let cmd = command_line(exe, "T:", true);
        assert!(cmd.contains("mount T: --crypt"), "{cmd}");
    }

    #[test]
    fn state_reports_enabled() {
        let s = State::Enabled {
            command: "x".to_string(),
        };
        assert!(s.is_enabled());
        assert!(!State::Disabled.is_enabled());
    }

    #[cfg(not(windows))]
    #[test]
    fn unit_text_has_required_keys() {
        let text = unit_text(std::path::Path::new("/usr/bin/tbx"), "/home/u/tbx", false);
        assert!(!text.contains("--crypt"), "指定が無ければ付けない");
        assert!(text.contains("ExecStart="), "ExecStart が無い");
        assert!(text.contains("--mode fuse"), "FUSE を指定していない");
        assert!(text.contains("WantedBy=default.target"), "有効化の対象が無い");
        assert!(text.contains("Restart=on-failure"), "再起動の指定が無い");
    }
}
