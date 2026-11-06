//! セッション情報。
//!
//! 秘密情報（`ndus` / `js_token` / `bdstoken`）は `Debug` で必ず伏せる。
//! 設定ファイル・ログ・エラー出力へ生値を流さないための最低限の防御。

use std::fmt;

use serde::{Deserialize, Serialize};

use crate::bootstrap::Bootstrap;

/// TeraBox のログインセッション。
#[derive(Clone, Serialize, Deserialize)]
pub struct Session {
    /// `ndus` Cookie。数日で失効する。
    pub ndus: String,
    /// `jsToken`。読み取り系 API の必須パラメータ。
    pub js_token: String,
    /// `bdstoken`。書き込み系 API の必須パラメータ。未ログインでは空。
    pub bdstoken: Option<String>,
    /// ユーザー ID。
    pub uk: Option<String>,
    /// このセッションで使う User-Agent。dlink 取得と実 DL で一致させる。
    pub user_agent: String,
    /// 取得時刻（epoch 秒）。失効判定の目安に使う。
    pub acquired_at: i64,
}

impl fmt::Debug for Session {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // 秘密情報は長さだけを示す。値は絶対に出さない。
        f.debug_struct("Session")
            .field("ndus", &Redacted::new(&self.ndus))
            .field("js_token", &Redacted::new(&self.js_token))
            .field("bdstoken", &self.bdstoken.as_deref().map(Redacted::new))
            .field("uk", &self.uk)
            .field("user_agent", &self.user_agent)
            .field("acquired_at", &self.acquired_at)
            .finish()
    }
}

/// 値を伏せたまま長さだけ見せるラッパ。
struct Redacted<'a>(&'a str);

impl<'a> Redacted<'a> {
    fn new(value: &'a str) -> Self {
        Self(value)
    }
}

impl fmt::Debug for Redacted<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "<redacted {} bytes>", self.0.len())
    }
}

impl Session {
    /// 起動情報と `ndus` からセッションを組み立てる。
    pub fn from_bootstrap(ndus: &str, bootstrap: &Bootstrap, user_agent: &str) -> Option<Self> {
        let js_token = bootstrap.js_token.clone()?;
        Some(Self {
            ndus: ndus.to_string(),
            js_token,
            bdstoken: bootstrap.bdstoken.clone(),
            uk: bootstrap.uk.clone(),
            user_agent: user_agent.to_string(),
            acquired_at: now_epoch(),
        })
    }

    /// 書き込み系 API を呼べるか。
    pub fn can_write(&self) -> bool {
        self.bdstoken.as_deref().map(|t| !t.is_empty()).unwrap_or(false)
    }

    /// 起動情報から取り込んだ新しいトークンで更新する（`jsToken` 再取得用）。
    pub fn refresh_from(&mut self, bootstrap: &Bootstrap) {
        if let Some(token) = &bootstrap.js_token {
            self.js_token = token.clone();
        }
        if let Some(token) = &bootstrap.bdstoken {
            if !token.is_empty() {
                self.bdstoken = Some(token.clone());
            }
        }
        if let Some(uk) = &bootstrap.uk {
            if uk != "0" {
                self.uk = Some(uk.clone());
            }
        }
        self.acquired_at = now_epoch();
    }

    /// 表示用のアカウント識別子。`uk` が無ければ「不明」。
    pub fn account_label(&self) -> String {
        match &self.uk {
            Some(uk) if !uk.is_empty() && uk != "0" => format!("uk={uk}"),
            _ => "uk=不明".to_string(),
        }
    }

    /// 経過時間（秒）。
    pub fn age_seconds(&self) -> i64 {
        now_epoch().saturating_sub(self.acquired_at)
    }
}

fn now_epoch() -> i64 {
    chrono::Utc::now().timestamp()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bootstrap() -> Bootstrap {
        Bootstrap {
            bdstoken: Some("bd".into()),
            js_token: Some("js".into()),
            pcf_token: None,
            uk: Some("42".into()),
            logged_in: true,
        }
    }

    #[test]
    fn debug_never_leaks_secret_values() {
        let s = Session::from_bootstrap("ndus-secret-value", &bootstrap(), "UA").unwrap();
        let text = format!("{s:?}");
        assert!(!text.contains("ndus-secret-value"));
        assert!(!text.contains("\"js\""));
        assert!(!text.contains("\"bd\""));
        assert!(text.contains("redacted"));
    }

    #[test]
    fn can_write_requires_bdstoken() {
        let s = Session::from_bootstrap("n", &bootstrap(), "UA").unwrap();
        assert!(s.can_write());

        let mut anon = bootstrap();
        anon.bdstoken = None;
        let s2 = Session::from_bootstrap("n", &anon, "UA").unwrap();
        assert!(!s2.can_write());
    }

    #[test]
    fn refresh_updates_token_and_keeps_uk() {
        let mut s = Session::from_bootstrap("n", &bootstrap(), "UA").unwrap();
        let mut fresh = Bootstrap {
            bdstoken: Some("new".into()),
            js_token: Some("js2".into()),
            pcf_token: None,
            uk: Some("0".into()),
            logged_in: false,
        };
        s.refresh_from(&fresh);
        assert_eq!(s.js_token, "js2");
        assert_eq!(s.bdstoken.as_deref(), Some("new"));
        assert_eq!(s.uk.as_deref(), Some("42"));

        fresh.uk = Some("99".into());
        s.refresh_from(&fresh);
        assert_eq!(s.uk.as_deref(), Some("99"));
    }
}
