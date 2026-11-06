//! 初期化ページ（`https://www.terabox.com/main`）からセッション用トークンを取り出す。
//!
//! 実測（2026-11-06、APK 4.28.2 解析時）:
//! - 未ログインでも `/main` は約 775KB の HTML を返す。
//! - HTML 中に `var templateData = {...};` があり、`bdstoken` / `pcftoken` /
//!   `uk` を含む。
//! - 同 HTML に
//!   ``decodeURIComponent(`function%20fn%28a%29%7Bwindow.jsToken%20%3D%20a%7D%3Bfn%28%22<192桁hex>%22%29`)``
//!   が埋め込まれており、JS を実行せずに jsToken を取り出せる。
//!
//! この 2 点に依存する。見つからない場合は推測で埋めずに
//! `TbxError::BootstrapFieldMissing` を返す。

use crate::error::{ApiError, TbxError};
use crate::percent;
use serde::{Deserialize, Serialize};

/// `/main` から取得した起動情報。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Bootstrap {
    /// 書き込み系 API に必要なトークン。未ログインでは空文字。
    pub bdstoken: Option<String>,
    /// 読み取り系 API の `jsToken`。192 桁 hex を実測。
    pub js_token: Option<String>,
    /// 端末識別用の `pcftoken`。
    pub pcf_token: Option<String>,
    /// ユーザー ID。未ログインでは 0。
    pub uk: Option<String>,
    /// ログイン済みと判断できるか（`uk` が 0 以外、かつ bdstoken が空でない）。
    pub logged_in: bool,
}

impl Bootstrap {
    /// HTML から起動情報を取り出す。
    ///
    /// `html` は `/main` の応答本文。取り出せなかった項目は `None` のまま返し、
    /// `js_token` だけは必須として扱う（無いと全 API が通らないため）。
    pub fn parse(html: &str) -> Result<Self, TbxError> {
        let template = parse_template_data(html);
        let js_token = parse_js_token(html)
            .ok_or(TbxError::BootstrapFieldMissing { field: "jsToken" })?;

        let uk = template
            .as_ref()
            .and_then(|v| v.get("uk"))
            .and_then(|v| match v {
                serde_json::Value::Number(n) => Some(n.to_string()),
                serde_json::Value::String(s) => Some(s.clone()),
                _ => None,
            });

        let bdstoken = template
            .as_ref()
            .and_then(|v| v.get("bdstoken"))
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .map(|s| s.to_string());

        let pcf_token = template
            .as_ref()
            .and_then(|v| v.get("pcftoken"))
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .map(|s| s.to_string());

        let logged_in = bdstoken.is_some() && uk.as_deref().map(|u| u != "0").unwrap_or(false);

        Ok(Self {
            bdstoken,
            js_token: Some(js_token),
            pcf_token,
            uk,
            logged_in,
        })
    }

    /// 未ログインを表す業務エラーを作る。
    pub fn require_login(&self) -> Result<(), TbxError> {
        if self.logged_in {
            Ok(())
        } else {
            Err(TbxError::Api(ApiError {
                endpoint: "/main (bootstrap)".into(),
                errno: -6,
                message: Some("user not login".into()),
                request_id: None,
            }))
        }
    }
}

/// `var templateData = { ... };` を取り出して JSON として読む。
fn parse_template_data(html: &str) -> Option<serde_json::Value> {
    const NEEDLE: &str = "var templateData";
    let start = html.find(NEEDLE)?;
    let after = &html[start + NEEDLE.len()..];
    let eq = after.find('=')?;
    let rest = &after[eq + 1..];
    // 先頭の空白を飛ばして `{` を探す。
    let brace = rest.find('{')?;
    let body = &rest[brace..];
    // 波括弧の対応を数えて終端を決める。文字列リテラル内の括弧は数えない。
    let mut depth = 0i32;
    let mut in_string = false;
    let mut escape = false;
    let bytes = body.as_bytes();
    let mut end = None;
    for (i, &b) in bytes.iter().enumerate() {
        if in_string {
            if escape {
                escape = false;
            } else if b == b'\\' {
                escape = true;
            } else if b == b'"' {
                in_string = false;
            }
            continue;
        }
        match b {
            b'"' => in_string = true,
            b'{' => depth += 1,
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    end = Some(i + 1);
                    break;
                }
            }
            _ => {}
        }
    }
    let end = end?;
    serde_json::from_str(&body[..end]).ok()
}

/// `decodeURIComponent(...)` の中の `fn("<hex>")` を拾う。
fn parse_js_token(html: &str) -> Option<String> {
    const NEEDLE: &str = "decodeURIComponent";
    let mut search_from = 0usize;
    while let Some(rel) = html[search_from..].find(NEEDLE) {
        let idx = search_from + rel;
        let tail = &html[idx..];
        // バッククォートで囲まれた引数を取る。
        if let Some(open) = tail.find('`') {
            if let Some(close_rel) = tail[open + 1..].find('`') {
                let raw = &tail[open + 1..open + 1 + close_rel];
                let decoded = percent::decode(raw);
                if let Some(token) = extract_fn_argument(&decoded) {
                    return Some(token);
                }
            }
        }
        search_from = idx + NEEDLE.len();
    }
    None
}

/// `fn("<hex>")` の引数を取り出す。長さは問わない（形式変化を検知できるように
/// 呼び出し側で検査する）。
fn extract_fn_argument(decoded: &str) -> Option<String> {
    let pos = decoded.find("fn(")?;
    let after = &decoded[pos + 3..];
    let quote_rel = after.find(|c| c == '"' || c == '\'')?;
    let quote = after.as_bytes()[quote_rel] as char;
    let inner = &after[quote_rel + 1..];
    let close = inner.find(quote)?;
    let token = &inner[..close];
    if token.is_empty() {
        None
    } else {
        Some(token.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 実 HTML の該当部分を再現した合成フィクスチャ（実データのコピーではない）。
    const SAMPLE: &str = r#"<script>var templateData = {"bdstoken":"","pcftoken":"17404426959ca97d8ea452dfca37b03a","userVipIdentity":0,"uk":0};</script><script>try { eval(decodeURIComponent(`function%20fn%28a%29%7Bwindow.jsToken%20%3D%20a%7D%3Bfn%28%22AABBCCDD%22%29`)) } catch(ex) {console.log(ex);}</script>"#;

    const LOGGED_IN: &str = r#"<script>var templateData = {"bdstoken":"tok123","pcftoken":"pcf","uk":424242};</script><script>eval(decodeURIComponent(`fn%28%22DEADBEEF%22%29`))</script>"#;

    #[test]
    fn parses_anonymous_bootstrap() {
        let b = Bootstrap::parse(SAMPLE).unwrap();
        assert_eq!(b.js_token.as_deref(), Some("AABBCCDD"));
        assert_eq!(b.bdstoken, None);
        assert_eq!(b.uk.as_deref(), Some("0"));
        assert!(!b.logged_in);
        assert!(b.require_login().is_err());
    }

    #[test]
    fn parses_logged_in_bootstrap() {
        let b = Bootstrap::parse(LOGGED_IN).unwrap();
        assert_eq!(b.js_token.as_deref(), Some("DEADBEEF"));
        assert_eq!(b.bdstoken.as_deref(), Some("tok123"));
        assert_eq!(b.uk.as_deref(), Some("424242"));
        assert!(b.logged_in);
        assert!(b.require_login().is_ok());
    }

    #[test]
    fn missing_js_token_is_explicit_error() {
        let err = Bootstrap::parse("<html>no token here</html>").unwrap_err();
        match err {
            TbxError::BootstrapFieldMissing { field } => assert_eq!(field, "jsToken"),
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn template_data_handles_braces_in_strings() {
        let html = r#"var templateData = {"note":"a{b}c","uk":7};x"#;
        let v = parse_template_data(html).unwrap();
        assert_eq!(v.get("uk").unwrap().as_i64(), Some(7));
        assert_eq!(v.get("note").unwrap().as_str(), Some("a{b}c"));
    }

    #[test]
    fn template_data_absent_is_none() {
        assert!(parse_template_data("<html></html>").is_none());
    }
}
