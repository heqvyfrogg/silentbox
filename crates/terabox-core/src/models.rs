//! API が返すデータ構造。
//!
//! フィールド名は TeraBox の生 JSON に合わせる。未知フィールドは無視するが、
//! 表示に必要な値は欠けても `None` で保持し、推測値で埋めない。

use serde::{Deserialize, Serialize};

/// ファイル/フォルダ 1 件。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FsEntry {
    /// サーバ側の一意 ID。文字列で保持する（数値精度の取りこぼしを避ける）。
    #[serde(default, deserialize_with = "de_fs_id")]
    pub fs_id: String,
    /// 親からの名前。
    #[serde(default)]
    pub server_filename: String,
    /// 絶対パス。
    #[serde(default)]
    pub path: String,
    /// ディレクトリなら 1。
    #[serde(default, deserialize_with = "de_int_bool")]
    pub isdir: bool,
    /// バイト数。ディレクトリは 0。
    #[serde(default)]
    pub size: u64,
    /// サーバ側更新時刻（epoch 秒）。
    #[serde(default)]
    pub server_mtime: i64,
    /// サーバ側作成時刻（epoch 秒）。
    #[serde(default)]
    pub server_ctime: i64,
    /// MD5（`md5` フィールド。ディレクトリでは空）。
    #[serde(default)]
    pub md5: String,
    /// サムネイル URL（ある場合のみ）。
    #[serde(default)]
    pub thumbs: Option<serde_json::Value>,
    /// カテゴリ番号（1=動画, 2=音声, 3=画像, 4=文書, 5=アプリ, 6=その他）。
    #[serde(default)]
    pub category: Option<i64>,
}

impl FsEntry {
    /// 一覧表示用の名前。`server_filename` が空なら `path` の末尾で補う。
    pub fn display_name(&self) -> &str {
        if !self.server_filename.is_empty() {
            return &self.server_filename;
        }
        match self.path.rsplit('/').next() {
            Some(tail) if !tail.is_empty() => tail,
            _ => &self.path,
        }
    }

    /// 更新時刻をローカル時刻の文字列にする。0 は「不明」。
    pub fn mtime_display(&self) -> String {
        if self.server_mtime <= 0 {
            return "不明".to_string();
        }
        match chrono::DateTime::from_timestamp(self.server_mtime, 0) {
            Some(utc) => utc
                .with_timezone(&chrono::Local)
                .format("%Y-%m-%d %H:%M")
                .to_string(),
            None => "不明".to_string(),
        }
    }
}

/// `fs_id` は数値でも文字列でも来るため、両方を受けて文字列化する。
fn de_fs_id<'de, D>(deserializer: D) -> Result<String, D::Error>
where
    D: serde::Deserializer<'de>,
{
    use serde::de::{self, Visitor};
    struct V;
    impl<'de> Visitor<'de> for V {
        type Value = String;
        fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
            f.write_str("整数または文字列の fs_id")
        }
        fn visit_str<E: de::Error>(self, v: &str) -> Result<String, E> {
            Ok(v.to_string())
        }
        fn visit_string<E: de::Error>(self, v: String) -> Result<String, E> {
            Ok(v)
        }
        fn visit_u64<E: de::Error>(self, v: u64) -> Result<String, E> {
            Ok(v.to_string())
        }
        fn visit_i64<E: de::Error>(self, v: i64) -> Result<String, E> {
            Ok(v.to_string())
        }
        fn visit_f64<E: de::Error>(self, v: f64) -> Result<String, E> {
            Ok((v as i64).to_string())
        }
        fn visit_none<E: de::Error>(self) -> Result<String, E> {
            Ok(String::new())
        }
        fn visit_unit<E: de::Error>(self) -> Result<String, E> {
            Ok(String::new())
        }
    }
    deserializer.deserialize_any(V)
}

/// `0/1` の数値と `true/false` の両方を受ける。
fn de_int_bool<'de, D>(deserializer: D) -> Result<bool, D::Error>
where
    D: serde::Deserializer<'de>,
{
    use serde::de::{self, Visitor};
    struct V;
    impl<'de> Visitor<'de> for V {
        type Value = bool;
        fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
            f.write_str("0/1 または真偽値")
        }
        fn visit_bool<E: de::Error>(self, v: bool) -> Result<bool, E> {
            Ok(v)
        }
        fn visit_u64<E: de::Error>(self, v: u64) -> Result<bool, E> {
            Ok(v != 0)
        }
        fn visit_i64<E: de::Error>(self, v: i64) -> Result<bool, E> {
            Ok(v != 0)
        }
        fn visit_str<E: de::Error>(self, v: &str) -> Result<bool, E> {
            Ok(matches!(v, "1" | "true" | "TRUE" | "True"))
        }
        fn visit_none<E: de::Error>(self) -> Result<bool, E> {
            Ok(false)
        }
        fn visit_unit<E: de::Error>(self) -> Result<bool, E> {
            Ok(false)
        }
    }
    deserializer.deserialize_any(V)
}

/// `/api/list` の応答。
#[derive(Debug, Clone, Deserialize)]
pub struct ListResponse {
    #[serde(default)]
    pub errno: i64,
    #[serde(default)]
    pub errmsg: Option<String>,
    #[serde(default)]
    pub show_msg: Option<String>,
    #[serde(default)]
    pub request_id: Option<serde_json::Value>,
    #[serde(default)]
    pub list: Vec<FsEntry>,
    /// サーバ側のカーソル。存在すれば続きがある。
    #[serde(default)]
    pub cursor: Option<serde_json::Value>,
    #[serde(default)]
    pub has_more: Option<i64>,
}

/// `/api/quota` 応答と `/api/user/getinfo` 応答を合成した容量情報。
///
/// 取得できなかった項目は `None` のままにする。推定値は入れない。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct QuotaInfo {
    /// 総容量（バイト）。
    pub total_bytes: Option<u64>,
    /// 使用量（バイト）。
    pub used_bytes: Option<u64>,
    /// 無料枠の上限（バイト）。サーバが返した場合のみ。
    pub free_limit_bytes: Option<u64>,
    /// VIP かどうか。
    pub is_vip: Option<bool>,
    /// VIP 種別の生値（`VIP_TYPE_*` 相当）。
    pub vip_kind: Option<String>,
    /// 単一ファイル上限（バイト）。サーバが返した場合のみ。
    pub max_file_size_bytes: Option<u64>,
    /// 同時ダウンロード可否。サーバが返した場合のみ。
    pub concurrent_download_allowed: Option<bool>,
    /// 恒久枠の上限（バイト）。`extra.init_quota_type` から読む。
    pub permanent_quota_bytes: Option<u64>,
    /// 期間限定枠の上限（バイト）。`extra.init_quota_type` から読む。
    pub temporary_quota_bytes: Option<u64>,
    /// 期間限定枠の期限（epoch 秒）。`extra.time_limit_quota_expire_time`。
    pub temporary_quota_expires_at: Option<i64>,
    /// `extra.init_quota_type` の生値（例: `permanent_30g_temp_994g`）。
    pub quota_type_raw: Option<String>,
    /// 取得できなかった項目の理由。
    pub unavailable: Vec<String>,
}

/// `permanent_30g_temp_994g` のような文字列から枠の内訳を読む。
///
/// `permanent_<n>g` と `temp_<n>g` を探す。見つからなければ `None` を返す
/// （推測で補わない）。
pub fn parse_quota_type(raw: &str) -> (Option<u64>, Option<u64>) {
    fn number_after(input: &str, prefix: &str) -> Option<u64> {
        let rest = input.split(prefix).nth(1)?;
        let digits: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
        if digits.is_empty() {
            return None;
        }
        let value: u64 = digits.parse().ok()?;
        // 単位は末尾の 1 文字（g / t / m）。
        let unit = rest[digits.len()..].chars().next()?;
        let multiplier = match unit {
            'g' | 'G' => 1024u64.pow(3),
            't' | 'T' => 1024u64.pow(4),
            'm' | 'M' => 1024u64.pow(2),
            _ => return None,
        };
        Some(value.saturating_mul(multiplier))
    }

    (number_after(raw, "permanent_"), number_after(raw, "temp_"))
}

impl QuotaInfo {
    /// 残量。総量と使用量の両方が取れたときだけ算出する。
    pub fn free_bytes(&self) -> Option<u64> {
        match (self.total_bytes, self.used_bytes) {
            (Some(t), Some(u)) => Some(t.saturating_sub(u)),
            _ => None,
        }
    }
}

/// 共有リンクの情報。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ShareInfo {
    /// 共有 ID。
    pub share_id: String,
    /// 共有したユーザーの `uk`。
    pub uk: String,
    /// 共有の説明。
    pub description: Option<String>,
    /// 期限切れかどうか（サーバが返した場合）。
    pub expired: Option<bool>,
    /// 中身の一覧。
    pub entries: Vec<FsEntry>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fs_id_accepts_number_and_string() {
        let a: FsEntry = serde_json::from_str(r#"{"fs_id":123456789012345,"path":"/a"}"#).unwrap();
        assert_eq!(a.fs_id, "123456789012345");

        let b: FsEntry = serde_json::from_str(r#"{"fs_id":"987","path":"/b"}"#).unwrap();
        assert_eq!(b.fs_id, "987");
    }

    #[test]
    fn isdir_accepts_int_and_bool() {
        let a: FsEntry = serde_json::from_str(r#"{"fs_id":1,"isdir":1}"#).unwrap();
        assert!(a.isdir);
        let b: FsEntry = serde_json::from_str(r#"{"fs_id":1,"isdir":0}"#).unwrap();
        assert!(!b.isdir);
        let c: FsEntry = serde_json::from_str(r#"{"fs_id":1,"isdir":true}"#).unwrap();
        assert!(c.isdir);
    }

    #[test]
    fn display_name_falls_back_to_path() {
        let e: FsEntry = serde_json::from_str(r#"{"fs_id":1,"path":"/dir/name.bin"}"#).unwrap();
        assert_eq!(e.display_name(), "name.bin");
    }

    #[test]
    fn list_response_keeps_errno() {
        let r: ListResponse = serde_json::from_str(r#"{"errno":-6}"#).unwrap();
        assert_eq!(r.errno, -6);
        assert!(r.list.is_empty());
    }

    #[test]
    fn quota_free_is_none_without_both_sides() {
        let q = QuotaInfo {
            total_bytes: Some(100),
            used_bytes: None,
            ..Default::default()
        };
        assert_eq!(q.free_bytes(), None);
    }
}
