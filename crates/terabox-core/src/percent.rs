//! 百分率エンコード/デコードの最小実装。
//!
//! TeraBox の初期化 HTML は `decodeURIComponent(\`...\`)` の中に jsToken を
//! 埋め込む。その 1 箇所のために依存クレートを増やさない。

/// `%XX` を復元する。`+` は空白として扱わない（URI エンコードの規則に合わせる）。
///
/// 不正な `%` シーケンスはそのまま残す。例外を投げないのは、HTML 全体を
/// 走査する用途で 1 箇所の異常により全体を捨てたくないため。
pub fn decode(input: &str) -> String {
    let bytes = input.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            let hi = hex_val(bytes[i + 1]);
            let lo = hex_val(bytes[i + 2]);
            match (hi, lo) {
                (Some(h), Some(l)) => {
                    out.push((h << 4) | l);
                    i += 3;
                    continue;
                }
                _ => {}
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn hex_val(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::decode;

    #[test]
    fn decodes_percent_sequences() {
        assert_eq!(decode("function%20fn%28a%29"), "function fn(a)");
    }

    #[test]
    fn keeps_plain_text() {
        assert_eq!(decode("abcXYZ"), "abcXYZ");
    }

    #[test]
    fn keeps_invalid_sequences() {
        assert_eq!(decode("100%"), "100%");
        assert_eq!(decode("%zz"), "%zz");
    }
}
