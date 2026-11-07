//! FUSE バックエンド（Linux / macOS）。
//!
//! `fuser` は純 Rust 実装で、ビルド時に libfuse を要求しない。実行時に
//! `/dev/fuse` とカーネルの FUSE 対応が必要。
//!
//! ## 設計
//! - FUSE は inode 番号で喋るが `RemoteFs` はパスで喋る。両者を表で結ぶ。
//! - 書き込みは全体置換しか API が無いため、開いたハンドルごとにバッファへ
//!   溜め、`flush` / `release` で 1 回だけ送る。失敗はその 2 つから返す
//!   （黙って成功を装わない）。
//! - `statfs` はサーバの実値。取れなければ 0（推測値を出さない）。

#![cfg(all(unix, feature = "fuse-backend"))]

use std::collections::HashMap;
use std::ffi::OsStr;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use fuser::{
    Config, Errno, FileAttr, FileHandle, FileType, Filesystem, FopenFlags, Generation, INodeNo,
    LockOwner, MountOption, OpenFlags, ReplyAttr, ReplyCreate, ReplyData, ReplyDirectory,
    ReplyEmpty, ReplyEntry, ReplyOpen, ReplyStatfs, ReplyWrite, Request, WriteFlags,
};
use terabox_core::vfs::{base_name, normalize_path, parent_path, RemoteFs, RemoteMeta};


/// 属性のキャッシュ有効期間。短くしておく（サーバが真実）。
const TTL: Duration = Duration::from_secs(1);

/// 1 つの開いたファイル。
struct OpenFile {
    /// この ハンドルが指すパス。
    path: String,
    /// 未送信の内容。`None` は未変更。
    pending: Option<Vec<u8>>,
}

/// inode とパス、開いているハンドルの対応。
struct State {
    paths: HashMap<u64, String>,
    inos: HashMap<String, u64>,
    next_ino: u64,
    handles: HashMap<u64, OpenFile>,
    next_handle: u64,
}

impl State {
    fn new() -> Self {
        let mut paths = HashMap::new();
        let mut inos = HashMap::new();
        paths.insert(1, "/".to_string());
        inos.insert("/".to_string(), 1);
        Self {
            paths,
            inos,
            next_ino: 2,
            handles: HashMap::new(),
            next_handle: 1,
        }
    }

    fn ino_for(&mut self, path: &str) -> u64 {
        let normalized = normalize_path(path);
        if let Some(existing) = self.inos.get(&normalized) {
            return *existing;
        }
        let ino = self.next_ino;
        self.next_ino += 1;
        self.paths.insert(ino, normalized.clone());
        inos_insert(&mut self.inos, normalized, ino);
        ino
    }

    fn path_of(&self, ino: u64) -> Option<String> {
        self.paths.get(&ino).cloned()
    }

    fn forget(&mut self, path: &str) {
        let normalized = normalize_path(path);
        if let Some(ino) = self.inos.remove(&normalized) {
            self.paths.remove(&ino);
        }
    }

    fn open(&mut self, path: &str, pending: Option<Vec<u8>>) -> u64 {
        let handle = self.next_handle;
        self.next_handle += 1;
        self.handles.insert(
            handle,
            OpenFile {
                path: normalize_path(path),
                pending,
            },
        );
        handle
    }
}

fn inos_insert(map: &mut HashMap<String, u64>, key: String, value: u64) {
    map.insert(key, value);
}

/// 現在の uid / gid。`/proc/self/status` から読む（macOS では 0）。
fn current_id(key: &str) -> u32 {
    let Ok(text) = std::fs::read_to_string("/proc/self/status") else {
        return 0;
    };
    for line in text.lines() {
        if let Some(rest) = line.strip_prefix(key) {
            if let Some(value) = rest.split_whitespace().nth(1) {
                return value.parse().unwrap_or(0);
            }
        }
    }
    0
}

fn uid() -> u32 {
    current_id("Uid")
}

fn gid() -> u32 {
    current_id("Gid")
}

/// `RemoteFs` を FUSE のファイルシステムへ写像する。
pub struct TeraBoxFuse {
    fs: Arc<dyn RemoteFs + Send + Sync>,
    state: Mutex<State>,
    /// 容量（総量, 残量）を返す関数。無ければ `statfs` は 0 を返す。
    usage: Option<Arc<dyn Fn() -> Option<(u64, u64)> + Send + Sync>>,
}

impl TeraBoxFuse {
    /// 新しいファイルシステムを作る。
    pub fn new(fs: Arc<dyn RemoteFs + Send + Sync>) -> Self {
        Self {
            fs,
            state: Mutex::new(State::new()),
            usage: None,
        }
    }

    /// 容量の取得元を渡す。
    pub fn with_usage(
        mut self,
        usage: Arc<dyn Fn() -> Option<(u64, u64)> + Send + Sync>,
    ) -> Self {
        self.usage = Some(usage);
        self
    }

    fn join(dir: &str, name: &OsStr) -> String {
        normalize_path(&format!("{dir}/{}", name.to_string_lossy()))
    }

    /// `RemoteMeta` から FUSE の属性を作る。
    fn attr_of(ino: u64, meta: &RemoteMeta) -> FileAttr {
        let mtime = if meta.mtime > 0 {
            UNIX_EPOCH + Duration::from_secs(meta.mtime as u64)
        } else {
            UNIX_EPOCH
        };
        FileAttr {
            ino: INodeNo(ino),
            size: if meta.is_dir { 0 } else { meta.size },
            blocks: if meta.is_dir {
                0
            } else {
                meta.size.div_ceil(512)
            },
            atime: mtime,
            mtime,
            ctime: mtime,
            crtime: mtime,
            kind: if meta.is_dir {
                FileType::Directory
            } else {
                FileType::RegularFile
            },
            perm: if meta.is_dir { 0o755 } else { 0o644 },
            nlink: if meta.is_dir { 2 } else { 1 },
            uid: uid(),
            gid: gid(),
            rdev: 0,
            blksize: 4096,
            flags: 0,
        }
    }

    /// 空のファイル属性（作成直後）。
    fn empty_attr(ino: u64, path_is_dir: bool) -> FileAttr {
        let now = SystemTime::now();
        FileAttr {
            ino: INodeNo(ino),
            size: 0,
            blocks: 0,
            atime: now,
            mtime: now,
            ctime: now,
            crtime: now,
            kind: if path_is_dir {
                FileType::Directory
            } else {
                FileType::RegularFile
            },
            perm: if path_is_dir { 0o755 } else { 0o644 },
            nlink: if path_is_dir { 2 } else { 1 },
            uid: uid(),
            gid: gid(),
            rdev: 0,
            blksize: 4096,
            flags: 0,
        }
    }

    /// パスの属性と ino を返す。存在しなければ `None`。
    fn lookup_attr(&self, path: &str) -> Option<(u64, FileAttr)> {
        let meta = crate::runtime::block_on(self.fs.metadata(path))
            .ok()
            .flatten()?;
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        let ino = state.ino_for(&meta.path);
        Some((ino, Self::attr_of(ino, &meta)))
    }

    /// ハンドルのバッファをサーバへ送る。
    fn persist(&self, handle: u64) -> Result<(), Errno> {
        let (path, data) = {
            let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
            let Some(open) = state.handles.get_mut(&handle) else {
                return Err(Errno::EBADF);
            };
            let Some(data) = open.pending.take() else {
                return Ok(());
            };
            (open.path.clone(), data)
        };
        match crate::runtime::block_on(self.fs.write(&path, &data)) {
            Ok(()) => Ok(()),
            Err(e) => {
                eprintln!("[fuse] {path} の送信に失敗しました: {e}");
                Err(Errno::EIO)
            }
        }
    }
}

impl Filesystem for TeraBoxFuse {
    fn lookup(&self, _req: &Request, parent: INodeNo, name: &OsStr, reply: ReplyEntry) {
        let dir = {
            let state = self.state.lock().unwrap_or_else(|e| e.into_inner());
            state.path_of(parent.0)
        };
        let Some(dir) = dir else {
            reply.error(Errno::ENOENT);
            return;
        };
        let path = Self::join(&dir, name);
        match self.lookup_attr(&path) {
            Some((_, attr)) => reply.entry(&TTL, &attr, Generation(0)),
            None => reply.error(Errno::ENOENT),
        }
    }

    fn getattr(&self, _req: &Request, ino: INodeNo, _fh: Option<FileHandle>, reply: ReplyAttr) {
        let path = {
            let state = self.state.lock().unwrap_or_else(|e| e.into_inner());
            state.path_of(ino.0)
        };
        let Some(path) = path else {
            reply.error(Errno::ENOENT);
            return;
        };
        match self.lookup_attr(&path) {
            Some((_, attr)) => reply.attr(&TTL, &attr),
            None => reply.error(Errno::ENOENT),
        }
    }

    fn mkdir(
        &self,
        _req: &Request,
        parent: INodeNo,
        name: &OsStr,
        _mode: u32,
        _umask: u32,
        reply: ReplyEntry,
    ) {
        let dir = {
            let state = self.state.lock().unwrap_or_else(|e| e.into_inner());
            state.path_of(parent.0)
        };
        let Some(dir) = dir else {
            reply.error(Errno::ENOENT);
            return;
        };
        let path = Self::join(&dir, name);
        match crate::runtime::block_on(self.fs.create_dir(&path)) {
            Ok(()) => {
                let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
                let ino = state.ino_for(&path);
                reply.entry(&TTL, &Self::empty_attr(ino, true), Generation(0));
            }
            Err(e) => {
                eprintln!("[fuse] {path} を作れませんでした: {e}");
                reply.error(Errno::EIO);
            }
        }
    }

    fn unlink(&self, _req: &Request, parent: INodeNo, name: &OsStr, reply: ReplyEmpty) {
        let dir = {
            let state = self.state.lock().unwrap_or_else(|e| e.into_inner());
            state.path_of(parent.0)
        };
        let Some(dir) = dir else {
            reply.error(Errno::ENOENT);
            return;
        };
        let path = Self::join(&dir, name);
        match crate::runtime::block_on(self.fs.delete(&path)) {
            Ok(()) => {
                self.state
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .forget(&path);
                reply.ok();
            }
            Err(e) => {
                eprintln!("[fuse] {path} を消せませんでした: {e}");
                reply.error(Errno::EIO);
            }
        }
    }

    fn rmdir(&self, req: &Request, parent: INodeNo, name: &OsStr, reply: ReplyEmpty) {
        self.unlink(req, parent, name, reply);
    }

    fn rename(
        &self,
        _req: &Request,
        parent: INodeNo,
        name: &OsStr,
        newparent: INodeNo,
        newname: &OsStr,
        _flags: fuser::RenameFlags,
        reply: ReplyEmpty,
    ) {
        let (from_dir, to_dir) = {
            let state = self.state.lock().unwrap_or_else(|e| e.into_inner());
            (state.path_of(parent.0), state.path_of(newparent.0))
        };
        let (Some(from_dir), Some(to_dir)) = (from_dir, to_dir) else {
            reply.error(Errno::ENOENT);
            return;
        };
        let from = Self::join(&from_dir, name);
        let to = Self::join(&to_dir, newname);
        match crate::runtime::block_on(self.fs.rename(&from, &to)) {
            Ok(()) => {
                self.state
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .forget(&from);
                reply.ok();
            }
            Err(e) => {
                eprintln!("[fuse] {from} を {to} へ移せませんでした: {e}");
                reply.error(Errno::EIO);
            }
        }
    }

    fn open(&self, _req: &Request, ino: INodeNo, _flags: OpenFlags, reply: ReplyOpen) {
        let path = {
            let state = self.state.lock().unwrap_or_else(|e| e.into_inner());
            state.path_of(ino.0)
        };
        let Some(path) = path else {
            reply.error(Errno::ENOENT);
            return;
        };
        let handle = self
            .state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .open(&path, None);
        reply.opened(FileHandle(handle), FopenFlags::empty());
    }

    fn create(
        &self,
        _req: &Request,
        parent: INodeNo,
        name: &OsStr,
        _mode: u32,
        _umask: u32,
        _flags: i32,
        reply: ReplyCreate,
    ) {
        let dir = {
            let state = self.state.lock().unwrap_or_else(|e| e.into_inner());
            state.path_of(parent.0)
        };
        let Some(dir) = dir else {
            reply.error(Errno::ENOENT);
            return;
        };
        let path = Self::join(&dir, name);

        // 空のファイルは送らない。内容は `flush` / `release` で 1 回だけ送る。
        // ここで 0 バイトを送ると、サーバに空ファイルが増える（実測で発生）。
        let (ino, handle) = {
            let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
            let ino = state.ino_for(&path);
            let handle = state.open(&path, Some(Vec::new()));
            (ino, handle)
        };
        reply.created(
            &TTL,
            &Self::empty_attr(ino, false),
            Generation(0),
            FileHandle(handle),
            FopenFlags::empty(),
        );
    }

    fn read(
        &self,
        _req: &Request,
        ino: INodeNo,
        _fh: FileHandle,
        offset: u64,
        size: u32,
        _flags: OpenFlags,
        _lock_owner: Option<LockOwner>,
        reply: ReplyData,
    ) {
        let path = {
            let state = self.state.lock().unwrap_or_else(|e| e.into_inner());
            state.path_of(ino.0)
        };
        let Some(path) = path else {
            reply.error(Errno::ENOENT);
            return;
        };

        // 未送信の内容があればそちらを優先する（書いた直後の read）。
        let pending = {
            let state = self.state.lock().unwrap_or_else(|e| e.into_inner());
            state
                .handles
                .values()
                .find(|o| o.path == path)
                .and_then(|o| o.pending.clone())
        };
        let data = match pending {
            Some(buf) => {
                let start = offset as usize;
                if start >= buf.len() {
                    Vec::new()
                } else {
                    let end = (start + size as usize).min(buf.len());
                    buf[start..end].to_vec()
                }
            }
            None => {
                match crate::runtime::block_on(self.fs.read_range(&path, offset, size as u64)) {
                    Ok(d) => d,
                    Err(e) => {
                        eprintln!("[fuse] {path} を読めませんでした: {e}");
                        reply.error(Errno::EIO);
                        return;
                    }
                }
            }
        };
        reply.data(&data);
    }

    fn write(
        &self,
        _req: &Request,
        _ino: INodeNo,
        fh: FileHandle,
        offset: u64,
        data: &[u8],
        _write_flags: WriteFlags,
        _flags: OpenFlags,
        _lock_owner: Option<LockOwner>,
        reply: ReplyWrite,
    ) {
        let ok = {
            let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
            match state.handles.get_mut(&fh.0) {
                Some(open) => {
                    let buf = open.pending.get_or_insert_with(Vec::new);
                    let start = offset as usize;
                    if start + data.len() > buf.len() {
                        buf.resize(start + data.len(), 0);
                    }
                    buf[start..start + data.len()].copy_from_slice(data);
                    true
                }
                None => false,
            }
        };
        if ok {
            reply.written(data.len() as u32);
        } else {
            reply.error(Errno::EBADF);
        }
    }

    fn flush(
        &self,
        _req: &Request,
        _ino: INodeNo,
        fh: FileHandle,
        _lock_owner: LockOwner,
        reply: ReplyEmpty,
    ) {
        match self.persist(fh.0) {
            Ok(()) => reply.ok(),
            Err(e) => reply.error(e),
        }
    }

    fn release(
        &self,
        _req: &Request,
        _ino: INodeNo,
        fh: FileHandle,
        _flags: OpenFlags,
        _lock_owner: Option<LockOwner>,
        _flush: bool,
        reply: ReplyEmpty,
    ) {
        // `release` は失敗を返せる。ここでも送らないと、`flush` を経ない
        // 書き込みが黙って消える。
        let result = self.persist(fh.0);
        self.state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .handles
            .remove(&fh.0);
        match result {
            Ok(()) => reply.ok(),
            Err(e) => reply.error(e),
        }
    }

    fn readdir(
        &self,
        _req: &Request,
        ino: INodeNo,
        _fh: FileHandle,
        offset: u64,
        mut reply: ReplyDirectory,
    ) {
        let path = {
            let state = self.state.lock().unwrap_or_else(|e| e.into_inner());
            state.path_of(ino.0)
        };
        let Some(path) = path else {
            reply.error(Errno::ENOENT);
            return;
        };
        let entries = match crate::runtime::block_on(self.fs.read_dir(&path)) {
            Ok(v) => v,
            Err(e) => {
                eprintln!("[fuse] {path} を列挙できませんでした: {e}");
                reply.error(Errno::EIO);
                return;
            }
        };

        // 0 番が `.`、1 番が `..`、以降が各項目。`offset` は「次に返す位置」。
        if offset == 0 && reply.add(INodeNo(ino.0), 1, FileType::Directory, ".") {
            reply.ok();
            return;
        }
        if offset <= 1 {
            let parent_ino = {
                let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
                let parent = parent_path(&path);
                state.ino_for(&parent)
            };
            if reply.add(INodeNo(parent_ino), 2, FileType::Directory, "..") {
                reply.ok();
                return;
            }
        }

        for (index, entry) in entries.iter().enumerate() {
            let position = index as u64 + 2;
            if position < offset {
                continue;
            }
            let child_path = normalize_path(&format!("{path}/{}", base_name(&entry.path)));
            let child_ino = self
                .state
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .ino_for(&child_path);
            let kind = if entry.isdir {
                FileType::Directory
            } else {
                FileType::RegularFile
            };
            if reply.add(INodeNo(child_ino), position + 1, kind, entry.display_name()) {
                break;
            }
        }
        reply.ok();
    }

    fn statfs(&self, _req: &Request, _ino: INodeNo, reply: ReplyStatfs) {
        let (total, free) = match &self.usage {
            Some(f) => f().unwrap_or((0, 0)),
            None => (0, 0),
        };
        let bsize = 4096u64;
        let blocks = total / bsize;
        let bfree = free / bsize;
        reply.statfs(blocks, bfree, bfree, 0, 0, bsize as u32, 255, bsize as u32);
    }
}

/// FUSE でマウントする。アンマウントされるまで戻らない。
pub async fn mount(
    fs: Arc<dyn RemoteFs + Send + Sync>,
    mountpoint: &Path,
    usage: Option<Arc<dyn Fn() -> Option<(u64, u64)> + Send + Sync>>,
) -> anyhow::Result<()> {
    if !mountpoint.exists() {
        anyhow::bail!("マウント先がありません: {}", mountpoint.display());
    }
    if !mountpoint.is_dir() {
        anyhow::bail!(
            "マウント先がディレクトリではありません: {}",
            mountpoint.display()
        );
    }

    // `Config` は non_exhaustive なので構造体リテラルで組み立てられない。
    // 既定を作ってから項目を差し替える。
    //
    // `AutoUnmount` は `acl != Owner`（AllowOther / AllowRoot）を要求する。
    // 実測: Owner のまま付けると `auto_unmount requires acl != Owner` で
    // マウントが失敗する。他の利用者へ見せる必要は無いので付けない。
    let mut config = Config::default();
    config.mount_options = vec![
        MountOption::FSName("silentbox".to_string()),
        MountOption::DefaultPermissions,
    ];

    let mut filesystem = TeraBoxFuse::new(fs);
    if let Some(usage) = usage {
        filesystem = filesystem.with_usage(usage);
    }

    let point = mountpoint.to_path_buf();
    // FUSE のループはブロッキングなので、専用スレッドへ逃がす。
    tokio::task::spawn_blocking(move || {
        fuser::mount(filesystem, &point, &config)
            .map_err(|e| anyhow::anyhow!("FUSE マウントに失敗しました: {e}"))
    })
    .await?
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn root_is_ino_one() {
        let state = State::new();
        assert_eq!(state.path_of(1).as_deref(), Some("/"));
    }

    #[test]
    fn ino_assignment_is_stable() {
        let mut state = State::new();
        let a = state.ino_for("/a");
        let b = state.ino_for("/a");
        assert_eq!(a, b);
        assert_ne!(a, state.ino_for("/b"));
        assert!(a >= 2, "0 と 1 は予約");
    }

    #[test]
    fn forget_drops_both_maps() {
        let mut state = State::new();
        let ino = state.ino_for("/gone");
        assert!(state.path_of(ino).is_some());
        state.forget("/gone");
        assert!(state.path_of(ino).is_none());
        assert!(state.inos.get("/gone").is_none());
    }

    #[test]
    fn join_normalizes() {
        assert_eq!(TeraBoxFuse::join("/a", OsStr::new("b")), "/a/b");
        assert_eq!(TeraBoxFuse::join("/", OsStr::new("b")), "/b");
    }

    #[test]
    fn handles_are_unique_and_carry_path() {
        let mut state = State::new();
        let h1 = state.open("/x", Some(Vec::new()));
        let h2 = state.open("/y", None);
        assert_ne!(h1, h2);
        assert_eq!(state.handles[&h1].path, "/x");
        assert_eq!(state.handles[&h2].path, "/y");
    }

    #[test]
    fn empty_attr_matches_kind() {
        let dir = TeraBoxFuse::empty_attr(2, true);
        assert_eq!(dir.kind, FileType::Directory);
        let file = TeraBoxFuse::empty_attr(3, false);
        assert_eq!(file.kind, FileType::RegularFile);
        assert_eq!(file.size, 0);
    }
}
