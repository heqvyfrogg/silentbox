//! 実行ファイルの探索。
//!
//! `which` クレートは Windows で `winsafe` を引き、GNU ツールチェーンでは
//! リンクできないライブラリ（`ktmw32`）を要求する。ここでは PATH と既知の
//! インストール先だけを見る最小実装に置き換える。

use std::path::{Path, PathBuf};

/// PATH から実行ファイルを探す。拡張子は Windows の実行形式だけを見る。
pub fn which(name: &str) -> Option<PathBuf> {
    if let Some(path) = look_on_path(name) {
        return Some(path);
    }
    // 明示パスが渡された場合はそのまま存在確認。
    let direct = Path::new(name);
    if direct.is_file() {
        return Some(direct.to_path_buf());
    }
    None
}

fn look_on_path(name: &str) -> Option<PathBuf> {
    let path_var = std::env::var_os("PATH")?;
    // 拡張子が既に付いている場合はそのまま探す。
    let has_ext = Path::new(name).extension().is_some();
    for dir in std::env::split_paths(&path_var) {
        if has_ext {
            let candidate = dir.join(name);
            if candidate.is_file() {
                return Some(candidate);
            }
        } else {
            for ext in ["exe", "bat", "cmd"] {
                let candidate = dir.join(format!("{name}.{ext}"));
                if candidate.is_file() {
                    return Some(candidate);
                }
            }
        }
    }
    None
}

/// `%PROGRAMFILES%` 配下の既知パスを確認する。
///
/// プレーヤ探索（`pick_player`）は個別パスを直接見ているため現状は未使用。
/// 将来インストーラ差異をまとめて吸収するための入口として残す。
#[allow(dead_code)]
pub fn known_programs_path(relative: &str) -> Option<PathBuf> {
    for var in ["PROGRAMFILES", "ProgramFiles(x86)", "LOCALAPPDATA"] {
        if let Ok(base) = std::env::var(var) {
            let candidate = PathBuf::from(base).join(relative);
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_binary_returns_none() {
        assert!(which("definitely-not-a-real-binary-name-xyz").is_none());
    }

    #[test]
    fn known_programs_path_missing_is_none() {
        assert!(known_programs_path(r"NoSuchVendor\NoSuchApp\app.exe").is_none());
    }
}
