//! 表示まわり。人間向けの整形だけを置く。
//!
//! 方針:
//! - 数値やパスを `...` で切り詰めない（全文を出す）。
//! - 絵文字を使わない。
//! - 単位は SI ではなく 1024 進で表示し、桁を明示する。

use terabox_core::models::{FsEntry, QuotaInfo};
use terabox_core::transfer::Progress;

/// バイト数を読みやすくする。`1234` → `1.2 KiB`。
pub fn bytes(n: u64) -> String {
    const UNITS: [&str; 6] = ["B", "KiB", "MiB", "GiB", "TiB", "PiB"];
    if n < 1024 {
        return format!("{n} B");
    }
    let mut value = n as f64;
    let mut unit = 0usize;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    format!("{value:.1} {}", UNITS[unit])
}

/// 速度表示。`bytes_per_sec` は f64。
pub fn speed(bytes_per_sec: f64) -> String {
    if !bytes_per_sec.is_finite() || bytes_per_sec <= 0.0 {
        return "0 B/s".to_string();
    }
    format!("{}/s", bytes((bytes_per_sec.round()) as u64))
}

/// 進捗バー。`width` 文字で描く。`done > total` でも 100% を超えない。
pub fn bar(done: u64, total: u64, width: usize) -> String {
    if total == 0 {
        return format!("[{}] 0 バイト", "-".repeat(width.saturating_sub(2)));
    }
    let ratio = (done as f64 / total as f64).clamp(0.0, 1.0);
    let filled = ((ratio * width as f64).round() as usize).min(width);
    let mut s = String::with_capacity(width + 2);
    s.push('[');
    s.push_str(&"=".repeat(filled));
    s.push_str(&" ".repeat(width - filled));
    s.push(']');
    s
}

/// 進捗イベントを 1 行にする。`\r` で上書きする呼び出しを想定。
pub fn progress_line(p: &Progress) -> String {
    match p {
        Progress::Note(text) => text.clone(),
        Progress::Overall {
            done,
            total,
            bytes_per_sec,
            parts,
        } => {
            let pct = if *total == 0 {
                100.0
            } else {
                (*done as f64 / *total as f64 * 100.0).min(100.0)
            };
            let active = parts
                .iter()
                .filter(|p| p.state == terabox_core::store::State::Partial)
                .count();
            format!(
                "{} {:5.1}%  {} / {}  {}  接続 {} 本",
                bar(*done, *total, 28),
                pct,
                bytes(*done),
                bytes(*total),
                speed(*bytes_per_sec),
                active
            )
        }
    }
}

/// 一覧の 1 行。名前は切り詰めない。
pub fn entry_line(e: &FsEntry) -> String {
    let kind = if e.isdir { "DIR " } else { "FILE" };
    let size = if e.isdir {
        "-".to_string()
    } else {
        bytes(e.size)
    };
    format!(
        "{kind}  {size:>10}  {}  {}",
        e.mtime_display(),
        e.display_name()
    )
}

/// 容量サマリ。取得できなかった項目は理由をそのまま出す。
pub fn quota_block(q: &QuotaInfo) -> String {
    let mut out = String::new();
    out.push_str(&format!(
        "総容量      : {}\n",
        q.total_bytes.map(bytes).unwrap_or_else(|| "未取得".into())
    ));
    out.push_str(&format!(
        "使用量      : {}\n",
        q.used_bytes.map(bytes).unwrap_or_else(|| "未取得".into())
    ));
    out.push_str(&format!(
        "残量        : {}\n",
        q.free_bytes().map(bytes).unwrap_or_else(|| "未取得".into())
    ));
    out.push_str(&format!(
        "無料枠上限  : {}\n",
        q.free_limit_bytes.map(bytes).unwrap_or_else(|| "未取得".into())
    ));
    // 期間限定枠は内訳を必ず見せる。合計だけ出すと「永久に 1TB」と誤解される。
    if q.permanent_quota_bytes.is_some() || q.temporary_quota_bytes.is_some() {
        out.push_str(&format!(
            "  内訳      : 恒久 {} + 期間限定 {}\n",
            q.permanent_quota_bytes
                .map(bytes)
                .unwrap_or_else(|| "不明".into()),
            q.temporary_quota_bytes
                .map(bytes)
                .unwrap_or_else(|| "不明".into())
        ));
        if let Some(expire) = q.temporary_quota_expires_at {
            let when = chrono::DateTime::from_timestamp(expire, 0)
                .map(|dt| dt.with_timezone(&chrono::Local).format("%Y-%m-%d %H:%M").to_string())
                .unwrap_or_else(|| "不明".to_string());
            out.push_str(&format!("  期間限定枠の期限: {when}\n"));
        }
    }
    out.push_str(&format!(
        "VIP         : {}\n",
        match (q.is_vip, &q.vip_kind) {
            (Some(true), Some(kind)) => format!("あり（{kind}）"),
            (Some(true), None) => "あり".to_string(),
            (Some(false), _) => "なし".to_string(),
            (None, _) => "未取得".to_string(),
        }
    ));
    out.push_str(&format!(
        "単一ファイル上限: {}\n",
        q.max_file_size_bytes
            .map(bytes)
            .unwrap_or_else(|| "未取得".into())
    ));
    out.push_str(&format!(
        "同時ダウンロード: {}\n",
        match q.concurrent_download_allowed {
            Some(true) => "可",
            Some(false) => "不可",
            None => "未取得",
        }
    ));
    if !q.unavailable.is_empty() {
        out.push_str("\n取得できなかった項目:\n");
        for item in &q.unavailable {
            out.push_str(&format!("  - {item}\n"));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bytes_units() {
        assert_eq!(bytes(0), "0 B");
        assert_eq!(bytes(1023), "1023 B");
        assert_eq!(bytes(1024), "1.0 KiB");
        assert_eq!(bytes(1024 * 1024), "1.0 MiB");
        assert_eq!(bytes(1024u64.pow(3)), "1.0 GiB");
    }

    #[test]
    fn bar_clamps() {
        assert_eq!(bar(0, 100, 10), "[          ]");
        assert_eq!(bar(200, 100, 10), "[==========]");
        assert_eq!(bar(0, 0, 10), "[--------] 0 バイト");
    }

    #[test]
    fn bar_width_is_stable() {
        for done in [0u64, 1, 50, 99, 100] {
            assert_eq!(bar(done, 100, 12).chars().count(), 14);
        }
    }
}
