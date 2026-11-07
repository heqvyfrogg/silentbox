//! 実行ファイルの探索。
//!
//! `which` クレートは Windows で `winsafe` を引き、GNU ツールチェーンでは
//! リンクできないライブラリ（`ktmw32`）を要求する。ここでは PATH と既知の
//! インストール先だけを見る最小実装に置き換える。
//!
//! Windows は拡張子（`exe` / `bat` / `cmd`）を補う。Unix は拡張子を補わず、
//! 実行ビットが立っていることを条件にする（`is_file()` だけでは、
//! 実行権限の無いファイルを「使える」と誤認する）。

use std::path::{Path, PathBuf};

/// PATH から実行ファイルを探す。
pub fn which(name: &str) -> Option<PathBuf> {
    if let Some(path) = look_on_path(name) {
        return Some(path);
    }
    // 明示パスが渡された場合はそのまま確認。
    let direct = Path::new(name);
    if is_executable(direct) {
        return Some(direct.to_path_buf());
    }
    None
}

fn look_on_path(name: &str) -> Option<PathBuf> {
    let path_var = std::env::var_os("PATH")?;
    let has_ext = Path::new(name).extension().is_some();
    for dir in std::env::split_paths(&path_var) {
        if has_ext {
            let candidate = dir.join(name);
            if is_executable(&candidate) {
                return Some(candidate);
            }
        } else {
            #[cfg(windows)]
            {
                for ext in ["exe", "bat", "cmd"] {
                    let candidate = dir.join(format!("{name}.{ext}"));
                    if is_executable(&candidate) {
                        return Some(candidate);
                    }
                }
            }
            #[cfg(not(windows))]
            {
                let candidate = dir.join(name);
                if is_executable(&candidate) {
                    return Some(candidate);
                }
            }
        }
    }
    None
}

/// 「実行できるファイル」か。
pub fn is_executable(path: &Path) -> bool {
    if !path.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        // 実行ビットが 1 つも立っていなければ、呼び出しても失敗する。
        return std::fs::metadata(path)
            .map(|m| m.permissions().mode() & 0o111 != 0)
            .unwrap_or(false);
    }
    #[cfg(not(unix))]
    {
        true
    }
}

/// `%PROGRAMFILES%` 配下の既知パスを確認する（Windows のみ意味を持つ）。
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

    #[cfg(unix)]
    #[test]
    fn non_executable_file_is_rejected() {
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join(format!("tbx-which-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("not-exec");
        std::fs::write(&file, b"#!/bin/sh\n").unwrap();
        let mut perms = std::fs::metadata(&file).unwrap().permissions();
        perms.set_mode(0o644);
        std::fs::set_permissions(&file, perms).unwrap();

        assert!(!is_executable(&file), "実行ビットが無いのに実行可と判定した");

        let mut perms = std::fs::metadata(&file).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&file, perms).unwrap();
        assert!(is_executable(&file));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn executable_file_is_found_by_direct_path() {
        // 実行中の自分自身（テストバイナリ）は必ず実行可能。
        let exe = std::env::current_exe().unwrap();
        assert!(which(&exe.display().to_string()).is_some());
    }
}
