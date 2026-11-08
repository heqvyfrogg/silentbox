//! Windows の 8.3 短縮名（ショートネーム）生成。
//!
//! WinFsp はディレクトリ列挙の「マーカー」に **8.3 形式の名前** を返す。
//! 例えば `tbx-probe` のマーカーは `tbx-prob`（ベース名 8 文字まで）になる。
//! フルネームだけで比較すると一致せず、2 ページ目以降が丸ごと欠落する
//! （実測: ルート直下の 2 件目が一覧から消えた）。
//!
//! ここでは FAT 系の 8.3 生成規則に沿って短縮名を作り、フルネームと
//! どちらでも一致するようにする。

/// 8.3 形式の短縮名を作る。
///
/// - ベース名は大文字化して 8 文字まで
/// - 拡張子は大文字化して 3 文字まで
/// - 拡張子が無ければ `.` を付けない
/// - 空白・制御文字は除去する
pub fn short_name(name: &str) -> String {
    let (stem, ext) = split_name(name);

    let base: String = sanitize(stem).chars().take(8).collect();
    let extension: String = sanitize(ext).chars().take(3).collect();

    if extension.is_empty() {
        base
    } else {
        format!("{base}.{extension}")
    }
}

/// 名前を「最後の `.` より前」と「後ろ」に分ける。
///
/// 先頭が `.` の隠しファイル風の名前は、拡張子なしとして扱う。
fn split_name(name: &str) -> (&str, &str) {
    match name.rfind('.') {
        Some(0) | None => (name, ""),
        Some(i) => (&name[..i], &name[i + 1..]),
    }
}

fn sanitize(input: &str) -> String {
    input
        .chars()
        .filter(|c| {
            !c.is_whitespace()
                && !c.is_control()
                && !matches!(c, '"' | '*' | '?' | '<' | '>' | '|' | ':' | '/' | '\\')
        })
        .flat_map(|c| c.to_uppercase())
        .collect()
}

/// マーカーがこの名前を指しているか。
///
/// フルネーム（大文字小文字を無視）と 8.3 短縮名のどちらでも一致させる。
/// 加えて、WinFsp がバッファの都合でマーカーを切り詰めて返す場合に備え、
/// 名前の接頭辞でも一致とみなす（実測: `a.png` のマーカーが `a.pn` で返り、
/// 完全一致だけで比べると 2 件目以降が丸ごと欠落した）。
///
/// 接頭辞の一致は 3 文字以上に限る。1〜2 文字の接頭辞まで許すと `a` が
/// `abc.txt` に一致して 1 件飛ばすため。
pub fn marker_matches(marker: &str, name: &str) -> bool {
    if marker.is_empty() {
        return false;
    }
    if marker.eq_ignore_ascii_case(name) {
        return true;
    }
    if marker.eq_ignore_ascii_case(&short_name(name)) {
        return true;
    }
    if marker.chars().count() < 3 {
        return false;
    }
    let needle = marker.to_lowercase();
    if name.to_lowercase().starts_with(&needle) {
        return true;
    }
    short_name(name).to_lowercase().starts_with(&needle)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn typical_names() {
        assert_eq!(short_name("tbx-probe"), "TBX-PROB");
        assert_eq!(short_name("readme.md"), "README.MD");
        assert_eq!(short_name("a.txt"), "A.TXT");
        assert_eq!(short_name("noext"), "NOEXT");
    }

    #[test]
    fn extension_is_three_chars() {
        assert_eq!(short_name("file.longext"), "FILE.LON");
    }

    #[test]
    fn hidden_style_has_no_extension() {
        assert_eq!(short_name(".bashrc"), ".BASHRC");
    }

    #[test]
    fn sanitizes_whitespace() {
        assert_eq!(short_name("a b.txt"), "AB.TXT");
    }

    #[test]
    fn marker_matches_full_and_short() {
        assert!(marker_matches("tbx-probe", "tbx-probe"));
        assert!(marker_matches("TBX-PROB", "tbx-probe"));
        assert!(marker_matches("tbx-prob", "tbx-probe"));
        // マーカーは名前より短いこともある（WinFsp がバッファに合わせて
        // 切り詰める）。前方一致で拾わないと 2 件目以降が丸ごと欠落する。
        assert!(marker_matches("tbx-pro", "tbx-probe"));
        assert!(!marker_matches("tbx-prx", "tbx-probe"));
        assert!(!marker_matches("zzz", "tbx-probe"));
    }

    #[test]
    fn truncated_marker_still_matches() {
        // 実測: 2 件目を返すときの WinFsp のマーカーは `a.pn` だった
        // （`a.png` の切り詰め）。完全一致だけで比べると一致せず、
        // 2 ページ目以降が空になる（実測: 2 件あるのに 1 件しか見えなかった）。
        assert!(marker_matches("a.pn", "a.png"));
        assert!(marker_matches("A.PN", "a.png"));
        // 短縮名の切り詰めでも拾えること。
        assert!(marker_matches("A.P", "a.png"));
    }

    #[test]
    fn marker_matches_uppercase_full_name() {
        assert!(marker_matches("README.MD", "readme.md"));
    }
}
