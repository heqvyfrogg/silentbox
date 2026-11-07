//! フォルダ一括ダウンロードの走査。
//!
//! リモートを幅優先で辿り、ローカルへ再現する相対パスを作る。転送そのものは
//! 呼び出し側（CLI）が既存の `Downloader` で行う。ここでは「何をどこへ」だけを
//! 決めるので、ネットワークに触らない純粋な組み立てとして試験できる。

use terabox_core::models::FsEntry;
use terabox_core::vfs::base_name;

/// 落とす 1 件。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlannedFile {
    /// リモートの絶対パス。
    pub remote_path: String,
    /// 保存先のディレクトリ（ルートからの相対）。
    pub relative_dirs: Vec<String>,
    /// 保存ファイル名。
    pub file_name: String,
    /// 期待サイズ。
    pub size: u64,
    /// `fs_id`。
    pub fs_id: String,
}

/// 作るべき空フォルダ（ルートからの相対）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlannedDir {
    pub relative_dirs: Vec<String>,
}

/// 走査結果。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Plan {
    pub dirs: Vec<PlannedDir>,
    /// 親を先に作れるよう、深さの浅い順に並べる。
    pub files: Vec<PlannedFile>,
}

/// 1 ディレクトリ分の一覧から、子の計画を積む。
///
/// `dir_relative` は、走査中のディレクトリがルートから見てどこにいるか。
pub fn extend(plan: &mut Plan, dir_relative: &[String], entries: &[FsEntry]) {
    for entry in entries {
        let name = if entry.server_filename.is_empty() {
            base_name(&entry.path)
        } else {
            entry.server_filename.clone()
        };
        if name.is_empty() {
            continue;
        }
        let mut child = dir_relative.to_vec();
        child.push(name.clone());
        if entry.isdir {
            plan.dirs.push(PlannedDir {
                relative_dirs: child,
            });
        } else {
            plan.files.push(PlannedFile {
                remote_path: entry.path.clone(),
                relative_dirs: dir_relative.to_vec(),
                file_name: name,
                size: entry.size,
                fs_id: entry.fs_id.clone(),
            });
        }
    }
}

/// 親ディレクトリを先に作れるよう並べ替える。同じ深さなら名前順。
pub fn sort(plan: &mut Plan) {
    plan.dirs.sort_by(|a, b| {
        a.relative_dirs
            .len()
            .cmp(&b.relative_dirs.len())
            .then_with(|| a.relative_dirs.cmp(&b.relative_dirs))
    });
    plan.files.sort_by(|a, b| {
        a.relative_dirs
            .len()
            .cmp(&b.relative_dirs.len())
            .then_with(|| a.relative_dirs.cmp(&b.relative_dirs))
            .then_with(|| a.file_name.cmp(&b.file_name))
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(path: &str, isdir: bool, size: u64) -> FsEntry {
        FsEntry {
            fs_id: format!("id-{}", base_name(path)),
            server_filename: base_name(path),
            path: path.to_string(),
            isdir,
            size,
            server_mtime: 0,
            server_ctime: 0,
            md5: String::new(),
            thumbs: None,
            category: None,
        }
    }

    #[test]
    fn mixes_files_and_dirs_at_root() {
        let mut plan = Plan::default();
        extend(
            &mut plan,
            &[],
            &[
                entry("/root/a.txt", false, 10),
                entry("/root/sub", true, 0),
            ],
        );
        assert_eq!(plan.files.len(), 1);
        assert_eq!(plan.dirs.len(), 1);
        assert_eq!(plan.files[0].remote_path, "/root/a.txt");
        assert_eq!(plan.files[0].relative_dirs, Vec::<String>::new());
        assert_eq!(plan.files[0].file_name, "a.txt");
        assert_eq!(plan.dirs[0].relative_dirs, vec!["sub".to_string()]);
    }

    #[test]
    fn nested_entries_carry_parent_dirs() {
        let mut plan = Plan::default();
        extend(
            &mut plan,
            std::slice::from_ref(&"sub".to_string()),
            &[entry("/root/sub/deep.txt", false, 5)],
        );
        assert_eq!(plan.files[0].relative_dirs, vec!["sub".to_string()]);
    }

    #[test]
    fn sorts_shallow_first() {
        let mut plan = Plan::default();
        extend(
            &mut plan,
            &["a".to_string(), "b".to_string()],
            &[entry("/root/a/b/c.txt", false, 1)],
        );
        extend(&mut plan, &["a".to_string()], &[entry("/root/a/z.txt", false, 2)]);
        extend(&mut plan, &[], &[entry("/root/top.txt", false, 3)]);
        sort(&mut plan);
        assert_eq!(plan.files[0].file_name, "top.txt");
        assert_eq!(plan.files[1].file_name, "z.txt");
        assert_eq!(plan.files[2].file_name, "c.txt");
    }

    #[test]
    fn empty_name_is_skipped() {
        let mut plan = Plan::default();
        // 名前もパスも空の項目（壊れた応答）は落とさない。
        let mut e = entry("/root/x", false, 0);
        e.server_filename = String::new();
        e.path = "/".to_string();
        extend(&mut plan, &[], &[e]);
        assert!(plan.files.is_empty(), "名前が決められない項目は落とさない");
    }
}
