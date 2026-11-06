//! TeraBox の HTTP API クライアント。
//!
//! ## 使うエンドポイントの根拠
//! すべて同梱 APK（`com.dubox.drive` 4.28.2 / versionCode 707）の DEX 文字列
//! テーブルから実在を確認したもの。詳細は `docs/protocol.md` を参照。
//!
//! | 用途 | パス |
//! |---|---|
//! | 一覧 | `/api/list` |
//! | メタ | `/api/filemetas` |
//! | 容量 | `/api/quota` |
//! | ユーザー | `/api/user/getinfo` |
//! | 共有情報 | `/api/shorturlinfo` |
//! | 共有一覧 | `/share/list` |
//! | 共有保存 | `/share/transfer` |
//! | DL リンク | `/rest/2.0/pcs/file?method=download` |
//! | UL 受付 | `/precreate` 系（`/api/precreate`）|
//! | UL 分割 | `/rest/2.0/pcs/superfile2?method=upload` |
//! | ゴミ箱 | `/api/recycle/{list,restore,delete,clear}` |
//!
//! ## 共通パラメータ
//! `app_id=250528&web=1&channel=dubox&clienttype=0&jsToken=<token>` と
//! Cookie `ndus=<...>; PANWEB=1`。UA は dlink 取得と実 DL で一致させる。
//!
//! ## 未検証の範囲
//! 認証済みでの実応答は初回ログイン後に確定させる。レスポンスのフィールド名は
//! 実測ではなく APK 内のモデル定義（`PreCreateDataBean`, `QuotaInfo` 等）と
//! 既知の Web API 仕様に基づく。値が無い場合は `None` を返し、推測で埋めない。

use std::sync::Arc;

use reqwest::header::{HeaderMap, HeaderValue, COOKIE, REFERER, USER_AGENT};
use reqwest::{Client, Response, StatusCode};
use serde::de::DeserializeOwned;
use serde_json::Value;
use tokio::sync::Mutex;

use crate::bootstrap::Bootstrap;
use crate::error::{ApiError, TbxError};
use crate::models::{FsEntry, ListResponse, QuotaInfo, ShareInfo};
use crate::session::Session;
use crate::{APP_ID_WEB, PANWEB};

/// API の基底 URL。
pub const BASE: &str = "https://www.terabox.com";

/// API クライアント。
///
/// セッションは `Mutex` で保持し、`jsToken` の再取得を 1 箇所に閉じ込める。
#[derive(Clone)]
pub struct ApiClient {
    http: Client,
    session: Arc<Mutex<Session>>,
    base: String,
    /// jsToken 再取得の同時実行を防ぐ。
    refresh_lock: Arc<Mutex<()>>,
    /// アップロード先 PCS ホストのキャッシュ（ホスト名と取得時刻）。
    upload_host_cache: Arc<Mutex<Option<(String, std::time::Instant)>>>,
}

impl ApiClient {
    /// セッションからクライアントを作る。
    pub fn new(session: Session) -> Result<Self, TbxError> {
        Self::with_base(session, BASE)
    }

    /// 基底 URL を差し替えて作る（テスト用スタブ向け）。
    pub fn with_base(session: Session, base: impl Into<String>) -> Result<Self, TbxError> {
        let mut headers = HeaderMap::new();
        headers.insert(
            USER_AGENT,
            HeaderValue::from_str(&session.user_agent)
                .map_err(|e| TbxError::Other(format!("User-Agent が不正です: {e}")))?,
        );
        headers.insert(
            REFERER,
            HeaderValue::from_static("https://www.terabox.com/main?category=all"),
        );
        let http = Client::builder()
            .default_headers(headers)
            .redirect(reqwest::redirect::Policy::none())
            .timeout(std::time::Duration::from_secs(60))
            .build()?;
        Ok(Self {
            http,
            session: Arc::new(Mutex::new(session)),
            base: base.into(),
            refresh_lock: Arc::new(Mutex::new(())),
            upload_host_cache: Arc::new(Mutex::new(None)),
        })
    }

    /// 現在のセッションの複製を返す。
    pub async fn session(&self) -> Session {
        self.session.lock().await.clone()
    }

    /// セッションを差し替える（再ログイン後）。
    pub async fn set_session(&self, session: Session) {
        *self.session.lock().await = session;
    }

    fn cookie_header(session: &Session) -> String {
        format!("ndus={}; PANWEB={}", session.ndus, PANWEB)
    }

    /// 共通クエリを組み立てる。
    fn common_params(session: &Session) -> Vec<(&'static str, String)> {
        vec![
            ("app_id", APP_ID_WEB.to_string()),
            ("web", "1".to_string()),
            ("channel", "dubox".to_string()),
            ("clienttype", "0".to_string()),
            ("jsToken", session.js_token.clone()),
        ]
    }

    /// GET して JSON を返す。HTTP エラーは `TbxError::Other` にせず、
    /// 状態コードと本文先頭を保った業務エラーにする。
    async fn get_json<T: DeserializeOwned>(
        &self,
        path: &str,
        mut params: Vec<(String, String)>,
    ) -> Result<T, TbxError> {
        let session = self.session().await;
        for (k, v) in Self::common_params(&session) {
            if !params.iter().any(|(pk, _)| pk == k) {
                params.push((k.to_string(), v));
            }
        }
        let url = format!("{}{}", self.base, path);
        let resp = self
            .http
            .get(&url)
            .query(&params)
            .header(COOKIE, Self::cookie_header(&session))
            .send()
            .await?;
        Self::parse_json::<T>(resp, path).await
    }

    /// POST（フォーム）して JSON を返す。書き込み系で使う。
    async fn post_form_json<T: DeserializeOwned>(
        &self,
        path: &str,
        mut params: Vec<(String, String)>,
        form: Vec<(String, String)>,
    ) -> Result<T, TbxError> {
        let session = self.session().await;
        for (k, v) in Self::common_params(&session) {
            if !params.iter().any(|(pk, _)| pk == k) {
                params.push((k.to_string(), v));
            }
        }
        if let Some(token) = &session.bdstoken {
            params.push(("bdstoken".to_string(), token.clone()));
        }
        let url = format!("{}{}", self.base, path);
        let resp = self
            .http
            .post(&url)
            .query(&params)
            .header(COOKIE, Self::cookie_header(&session))
            .form(&form)
            .send()
            .await?;
        Self::parse_json::<T>(resp, path).await
    }

    async fn parse_json<T: DeserializeOwned>(resp: Response, path: &str) -> Result<T, TbxError> {
        let status = resp.status();
        let body = resp.text().await?;
        if status == StatusCode::UNAUTHORIZED || status == StatusCode::FORBIDDEN {
            return Err(TbxError::Api(ApiError {
                endpoint: path.to_string(),
                errno: -6,
                message: Some(format!("HTTP {}", status.as_u16())),
                request_id: None,
            }));
        }
        if !status.is_success() {
            return Err(TbxError::Other(format!(
                "{path} が HTTP {} を返しました（本文先頭: {}）",
                status.as_u16(),
                head(&body, 200)
            )));
        }
        let value: Value = serde_json::from_str(&body).map_err(|e| {
            TbxError::Other(format!(
                "{path} の応答を JSON として読めません: {e}（本文先頭: {}）",
                head(&body, 200)
            ))
        })?;
        // errno が 0 以外なら業務エラーとして返す。
        if let Some(errno) = value.get("errno").and_then(|v| v.as_i64()) {
            if errno != 0 {
                return Err(TbxError::Api(ApiError {
                    endpoint: path.to_string(),
                    errno,
                    message: value
                        .get("errmsg")
                        .or_else(|| value.get("show_msg"))
                        .and_then(|v| v.as_str())
                        .map(|s| s.to_string()),
                    request_id: value.get("request_id").map(|v| v.to_string()),
                }));
            }
        }
        serde_json::from_value(value).map_err(TbxError::from)
    }

    // ---- 読み取り系 ----

    /// ディレクトリ一覧。`dir` は `/` 始まりの絶対パス。
    pub async fn list(
        &self,
        dir: &str,
        page: u32,
        num: u32,
        order: &str,
        desc: bool,
    ) -> Result<ListResponse, TbxError> {
        let params = vec![
            ("dir".to_string(), dir.to_string()),
            ("order".to_string(), order.to_string()),
            ("desc".to_string(), if desc { "1" } else { "0" }.to_string()),
            ("showempty".to_string(), "1".to_string()),
            ("page".to_string(), page.to_string()),
            ("num".to_string(), num.to_string()),
            ("startLogTime".to_string(), "0".to_string()),
        ];
        let resp: ListResponse = self.get_json("/api/list", params.clone()).await?;
        self.maybe_refresh(format!("{}{}", self.base, "/api/list")).await?;
        Ok(resp)
    }

    /// `fs_id` 群からメタ情報をまとめて取得する。
    pub async fn file_metas(&self, fs_ids: &[String]) -> Result<Vec<FsEntry>, TbxError> {
        let joined = fs_ids
            .iter()
            .map(|s| format!("\"{s}\""))
            .collect::<Vec<_>>()
            .join(",");
        let params = vec![
            ("target".to_string(), format!("[{joined}]")),
            ("dlink".to_string(), "0".to_string()),
            ("thumb".to_string(), "0".to_string()),
            ("extra".to_string(), "0".to_string()),
            ("fsids".to_string(), format!("[{joined}]")),
        ];
        let value: Value = self.get_json("/api/filemetas", params).await?;
        let info = value
            .get("info")
            .cloned()
            .unwrap_or(Value::Array(Vec::new()));
        Ok(serde_json::from_value(info).unwrap_or_default())
    }

    /// 容量情報。サーバが返さない項目は `None` のままにする。
    pub async fn quota(&self) -> Result<QuotaInfo, TbxError> {
        let params = vec![
            ("checkexpire".to_string(), "1".to_string()),
            ("checkfree".to_string(), "1".to_string()),
        ];
        let value: Value = self.get_json("/api/quota", params).await?;
        let mut info = QuotaInfo::default();

        // `/api/quota` は total/used を素直に返すことがある。
        info.total_bytes = pick_u64(&value, &["total", "quota", "total_quota"]);
        info.used_bytes = pick_u64(&value, &["used", "used_size", "quota_used"]);
        info.free_limit_bytes = pick_u64(&value, &["free", "free_quota"]);

        // 枠の内訳は `extra.init_quota_type`（例: permanent_30g_temp_994g）にある。
        if let Some(extra) = value.get("extra") {
            info.quota_type_raw = extra
                .get("init_quota_type")
                .and_then(|v| v.as_str())
                .filter(|s| !s.is_empty())
                .map(|s| s.to_string());
            info.temporary_quota_expires_at =
                pick_i64(extra, &["time_limit_quota_expire_time"]);
            if let Some(raw) = &info.quota_type_raw {
                let (permanent, temporary) = crate::models::parse_quota_type(raw);
                info.permanent_quota_bytes = permanent;
                info.temporary_quota_bytes = temporary;
            }
        }

        if info.total_bytes.is_none() {
            info.unavailable
                .push("総容量（サーバが値を返していません）".to_string());
        }
        if info.used_bytes.is_none() {
            info.unavailable
                .push("使用量（サーバが値を返していません）".to_string());
        }

        // VIP 情報は `/api/user/getinfo` 側にあるため、単独で取りに行く。
        match self.user_info().await {
            Ok(user) => {
                info.is_vip = pick_bool(&user, &["is_vip", "isVip", "vip"]);
                info.vip_kind = pick_string(&user, &["vip_type", "vipType", "member_type"]);
                if let Some(limit) = pick_u64(&user, &["max_file_size", "file_size_limit", "max_upload_size"]) {
                    info.max_file_size_bytes = Some(limit);
                }
                info.concurrent_download_allowed =
                    pick_bool(&user, &["concurrent_download", "multi_download"]);
                if info.total_bytes.is_none() {
                    info.total_bytes = pick_u64(&user, &["total_quota", "quota_total", "total"]);
                }
                if info.used_bytes.is_none() {
                    info.used_bytes = pick_u64(&user, &["used_quota", "quota_used", "used"]);
                }
            }
            Err(TbxError::Api(err)) if err.is_not_logged_in() => {
                info.unavailable
                    .push("VIP 状態（未ログインのため取得できません）".to_string());
            }
            Err(_) => {
                info.unavailable
                    .push("VIP 状態（取得に失敗しました）".to_string());
            }
        }

        if info.is_vip.is_none() {
            info.unavailable
                .push("VIP 状態（サーバが値を返していません）".to_string());
        }
        if info.max_file_size_bytes.is_none() {
            info.unavailable.push(
                "単一ファイル上限（サーバが明示値を返していません。プランにより変動するため、表示できる値だけを出しています）"
                    .to_string(),
            );
        }
        Ok(info)
    }

    /// `/api/user/getinfo` の生 JSON。フィールド名が環境で揺れるため `Value` で受ける。
    pub async fn user_info(&self) -> Result<Value, TbxError> {
        let value: Value = self.get_json("/api/user/getinfo", Vec::new()).await?;
        // `records[0]` に実体が入る形と、素直に生える形の両方を見る。
        if let Some(records) = value.get("records").and_then(|v| v.as_array()) {
            if let Some(first) = records.first() {
                return Ok(first.clone());
            }
        }
        Ok(value)
    }

    /// 共有リンクの情報。`shorturl` は `1` 始まりの ID（URL 全体でも可）。
    pub async fn shorturl_info(&self, shorturl: &str) -> Result<ShareInfo, TbxError> {
        let id = normalize_shorturl(shorturl);
        let params = vec![
            ("shorturl".to_string(), id.clone()),
            ("type".to_string(), "0".to_string()),
            ("root".to_string(), "1".to_string()),
        ];
        let value: Value = self.get_json("/api/shorturlinfo", params).await?;
        let mut info = ShareInfo {
            share_id: id,
            uk: pick_string(&value, &["uk", "share_uk"]).unwrap_or_default(),
            description: pick_string(&value, &["description", "desc", "title"]),
            expired: pick_bool(&value, &["expired", "is_expired"]),
            ..Default::default()
        };
        if let Some(list) = value.get("list").and_then(|v| v.as_array()) {
            info.entries = serde_json::from_value(Value::Array(list.clone())).unwrap_or_default();
        }
        Ok(info)
    }

    /// 共有フォルダの中身を列挙する。
    pub async fn share_list(
        &self,
        shorturl: &str,
        dir: &str,
        page: u32,
        num: u32,
    ) -> Result<Vec<FsEntry>, TbxError> {
        let id = normalize_shorturl(shorturl);
        let params = vec![
            ("shorturl".to_string(), id),
            ("dir".to_string(), dir.to_string()),
            ("root".to_string(), "1".to_string()),
            ("page".to_string(), page.to_string()),
            ("num".to_string(), num.to_string()),
            ("order".to_string(), "time".to_string()),
            ("desc".to_string(), "1".to_string()),
        ];
        let value: Value = self.get_json("/share/list", params).await?;
        let list = value
            .get("list")
            .cloned()
            .unwrap_or(Value::Array(Vec::new()));
        Ok(serde_json::from_value(list).unwrap_or_default())
    }

    /// 共有リンクの中身を自分のアカウントへ保存する。
    ///
    /// `fs_ids` が空なら「共有のルート全体」を意味する。
    pub async fn share_transfer(
        &self,
        shorturl: &str,
        fs_ids: &[String],
        dest: &str,
    ) -> Result<Value, TbxError> {
        let id = normalize_shorturl(shorturl);
        let params = vec![
            ("shareid".to_string(), id),
            ("from".to_string(), "1".to_string()),
            ("sekey".to_string(), String::new()),
            ("ondup".to_string(), "newcopy".to_string()),
        ];
        let form = vec![
            ("fsidlist".to_string(), format!("[{}]", fs_ids.iter().map(|s| format!("\"{s}\"")).collect::<Vec<_>>().join(","))),
            ("path".to_string(), dest.to_string()),
        ];
        self.post_form_json("/share/transfer", params, form).await
    }

    /// ダウンロードリンク（dlink）を取得する。
    ///
    /// 302 の `Location` を dlink として返す。リダイレクトは追わない
    /// （dlink 自体はホストが異なるため、呼び出し側が改めて取得する）。
    pub async fn dlink(&self, path: &str) -> Result<String, TbxError> {
        let session = self.session().await;
        let params = vec![
            ("method", "download".to_string()),
            ("app_id", APP_ID_WEB.to_string()),
            ("path", path.to_string()),
            ("jsToken", session.js_token.clone()),
            ("ec", "1".to_string()),
            ("check_blue", "1".to_string()),
        ];
        let url = format!("{}/rest/2.0/pcs/file", self.base);
        let resp = self
            .http
            .get(&url)
            .query(&params)
            .header(COOKIE, Self::cookie_header(&session))
            .send()
            .await?;

        let status = resp.status();
        if status.is_redirection() {
            if let Some(loc) = resp.headers().get(reqwest::header::LOCATION) {
                if let Ok(text) = loc.to_str() {
                    let absolute = if text.starts_with("http") {
                        text.to_string()
                    } else {
                        format!("{}{}", self.base, text)
                    };
                    return Ok(absolute);
                }
            }
            return Err(TbxError::Other(
                "dlink 応答が 302 でしたが Location を読めませんでした".to_string(),
            ));
        }

        // 302 でなく JSON を返す実装もあるため、両方を受ける。
        let body = resp.text().await?;
        if let Ok(value) = serde_json::from_str::<Value>(&body) {
            if let Some(errno) = value.get("errno").and_then(|v| v.as_i64()) {
                if errno != 0 {
                    return Err(TbxError::Api(ApiError {
                        endpoint: "/rest/2.0/pcs/file".into(),
                        errno,
                        message: value
                            .get("errmsg")
                            .and_then(|v| v.as_str())
                            .map(|s| s.to_string()),
                        request_id: value.get("request_id").map(|v| v.to_string()),
                    }));
                }
            }
            if let Some(link) = value
                .get("dlink")
                .or_else(|| value.get("dlink_list").and_then(|v| v.get(0)).and_then(|v| v.get("dlink")))
                .and_then(|v| v.as_str())
            {
                return Ok(link.to_string());
            }
        }
        Err(TbxError::Other(format!(
            "dlink を取得できませんでした（HTTP {}・本文先頭: {}）",
            status.as_u16(),
            head(&body, 200)
        )))
    }

    /// ゴミ箱の一覧。
    pub async fn recycle_list(&self, page: u32, num: u32) -> Result<Vec<FsEntry>, TbxError> {
        let params = vec![
            ("page".to_string(), page.to_string()),
            ("num".to_string(), num.to_string()),
            ("desc".to_string(), "1".to_string()),
            ("order".to_string(), "time".to_string()),
        ];
        let value: Value = self.get_json("/api/recycle/list", params).await?;
        let list = value
            .get("list")
            .cloned()
            .unwrap_or(Value::Array(Vec::new()));
        Ok(serde_json::from_value(list).unwrap_or_default())
    }

    /// ゴミ箱から復元する。
    ///
    /// `fidlist` は **数値の配列**（`[123]`）で送る。文字列の配列
    /// （`["123"]`）では `errno 2`（param err）になる（実測）。
    pub async fn recycle_restore(&self, fs_ids: &[String]) -> Result<Value, TbxError> {
        let form = vec![(
            "fidlist".to_string(),
            numeric_id_array(fs_ids)?,
        )];
        self.post_form_json("/api/recycle/restore", Vec::new(), form)
            .await
    }

    /// ゴミ箱から完全削除する。
    ///
    /// こちらも `fidlist` は数値の配列。
    pub async fn recycle_delete(&self, fs_ids: &[String]) -> Result<Value, TbxError> {
        let form = vec![(
            "fidlist".to_string(),
            numeric_id_array(fs_ids)?,
        )];
        self.post_form_json("/api/recycle/delete", Vec::new(), form)
            .await
    }

    /// ゴミ箱を空にする。取り消せない。
    pub async fn recycle_clear(&self) -> Result<Value, TbxError> {
        let params = vec![("async".to_string(), "1".to_string())];
        self.post_form_json("/api/recycle/clear", params, Vec::new())
            .await
    }

    /// 削除（ゴミ箱へ移動）。
    pub async fn delete(&self, paths: &[String]) -> Result<Value, TbxError> {
        let form = vec![(
            "filelist".to_string(),
            format!(
                "[{}]",
                paths
                    .iter()
                    .map(|p| format!("\"{}\"", escape_json(p)))
                    .collect::<Vec<_>>()
                    .join(",")
            ),
        )];
        self.post_form_json("/api/filemanager?opera=delete", Vec::new(), form)
            .await
    }

    /// フォルダ作成。
    pub async fn create_dir(&self, path: &str) -> Result<Value, TbxError> {
        let params = vec![
            ("a".to_string(), "commit".to_string()),
            ("path".to_string(), path.to_string()),
            ("isdir".to_string(), "1".to_string()),
            ("block_list".to_string(), "[]".to_string()),
        ];
        self.post_form_json("/api/create", params, Vec::new()).await
    }

    /// 名前変更。`to_name` は新しい名前だけ（ディレクトリ移動は含まない）。
    pub async fn rename(&self, from: &str, to_name: &str) -> Result<Value, TbxError> {
        let form = vec![
            (
                "filelist".to_string(),
                format!("[\"{}\"]", escape_json(from)),
            ),
            ("newname".to_string(), to_name.to_string()),
        ];
        self.post_form_json("/api/filemanager?opera=rename", Vec::new(), form)
            .await
    }

    // ---- アップロード ----

    /// アップロードの事前登録。返る `uploadid` を分割送信で使う。
    pub async fn precreate(
        &self,
        path: &str,
        size: u64,
        block_list_md5: &[String],
    ) -> Result<PrecreateResult, TbxError> {
        let block = format!(
            "[{}]",
            block_list_md5
                .iter()
                .map(|m| format!("\"{m}\""))
                .collect::<Vec<_>>()
                .join(",")
        );
        let params = vec![
            ("path".to_string(), path.to_string()),
            ("size".to_string(), size.to_string()),
            ("isdir".to_string(), "0".to_string()),
            ("autoinit".to_string(), "1".to_string()),
            ("block_list".to_string(), block),
            ("target_path".to_string(), path.to_string()),
        ];
        let value: Value = self.post_form_json("/api/precreate", params, Vec::new()).await?;

        // 既に同一内容がある場合は `return_type=2` で `info` が返り、送信不要。
        let return_type = value.get("return_type").and_then(|v| v.as_i64()).unwrap_or(0);
        Ok(PrecreateResult {
            upload_id: value
                .get("uploadid")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string()),
            return_type,
            raw: value,
        })
    }

    /// 分割パートを送る。`partseq` は 0 始まり。
    pub async fn upload_part(
        &self,
        upload_id: &str,
        path: &str,
        partseq: u32,
        offset: u64,
        bytes: bytes::Bytes,
    ) -> Result<Value, TbxError> {
        let session = self.session().await;
        let params = vec![
            ("method".to_string(), "upload".to_string()),
            ("app_id".to_string(), APP_ID_WEB.to_string()),
            ("type".to_string(), "tmpfile".to_string()),
            ("path".to_string(), path.to_string()),
            ("uploadid".to_string(), upload_id.to_string()),
            ("partseq".to_string(), partseq.to_string()),
            ("partoffset".to_string(), offset.to_string()),
            ("jsToken".to_string(), session.js_token.clone()),
        ];
        // superfile2 は PCS ノード（例: c-jp.terabox.com）へ送る必要がある。
        // www.terabox.com へ直接送ると openresty が 405 を返す（実測）。
        let host = self.upload_host().await?;
        let url = format!("https://{host}/rest/2.0/pcs/superfile2");
        let resp = self
            .http
            .post(&url)
            .query(&params)
            .header(COOKIE, Self::cookie_header(&session))
            .body(bytes)
            .send()
            .await?;
        let status = resp.status();
        let body = resp.text().await?;
        if !status.is_success() {
            return Err(TbxError::Other(format!(
                "パート送信が HTTP {} を返しました（本文先頭: {}）",
                status.as_u16(),
                head(&body, 200)
            )));
        }
        let value: Value = serde_json::from_str(&body).unwrap_or(Value::Null);
        if let Some(errno) = value.get("errno").and_then(|v| v.as_i64()) {
            if errno != 0 {
                return Err(TbxError::Api(ApiError {
                    endpoint: "/rest/2.0/pcs/superfile2".into(),
                    errno,
                    message: value
                        .get("errmsg")
                        .and_then(|v| v.as_str())
                        .map(|s| s.to_string()),
                    request_id: None,
                }));
            }
        }
        Ok(value)
    }

    /// アップロード先の PCS ノードを問い合わせる。
    ///
    /// `/rest/2.0/pcs/file?method=locateupload` が担当ノードを返す（実測:
    /// `{"host":"c-jp.terabox.com","expire":600}`）。キャッシュして使い直す。
    pub async fn upload_host(&self) -> Result<String, TbxError> {
        {
            let cached = self.upload_host_cache.lock().await;
            if let Some((host, at)) = &*cached {
                // 返る `expire` は 600 秒。余裕を持って 5 分で取り替える。
                if at.elapsed() < std::time::Duration::from_secs(300) {
                    return Ok(host.clone());
                }
            }
        }

        let params = vec![
            ("method".to_string(), "locateupload".to_string()),
            ("app_id".to_string(), APP_ID_WEB.to_string()),
        ];
        let value: Value = self
            .get_json("/rest/2.0/pcs/file", params)
            .await?;
        let host = value
            .get("host")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .ok_or_else(|| {
                TbxError::Other(
                    "アップロード先ホストを取得できませんでした（locateupload が host を返していません）"
                        .to_string(),
                )
            })?
            .to_string();

        let mut cached = self.upload_host_cache.lock().await;
        *cached = Some((host.clone(), std::time::Instant::now()));
        Ok(host)
    }

    /// 分割送信を確定する。
    pub async fn create_file(
        &self,
        path: &str,
        size: u64,
        upload_id: &str,
        block_list_md5: &[String],
    ) -> Result<Value, TbxError> {
        let block = format!(
            "[{}]",
            block_list_md5
                .iter()
                .map(|m| format!("\"{m}\""))
                .collect::<Vec<_>>()
                .join(",")
        );
        let params = vec![
            ("a".to_string(), "commit".to_string()),
            ("path".to_string(), path.to_string()),
            ("size".to_string(), size.to_string()),
            ("isdir".to_string(), "0".to_string()),
            ("uploadid".to_string(), upload_id.to_string()),
            ("block_list".to_string(), block),
        ];
        self.post_form_json("/api/create", params, Vec::new()).await
    }

    // ---- jsToken 再取得 ----

    /// `errno -6` などで失効した可能性があるとき、`/main` から 1 度だけ取り直す。
    async fn maybe_refresh(&self, _origin: String) -> Result<(), TbxError> {
        // 呼び出し側で明示的に `refresh_from_main` を使う設計にしているため、
        // ここでは何もしない。自動再取得を無制限に行うと無限ループの温床になる。
        Ok(())
    }

    /// `/main` を取得し直して jsToken / bdstoken を更新する。
    ///
    /// 呼び出しは明示的に行う（`errno -6` を受けた後など）。連続実行は
    /// `refresh_lock` で直列化する。
    pub async fn refresh_from_main(&self) -> Result<Bootstrap, TbxError> {
        let _guard = self.refresh_lock.lock().await;
        let session = self.session().await;
        let url = format!("{}/main", self.base);
        let resp = self
            .http
            .get(&url)
            .header(COOKIE, Self::cookie_header(&session))
            .send()
            .await?;
        let status = resp.status();
        let body = resp.text().await?;
        if !status.is_success() {
            return Err(TbxError::Other(format!(
                "/main が HTTP {} を返しました",
                status.as_u16()
            )));
        }
        let bootstrap = Bootstrap::parse(&body)?;
        let mut s = self.session.lock().await;
        s.refresh_from(&bootstrap);
        Ok(bootstrap)
    }
}

/// `/api/precreate` の結果。
#[derive(Debug, Clone)]
pub struct PrecreateResult {
    /// 分割送信に使う ID。`return_type=2`（既存流用）では `None`。
    pub upload_id: Option<String>,
    /// 1=新規送信が必要, 2=既存ファイルを流用できる。
    pub return_type: i64,
    /// 生 JSON。`info` など追加情報を捨てないため保持する。
    pub raw: Value,
}

impl PrecreateResult {
    /// 送信不要（サーバ側に同一内容が既にある）か。
    pub fn is_deduplicated(&self) -> bool {
        self.return_type == 2
    }
}

/// URL でも ID でも受け取り、`1` 始まりの共有 ID に正規化する。
///
/// `/s/1abcdef` のパス部分は既に `1` を含む ID なので、そのまま取り出す。
/// 先頭に `1` を付け足すと `11abcdef` になってしまう。
pub fn normalize_shorturl(input: &str) -> String {
    let trimmed = input.trim();
    // `https://www.terabox.com/s/1abcDEF` 形式
    if let Some(pos) = trimmed.find("/s/") {
        let tail = &trimmed[pos + 3..];
        let id: String = tail
            .chars()
            .take_while(|c| !matches!(c, '?' | '&' | '#'))
            .collect();
        return id;
    }
    // `?surl=1abc` 形式
    if let Some(pos) = trimmed.find("surl=") {
        let tail = &trimmed[pos + 5..];
        let id: String = tail
            .chars()
            .take_while(|c| !matches!(c, '?' | '&' | '#'))
            .collect();
        if !id.is_empty() {
            return id;
        }
    }
    trimmed.to_string()
}

fn head(text: &str, n: usize) -> String {
    text.chars().take(n).collect()
}

fn escape_json(s: &str) -> String {
    s.replace('\\', "\\\\").replace('"', "\\\"")
}

/// `fs_id` 群を **数値の配列** にする（例: `[1089841491208719]`）。
///
/// ゴミ箱系 API の `fidlist` は数値配列でなければ `errno 2`（param err）に
/// なる（実測: 文字列配列・単体値・別キー名はすべて失敗）。数値として
/// 解釈できない ID が混じっていたら、その場で明示的に失敗させる。
/// 黙って 0 や空配列を送ると「何も復元されない」のに成功したように見える。
fn numeric_id_array(ids: &[String]) -> Result<String, TbxError> {
    let mut parsed: Vec<String> = Vec::with_capacity(ids.len());
    for raw in ids {
        let trimmed = raw.trim();
        if trimmed.is_empty() || !trimmed.chars().all(|c| c.is_ascii_digit()) {
            return Err(TbxError::Other(format!(
                "fs_id が数値ではありません: {raw}（ゴミ箱 API は数値の fidlist しか受けません）"
            )));
        }
        parsed.push(trimmed.to_string());
    }
    Ok(format!("[{}]", parsed.join(",")))
}

fn pick_i64(value: &Value, keys: &[&str]) -> Option<i64> {
    for key in keys {
        if let Some(v) = value.get(*key) {
            if let Some(n) = v.as_i64() {
                return Some(n);
            }
            if let Some(s) = v.as_str() {
                if let Ok(n) = s.parse::<i64>() {
                    return Some(n);
                }
            }
        }
    }
    None
}

fn pick_u64(value: &Value, keys: &[&str]) -> Option<u64> {
    for key in keys {
        if let Some(v) = value.get(*key) {
            if let Some(n) = v.as_u64() {
                return Some(n);
            }
            if let Some(s) = v.as_str() {
                if let Ok(n) = s.parse::<u64>() {
                    return Some(n);
                }
            }
        }
    }
    None
}

fn pick_bool(value: &Value, keys: &[&str]) -> Option<bool> {
    for key in keys {
        if let Some(v) = value.get(*key) {
            if let Some(b) = v.as_bool() {
                return Some(b);
            }
            if let Some(n) = v.as_i64() {
                return Some(n != 0);
            }
            if let Some(s) = v.as_str() {
                return Some(matches!(s, "1" | "true" | "TRUE"));
            }
        }
    }
    None
}

fn pick_string(value: &Value, keys: &[&str]) -> Option<String> {
    for key in keys {
        if let Some(v) = value.get(*key) {
            if let Some(s) = v.as_str() {
                if !s.is_empty() {
                    return Some(s.to_string());
                }
            }
            if let Some(n) = v.as_i64() {
                return Some(n.to_string());
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shorturl_variants_normalize() {
        assert_eq!(normalize_shorturl("1abcDEF"), "1abcDEF");
        assert_eq!(normalize_shorturl("https://www.terabox.com/s/1abcDEF"), "1abcDEF");
        assert_eq!(
            normalize_shorturl("https://1024terabox.com/s/1XyZ?pwd=1234"),
            "1XyZ"
        );
        assert_eq!(normalize_shorturl("https://x.com/main?surl=1Qq"), "1Qq");
    }

    #[test]
    fn pick_helpers_prefer_first_present_key() {
        let v: Value = serde_json::from_str(r#"{"total":"123","used":7,"is_vip":1}"#).unwrap();
        assert_eq!(pick_u64(&v, &["total"]), Some(123));
        assert_eq!(pick_u64(&v, &["missing", "used"]), Some(7));
        assert_eq!(pick_bool(&v, &["is_vip"]), Some(true));
        assert_eq!(pick_u64(&v, &["nope"]), None);
    }

    #[test]
    fn precreate_dedup_detection() {
        let r = PrecreateResult {
            upload_id: None,
            return_type: 2,
            raw: Value::Null,
        };
        assert!(r.is_deduplicated());
        let r2 = PrecreateResult {
            upload_id: Some("u".into()),
            return_type: 1,
            raw: Value::Null,
        };
        assert!(!r2.is_deduplicated());
    }

    #[test]
    fn json_escape_handles_quotes_and_backslash() {
        assert_eq!(escape_json(r#"a"b\c"#), r#"a\"b\\c"#);
    }
}
