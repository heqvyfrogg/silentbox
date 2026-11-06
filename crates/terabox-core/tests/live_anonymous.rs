//! 実ネットワークに対する検証（既定では実行しない）。
//!
//! `cargo test --ignored` で明示的に走らせる。認証不要で確認できる範囲だけを
//! 対象にし、認証が必要な経路はここでは扱わない（未検証を成功扱いにしない）。
//!
//! 実行例:
//!   cargo test -p terabox-core --test live_anonymous -- --ignored --nocapture

use terabox_core::api::ApiClient;
use terabox_core::bootstrap::Bootstrap;
use terabox_core::session::Session;
use terabox_core::TbxError;

fn anon_session() -> Session {
    Session {
        ndus: String::new(),
        js_token: String::new(),
        bdstoken: None,
        uk: None,
        user_agent: terabox_core::DEFAULT_UA.to_string(),
        acquired_at: chrono::Utc::now().timestamp(),
    }
}

/// `/main` を取得し、jsToken が 192 桁の hex として取り出せることを確認する。
#[tokio::test]
#[ignore = "実ネットワークへ接続する"]
async fn bootstrap_from_live_main_page() {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .build()
        .expect("client");
    let resp = client
        .get("https://www.terabox.com/main")
        .header(reqwest::header::USER_AGENT, terabox_core::DEFAULT_UA)
        .send()
        .await
        .expect("GET /main");
    assert!(
        resp.status().is_success(),
        "/main が {} を返しました",
        resp.status()
    );
    let body = resp.text().await.expect("body");
    println!("/main の本文長: {} バイト", body.len());

    let bootstrap = Bootstrap::parse(&body).expect("/main からトークンを取り出せること");
    let token = bootstrap.js_token.clone().expect("jsToken");
    println!("jsToken の長さ: {}", token.len());
    assert_eq!(
        token.len(),
        192,
        "jsToken の長さが想定（192）と違います。HTML 構造の変更を疑ってください"
    );
    assert!(
        token.chars().all(|c| c.is_ascii_hexdigit()),
        "jsToken が 16 進文字列ではありません"
    );
    assert!(
        !bootstrap.logged_in,
        "匿名アクセスがログイン済みと判定されました"
    );
}

/// 未ログインで `/api/list` を叩くと `errno -6` が返ることを確認する。
#[tokio::test]
#[ignore = "実ネットワークへ接続する"]
async fn list_without_login_returns_minus_six() {
    let api = ApiClient::new(anon_session()).expect("client");
    match api.list("/", 1, 10, "name", false).await {
        Err(TbxError::Api(err)) => {
            println!("errno = {} / message = {:?}", err.errno, err.message);
            assert_eq!(err.errno, -6, "未ログインの errno が -6 ではありません");
            assert!(err.is_not_logged_in());
        }
        other => panic!("未ログインで成功してしまいました: {other:?}"),
    }
}

/// 不正な共有リンクは業務エラーとして返ることを確認する。
///
/// サーバは入力によって `errno` を変える（実測: `1test` は 460030 "dragdrop"、
/// `1zzzzzzzzz` は 105 "share link error stat"）。特定の値へ固定すると
/// サーバ側の割り当て変更で壊れるため、ここでは「0 以外の errno で失敗する」
/// ことだけを検証し、実際の値は表示する。
#[tokio::test]
#[ignore = "実ネットワークへ接続する"]
async fn invalid_share_link_reports_error() {
    let api = ApiClient::new(anon_session()).expect("client");
    match api.shorturl_info("https://www.terabox.com/s/1test").await {
        Err(TbxError::Api(err)) => {
            println!(
                "errno = {} / message = {:?} / display = {}",
                err.errno,
                err.message,
                err.display_message()
            );
            assert_ne!(err.errno, 0, "errno 0（成功）がエラーとして返されました");
            assert!(!err.is_not_logged_in(), "共有リンク不正が未ログイン扱いになっています");
        }
        other => panic!("不正な共有リンクで成功してしまいました: {other:?}"),
    }
}
