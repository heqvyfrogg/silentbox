//! リモートファイルシステムの抽象。
//!
//! WinFsp（Windows のネイティブドライブ）と WebDAV（インストーラ不要の
//! フォールバック）は、どちらもこのトレイト越しに同じ API を叩く。
//! 具象バックエンドを追加しても上位の CLI / TUI は変えなくて済む。
//!
//! パス規約: すべて `/` 始まり・`/` 区切り。ルートは `/`。

use async_trait::async_trait;

use crate::error::TbxError;
use crate::models::FsEntry;

/// リモート FS のメタ情報。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteMeta {
    pub path: String,
    pub size: u64,
    pub is_dir: bool,
    /// 更新時刻（epoch 秒、不明なら 0）。
    pub mtime: i64,
    pub fs_id: String,
}

impl RemoteMeta {
    pub fn from_entry(entry: &FsEntry) -> Self {
        Self {
            path: entry.path.clone(),
            size: entry.size,
            is_dir: entry.isdir,
            mtime: entry.server_mtime,
            fs_id: entry.fs_id.clone(),
        }
    }
}

/// リモート FS 操作。
#[async_trait]
pub trait RemoteFs: Send + Sync {
    /// ディレクトリ直下を列挙する。
    async fn read_dir(&self, path: &str) -> Result<Vec<FsEntry>, TbxError>;

    /// 1 件のメタ情報を返す。存在しなければ `Ok(None)`。
    async fn metadata(&self, path: &str) -> Result<Option<RemoteMeta>, TbxError>;

    /// ファイル全体を読む。
    ///
    /// 巨大ファイルを丸ごとメモリに載せたくない呼び出し元のために、
    /// 範囲読み出し `read_range` も併せて提供する。
    async fn read(&self, path: &str) -> Result<Vec<u8>, TbxError>;

    /// 範囲読み出し（`offset` から `length` バイト）。
    async fn read_range(&self, path: &str, offset: u64, length: u64) -> Result<Vec<u8>, TbxError>;

    /// 書き込み（新規作成または上書き）。
    async fn write(&self, path: &str, data: &[u8]) -> Result<(), TbxError>;

    /// 削除。既定ではゴミ箱へ移動する実装を推奨。
    async fn delete(&self, path: &str) -> Result<(), TbxError>;

    /// 名前変更。
    async fn rename(&self, from: &str, to: &str) -> Result<(), TbxError>;

    /// ディレクトリ作成。
    async fn create_dir(&self, path: &str) -> Result<(), TbxError>;
}

/// `Arc<T>` をそのまま `RemoteFs` として扱えるようにする。
///
/// WebDAV / WinFsp の両バックエンドは `Arc<dyn RemoteFs>` を共有するため、
/// 参照カウント越しの委譲を用意しておく。
#[async_trait]
impl<T: RemoteFs + ?Sized> RemoteFs for std::sync::Arc<T> {
    async fn read_dir(&self, path: &str) -> Result<Vec<FsEntry>, TbxError> {
        (**self).read_dir(path).await
    }

    async fn metadata(&self, path: &str) -> Result<Option<RemoteMeta>, TbxError> {
        (**self).metadata(path).await
    }

    async fn read(&self, path: &str) -> Result<Vec<u8>, TbxError> {
        (**self).read(path).await
    }

    async fn read_range(&self, path: &str, offset: u64, length: u64) -> Result<Vec<u8>, TbxError> {
        (**self).read_range(path, offset, length).await
    }

    async fn write(&self, path: &str, data: &[u8]) -> Result<(), TbxError> {
        (**self).write(path, data).await
    }

    async fn delete(&self, path: &str) -> Result<(), TbxError> {
        (**self).delete(path).await
    }

    async fn rename(&self, from: &str, to: &str) -> Result<(), TbxError> {
        (**self).rename(from, to).await
    }

    async fn create_dir(&self, path: &str) -> Result<(), TbxError> {
        (**self).create_dir(path).await
    }
}

/// パスの正規化。`//` の連続を 1 本にし、末尾の `/` を落とす（ルートは `/` のまま）。
pub fn normalize_path(input: &str) -> String {
    let replaced = input.replace('\\', "/");
    let mut out = String::with_capacity(replaced.len() + 1);
    if !replaced.starts_with('/') {
        out.push('/');
    }
    let mut last_slash = false;
    for ch in replaced.chars() {
        if ch == '/' {
            if last_slash {
                continue;
            }
            last_slash = true;
            out.push('/');
        } else {
            last_slash = false;
            out.push(ch);
        }
    }
    if out.len() > 1 && out.ends_with('/') {
        out.pop();
    }
    if out.is_empty() {
        out.push('/');
    }
    out
}

/// 親パスを返す。ルートの親はルート。
pub fn parent_path(path: &str) -> String {
    let p = normalize_path(path);
    if p == "/" {
        return "/".to_string();
    }
    match p.rfind('/') {
        Some(0) => "/".to_string(),
        Some(i) => p[..i].to_string(),
        None => "/".to_string(),
    }
}

/// 末尾の名前を返す。ルートは空文字。
pub fn base_name(path: &str) -> String {
    let p = normalize_path(path);
    if p == "/" {
        return String::new();
    }
    match p.rfind('/') {
        Some(i) => p[i + 1..].to_string(),
        None => p,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_collapses_slashes() {
        assert_eq!(normalize_path("//a///b/"), "/a/b");
        assert_eq!(normalize_path("a/b"), "/a/b");
        assert_eq!(normalize_path("/"), "/");
        assert_eq!(normalize_path(""), "/");
        assert_eq!(normalize_path("\\\\a\\b"), "/a/b");
    }

    #[test]
    fn parent_and_base() {
        assert_eq!(parent_path("/a/b/c"), "/a/b");
        assert_eq!(parent_path("/a"), "/");
        assert_eq!(parent_path("/"), "/");
        assert_eq!(base_name("/a/b/c"), "c");
        assert_eq!(base_name("/"), "");
    }
}
