//! ブートストラップ解析の回帰試験。
//!
//! 実 HTML の該当部分を縮めた **合成フィクスチャ** を使う。実データのコピーでは
//! ないため、サーバ側の細部が変わってもここは壊れない。壊れるべきなのは
//! 「トークンの埋め込み方が変わったとき」だけ。

use terabox_core::bootstrap::Bootstrap;
use terabox_core::session::Session;

/// 匿名アクセス時の構造（`bdstoken` が空、`uk` が 0）。
///
/// トークンは 192 桁の hex（`AABBCCDDEEFF0011` を 12 回）。長さだけを検証する。
const ANONYMOUS: &str = r#"
<html><head><script>
var templateData = {"bdstoken":"","pcftoken":"17404426959ca97d8ea452dfca37b03a","userVipIdentity":0,"uk":0,"internal":false};
</script>
<script>try { eval(decodeURIComponent(`function%20fn%28a%29%7Bwindow.jsToken%20%3D%20a%7D%3Bfn%28%22AABBCCDDEEFF0011AABBCCDDEEFF0011AABBCCDDEEFF0011AABBCCDDEEFF0011AABBCCDDEEFF0011AABBCCDDEEFF0011AABBCCDDEEFF0011AABBCCDDEEFF0011AABBCCDDEEFF0011AABBCCDDEEFF0011AABBCCDDEEFF0011AABBCCDDEEFF0011%22%29`)) } catch(ex) {console.log(ex);}</script>
</head><body></body></html>
"#;

/// ログイン済みの構造。
const LOGGED_IN: &str = r#"
<html><head><script>
var templateData = {"bdstoken":"bd-token-value","pcftoken":"pcf","uk":424242,"userVipIdentity":1};
</script>
<script>eval(decodeURIComponent(`fn%28%22DEADBEEF%22%29`))</script>
</head><body></body></html>
"#;

#[test]
fn anonymous_page_yields_token_but_not_logged_in() {
    let b = Bootstrap::parse(ANONYMOUS).expect("解析できること");
    assert_eq!(b.js_token.as_deref().map(|s| s.len()), Some(192));
    assert_eq!(b.bdstoken, None);
    assert_eq!(b.uk.as_deref(), Some("0"));
    assert!(!b.logged_in);
}

#[test]
fn logged_in_page_yields_session() {
    let b = Bootstrap::parse(LOGGED_IN).expect("解析できること");
    assert_eq!(b.js_token.as_deref(), Some("DEADBEEF"));
    assert_eq!(b.bdstoken.as_deref(), Some("bd-token-value"));
    assert_eq!(b.uk.as_deref(), Some("424242"));
    assert!(b.logged_in);

    let s = Session::from_bootstrap("ndus-value", &b, terabox_core::DEFAULT_UA)
        .expect("セッションを作れること");
    assert!(s.can_write());
    assert_eq!(s.account_label(), "uk=424242");
}

#[test]
fn missing_token_is_reported_not_guessed() {
    let err = Bootstrap::parse("<html>nothing</html>").unwrap_err();
    let text = err.to_string();
    assert!(
        text.contains("jsToken"),
        "どのフィールドが欠けたか示す必要があります: {text}"
    );
}

#[test]
fn percent_encoded_payload_is_decoded() {
    // `fn%28%22X%22%29` は `fn("X")`。%XX を復元できないと取り出せない。
    let html = r#"<script>decodeURIComponent(`fn%28%22TOKEN123%22%29`)</script>"#;
    let b = Bootstrap::parse(html).expect("解析できること");
    assert_eq!(b.js_token.as_deref(), Some("TOKEN123"));
}

#[test]
fn template_data_with_braces_in_strings_is_parsed() {
    // 中括弧を含む文字列リテラルがあっても、JSON の終端を誤らないこと。
    let html = r#"<script>var templateData = {"note":"a{b}c","uk":7};</script><script>decodeURIComponent(`fn%28%22X%22%29`)</script>"#;
    let b = Bootstrap::parse(html).expect("解析できること");
    assert_eq!(b.uk.as_deref(), Some("7"));
    assert_eq!(b.js_token.as_deref(), Some("X"));
}

#[test]
fn token_is_hex_and_full_length() {
    let b = Bootstrap::parse(ANONYMOUS).expect("解析できること");
    let token = b.js_token.expect("jsToken");
    assert_eq!(token.len(), 192, "実測した長さと違います");
    assert!(
        token.chars().all(|c| c.is_ascii_hexdigit()),
        "16 進以外の文字が混じっています"
    );
}
