//! ブラウザを使ったログイン。
//!
//! 手順:
//! 1. Chrome か Edge を、専用のユーザーデータディレクトリ + リモートデバッグ
//!    ポート付きで起動する。既存プロファイルには触れない。
//! 2. TeraBox のログイン画面を開く。
//! 3. 利用者が通常の手順でログインする。
//! 4. CDP で `ndus` Cookie を読み取る。
//! 5. 同じ Cookie で `/main` を取得し、`jsToken` / `bdstoken` を得る。
//! 6. ブラウザを閉じる。
//!
//! ブラウザが見つからない、または CDP に繋がらない場合は、Cookie 貼り付けへ
//! 誘導する（`--paste`）。推測でログイン済み扱いにはしない。

use std::path::PathBuf;
use std::time::Duration;

use terabox_core::bootstrap::Bootstrap;
use terabox_core::{Session, DEFAULT_UA};

use crate::cdp::{self, CdpSession};

/// ログイン画面の URL。
pub const LOGIN_URL: &str = "https://www.terabox.com/main";

/// ブラウザ起動の結果。
pub enum BrowserLogin {
    /// Cookie を取得できた。
    Success(Session),
    /// ブラウザは開いたがログインが確認できなかった。
    NotLoggedIn,
    /// ブラウザを起動できなかった（理由付き）。貼り付けへ誘導する。
    Unavailable(String),
}

/// 既定の待ち時間。
///
/// 利用者がログインを終えるまでポーリングする。長すぎても短すぎても
/// 使い勝手が悪いため、5 分を上限にする。
const LOGIN_TIMEOUT: Duration = Duration::from_secs(300);

/// ブラウザを起動してログインを待つ。
pub async fn login_with_browser(profile_dir: PathBuf, port: u16) -> BrowserLogin {
    let browser = match find_browser() {
        Some(b) => b,
        None => {
            return BrowserLogin::Unavailable(
                "Chrome / Edge が見つかりませんでした。`tbx login --paste` で Cookie を貼り付けてください。"
                    .into(),
            )
        }
    };

    if let Err(e) = std::fs::create_dir_all(&profile_dir) {
        return BrowserLogin::Unavailable(format!(
            "ブラウザ用ディレクトリを作れませんでした（{}）: {e}",
            profile_dir.display()
        ));
    }

    // 子プロセスを保持しておき、終了時に落とす。
    let mut child = match std::process::Command::new(&browser.path)
        .arg(format!("--remote-debugging-port={port}"))
        .arg(format!("--user-data-dir={}", profile_dir.display()))
        .arg("--no-first-run")
        .arg("--no-default-browser-check")
        .arg("--disable-features=Translate")
        .arg(LOGIN_URL)
        .spawn()
    {
        Ok(c) => c,
        Err(e) => {
            return BrowserLogin::Unavailable(format!(
                "ブラウザを起動できませんでした（{}）: {e}",
                browser.path.display()
            ))
        }
    };

    println!("ブラウザでログインしてください。完了を検出すると自動で閉じます。");
    println!("（5 分以内にログインしない場合は終了します）");

    let ready = cdp::wait_for_port(port, Duration::from_secs(30)).await;
    if !ready {
        let _ = child.kill();
        return BrowserLogin::Unavailable(format!(
            "ブラウザのデバッグポート {port} に接続できませんでした。別のアプリが使用している可能性があります。"
        ));
    }

    let deadline = tokio::time::Instant::now() + LOGIN_TIMEOUT;
    let mut session: Option<Session> = None;

    while tokio::time::Instant::now() < deadline {
        if let Ok(mut cdp_session) = CdpSession::connect(port).await {
            match cdp_session.find_cookie("ndus").await {
                Ok(Some(ndus)) if !ndus.is_empty() => {
                    // Cookie が取れたら、その Cookie で初期化ページを読み直す。
                    match fetch_bootstrap(&ndus, DEFAULT_UA).await {
                        Ok(bootstrap) if bootstrap.logged_in => {
                            if let Some(s) = Session::from_bootstrap(&ndus, &bootstrap, DEFAULT_UA) {
                                session = Some(s);
                                break;
                            }
                        }
                        Ok(_) => {
                            // Cookie はあるが bdstoken がまだ無い = ログイン処理中。
                        }
                        Err(e) => {
                            eprintln!("初期化ページの取得に失敗しました（継続します）: {e}");
                        }
                    }
                }
                _ => {}
            }
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
    }

    // ブラウザを閉じる。利用者の環境を残さない。
    let _ = child.kill();
    let _ = child.wait();

    match session {
        Some(s) => BrowserLogin::Success(s),
        None => {
            if tokio::time::Instant::now() >= deadline {
                BrowserLogin::Unavailable(
                    "5 分以内にログインを確認できませんでした。`tbx login --paste` をお試しください。"
                        .into(),
                )
            } else {
                BrowserLogin::NotLoggedIn
            }
        }
    }
}

/// Cookie から `/main` を取得してトークンを取り出す。
pub async fn fetch_bootstrap(ndus: &str, user_agent: &str) -> Result<Bootstrap, String> {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(30))
        .build()
        .map_err(|e| format!("HTTP クライアントを作れませんでした: {e}"))?;
    let resp = client
        .get(LOGIN_URL)
        .header(reqwest::header::USER_AGENT, user_agent)
        .header(
            reqwest::header::COOKIE,
            format!("ndus={ndus}; PANWEB=1"),
        )
        .send()
        .await
        .map_err(|e| format!("/main の取得に失敗しました: {e}"))?;
    let status = resp.status();
    let body = resp
        .text()
        .await
        .map_err(|e| format!("/main の本文を読めませんでした: {e}"))?;
    if !status.is_success() {
        return Err(format!("/main が HTTP {} を返しました", status.as_u16()));
    }
    Bootstrap::parse(&body).map_err(|e| e.to_string())
}

/// 見つかったブラウザ。
pub struct Browser {
    pub path: PathBuf,
    pub name: &'static str,
}

/// インストール済みのブラウザを探す。
///
/// PATH と既知のインストール先だけを見る。ディスク全体の探索はしない。
/// CDP（`--remote-debugging-port`）を話せるのは Chrome 系だけなので、
/// Firefox などは候補に入れない。
pub fn find_browser() -> Option<Browser> {
    let candidates: Vec<(PathBuf, &'static str)> = {
        let mut v: Vec<(PathBuf, &'static str)> = Vec::new();
        #[cfg(windows)]
        {
            if let Ok(p) = std::env::var("PROGRAMFILES") {
                v.push((PathBuf::from(&p).join(r"Google\Chrome\Application\chrome.exe"), "Chrome"));
                v.push((PathBuf::from(&p).join(r"Microsoft\Edge\Application\msedge.exe"), "Edge"));
            }
            if let Ok(p) = std::env::var("PROGRAMFILES(X86)") {
                v.push((PathBuf::from(&p).join(r"Google\Chrome\Application\chrome.exe"), "Chrome"));
                v.push((PathBuf::from(&p).join(r"Microsoft\Edge\Application\msedge.exe"), "Edge"));
            }
            if let Ok(p) = std::env::var("LOCALAPPDATA") {
                v.push((PathBuf::from(&p).join(r"Google\Chrome\Application\chrome.exe"), "Chrome"));
            }
        }
        #[cfg(target_os = "macos")]
        {
            v.push((
                PathBuf::from("/Applications/Google Chrome.app/Contents/MacOS/Google Chrome"),
                "Chrome",
            ));
            v.push((
                PathBuf::from("/Applications/Microsoft Edge.app/Contents/MacOS/Microsoft Edge"),
                "Edge",
            ));
            v.push((
                PathBuf::from("/Applications/Chromium.app/Contents/MacOS/Chromium"),
                "Chromium",
            ));
        }
        v
    };

    for (path, name) in candidates {
        if crate::which::is_executable(&path) {
            return Some(Browser { path, name });
        }
    }

    // PATH 上の実行名も見る。Linux のディストリごとに名前が違うため、
    // 代表的なものを順に試す。
    let commands: &[(&str, &str)] = if cfg!(windows) {
        &[("chrome", "Chrome"), ("msedge", "Edge")]
    } else {
        &[
            ("google-chrome", "Chrome"),
            ("chromium", "Chromium"),
            ("chromium-browser", "Chromium"),
            ("brave-browser", "Brave"),
            ("microsoft-edge", "Edge"),
        ]
    };
    for (cmd, name) in commands {
        if let Some(found) = crate::which::which(cmd) {
            return Some(Browser { path: found, name });
        }
    }
    None
}

/// 貼り付けログイン用の入力を受け取ってセッションを作る。
///
/// `ndus` は必須。`jsToken` は省略できる（`/main` から取り直す）。
pub async fn login_with_paste(ndus: String, js_token: Option<String>) -> Result<Session, String> {
    let ndus = ndus.trim().to_string();
    if ndus.is_empty() {
        return Err("ndus が空です".into());
    }
    let mut bootstrap = fetch_bootstrap(&ndus, DEFAULT_UA).await?;
    if let Some(token) = js_token {
        let token = token.trim().to_string();
        if !token.is_empty() {
            bootstrap.js_token = Some(token);
        }
    }
    if !bootstrap.logged_in {
        return Err(
            "ndus は受け取りましたが、ログイン済みと確認できませんでした。ブラウザで TeraBox にログインした状態で ndus を取り直してください。"
                .into(),
        );
    }
    Session::from_bootstrap(&ndus, &bootstrap, DEFAULT_UA)
        .ok_or_else(|| "セッションを組み立てられませんでした（jsToken が取得できません）".to_string())
}
