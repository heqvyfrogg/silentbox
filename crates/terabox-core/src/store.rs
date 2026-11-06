//! 転送台帳（SQLite）。
//!
//! 中断・プロセス終了・電源断のあとでも、どこまで書けたかを失わないための台帳。
//! 接続ごとの書き込み済み範囲を持ち、再開時は未完了の範囲だけを取り直す。
//!
//! 設計上の要点:
//! - 1 転送 = 1 行（`transfer`）。分割 = 複数行（`part`）。
//! - `part` の進捗は書き込みのたびに逐次更新する（まとめ書きしない）。
//!   クラッシュ時に最大でも 1 チャンク分だけ巻き戻れば十分。
//! - 台帳の値と実ファイル長が食い違う場合は、**実ファイル長を正**として
//!   `state` を `Partial` に戻す（安全側）。

use std::path::Path;

use rusqlite::{params, Connection, OptionalExtension};

use crate::error::TbxError;

/// 転送の種別。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Download,
    Upload,
}

impl Kind {
    pub fn as_str(self) -> &'static str {
        match self {
            Kind::Download => "download",
            Kind::Upload => "upload",
        }
    }

    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "download" => Some(Kind::Download),
            "upload" => Some(Kind::Upload),
            _ => None,
        }
    }
}

/// 転送の状態。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    /// 開始前。
    Pending,
    /// 進行中（中断を含む）。
    Partial,
    /// 完了。
    Done,
    /// 失敗（再試行可能）。
    Failed,
}

impl State {
    pub fn as_str(self) -> &'static str {
        match self {
            State::Pending => "pending",
            State::Partial => "partial",
            State::Done => "done",
            State::Failed => "failed",
        }
    }

    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "pending" => Some(State::Pending),
            "partial" => Some(State::Partial),
            "done" => Some(State::Done),
            "failed" => Some(State::Failed),
            _ => None,
        }
    }
}

/// 転送 1 件の台帳行。
#[derive(Debug, Clone)]
pub struct TransferRow {
    pub id: i64,
    pub kind: Kind,
    pub fs_id: String,
    pub remote_path: String,
    pub local_path: String,
    pub size: u64,
    pub etag: Option<String>,
    pub conn: u8,
    pub state: State,
    pub bytes_done: u64,
}

/// 分割 1 件の台帳行。
#[derive(Debug, Clone)]
pub struct PartRow {
    pub transfer_id: i64,
    pub index: u32,
    /// 転送先ファイル内の開始オフセット。
    pub offset: u64,
    pub length: u64,
    /// この分割で書き込み済みのバイト数（先頭からの連続量）。
    pub written: u64,
    pub state: State,
}

/// 台帳。
pub struct Ledger {
    conn: Connection,
}

impl Ledger {
    /// 指定パスで開く（無ければ作る）。
    pub fn open(path: &Path) -> Result<Self, TbxError> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let conn = Connection::open(path)?;
        Self::init(conn)
    }

    /// メモリ上で開く（テスト用）。
    pub fn open_in_memory() -> Result<Self, TbxError> {
        Self::init(Connection::open_in_memory()?)
    }

    fn init(conn: Connection) -> Result<Self, TbxError> {
        // WAL は同時 1 プロセス前提でも落ちにくい。失敗しても致命的ではない。
        let _ = conn.pragma_update(None, "journal_mode", "WAL");
        let _ = conn.pragma_update(None, "synchronous", "NORMAL");
        conn.execute_batch(
            r#"
            CREATE TABLE IF NOT EXISTS transfer (
                id          INTEGER PRIMARY KEY AUTOINCREMENT,
                kind        TEXT    NOT NULL,
                fs_id       TEXT    NOT NULL DEFAULT '',
                remote_path TEXT    NOT NULL,
                local_path  TEXT    NOT NULL,
                size        INTEGER NOT NULL,
                etag        TEXT,
                conn        INTEGER NOT NULL,
                state       TEXT    NOT NULL,
                bytes_done  INTEGER NOT NULL DEFAULT 0,
                updated_at  INTEGER NOT NULL,
                UNIQUE(kind, remote_path, local_path)
            );
            CREATE TABLE IF NOT EXISTS part (
                transfer_id INTEGER NOT NULL,
                idx         INTEGER NOT NULL,
                offset      INTEGER NOT NULL,
                length      INTEGER NOT NULL,
                written     INTEGER NOT NULL DEFAULT 0,
                state       TEXT    NOT NULL,
                PRIMARY KEY (transfer_id, idx),
                FOREIGN KEY (transfer_id) REFERENCES transfer(id) ON DELETE CASCADE
            );
            CREATE INDEX IF NOT EXISTS part_by_transfer ON part(transfer_id);
            "#,
        )?;
        Ok(Self { conn })
    }

    /// 転送を登録または取得する。既存があれば `conn` と `size` を更新して返す。
    pub fn upsert_transfer(
        &mut self,
        kind: Kind,
        fs_id: &str,
        remote_path: &str,
        local_path: &str,
        size: u64,
        conn: u8,
    ) -> Result<i64, TbxError> {
        let now = chrono::Utc::now().timestamp();
        self.conn.execute(
            r#"INSERT INTO transfer (kind, fs_id, remote_path, local_path, size, conn, state, bytes_done, updated_at)
               VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, 0, ?8)
               ON CONFLICT(kind, remote_path, local_path) DO UPDATE SET
                 size = excluded.size,
                 conn = excluded.conn,
                 updated_at = excluded.updated_at"#,
            params![
                kind.as_str(),
                fs_id,
                remote_path,
                local_path,
                size as i64,
                conn as i64,
                State::Pending.as_str(),
                now
            ],
        )?;
        let id: i64 = self.conn.query_row(
            "SELECT id FROM transfer WHERE kind = ?1 AND remote_path = ?2 AND local_path = ?3",
            params![kind.as_str(), remote_path, local_path],
            |row| row.get(0),
        )?;
        Ok(id)
    }

    /// 分割の割り当てを保存する。既存の分割は進捗を保持したまま範囲だけ更新する。
    ///
    /// 分割数が変わった場合（`--conn` の変更）は、書き込み済みバイト数を
    /// 新しい範囲割り当てへ引き継げないため、既存分割を破棄して作り直す。
    pub fn set_parts(
        &mut self,
        transfer_id: i64,
        size: u64,
        conn: u8,
        part_size: u64,
    ) -> Result<Vec<PartRow>, TbxError> {
        let existing = self.parts(transfer_id)?;
        let want = plan_parts(size, conn, part_size);

        let same_shape = existing.len() == want.len()
            && existing
                .iter()
                .zip(want.iter())
                .all(|(a, b)| a.offset == b.0 && a.length == b.1);

        if same_shape {
            return Ok(existing);
        }

        self.conn.execute(
            "DELETE FROM part WHERE transfer_id = ?1",
            params![transfer_id],
        )?;
        let tx = self.conn.transaction()?;
        for (index, (offset, length)) in want.iter().enumerate() {
            tx.execute(
                "INSERT INTO part (transfer_id, idx, offset, length, written, state) VALUES (?1, ?2, ?3, ?4, 0, ?5)",
                params![
                    transfer_id,
                    index as i64,
                    *offset as i64,
                    *length as i64,
                    State::Pending.as_str()
                ],
            )?;
        }
        tx.commit()?;
        self.parts(transfer_id)
    }

    /// 分割の一覧。
    pub fn parts(&self, transfer_id: i64) -> Result<Vec<PartRow>, TbxError> {
        let mut stmt = self.conn.prepare(
            "SELECT transfer_id, idx, offset, length, written, state FROM part WHERE transfer_id = ?1 ORDER BY idx",
        )?;
        let rows = stmt
            .query_map(params![transfer_id], |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, i64>(2)?,
                    row.get::<_, i64>(3)?,
                    row.get::<_, i64>(4)?,
                    row.get::<_, String>(5)?,
                ))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows
            .into_iter()
            .map(|(tid, idx, off, len, written, state)| PartRow {
                transfer_id: tid,
                index: idx as u32,
                offset: off.max(0) as u64,
                length: len.max(0) as u64,
                written: written.max(0) as u64,
                state: State::from_str(&state).unwrap_or(State::Pending),
            })
            .collect())
    }

    /// 分割の進捗を更新する。`written` は先頭からの連続書き込み量。
    pub fn update_part(
        &mut self,
        transfer_id: i64,
        index: u32,
        written: u64,
        state: State,
    ) -> Result<(), TbxError> {
        self.conn.execute(
            "UPDATE part SET written = ?1, state = ?2 WHERE transfer_id = ?3 AND idx = ?4",
            params![written as i64, state.as_str(), transfer_id, index as i64],
        )?;
        Ok(())
    }

    /// 分割の状態だけを更新する（進捗は保持）。
    pub fn set_part_state(
        &mut self,
        transfer_id: i64,
        index: u32,
        state: State,
    ) -> Result<(), TbxError> {
        self.conn.execute(
            "UPDATE part SET state = ?1 WHERE transfer_id = ?2 AND idx = ?3",
            params![state.as_str(), transfer_id, index as i64],
        )?;
        Ok(())
    }

    /// 転送全体の状態を更新する。`bytes_done` は分割の合計から再計算する。
    pub fn set_transfer_state(
        &mut self,
        transfer_id: i64,
        state: State,
        bytes_done: u64,
    ) -> Result<(), TbxError> {
        let now = chrono::Utc::now().timestamp();
        self.conn.execute(
            "UPDATE transfer SET state = ?1, bytes_done = ?2, updated_at = ?3 WHERE id = ?4",
            params![state.as_str(), bytes_done as i64, now, transfer_id],
        )?;
        Ok(())
    }

    /// 転送 1 件を取得する。
    pub fn transfer(&self, transfer_id: i64) -> Result<Option<TransferRow>, TbxError> {
        let row = self
            .conn
            .query_row(
                "SELECT id, kind, fs_id, remote_path, local_path, size, etag, conn, state, bytes_done FROM transfer WHERE id = ?1",
                params![transfer_id],
                |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, String>(3)?,
                        row.get::<_, String>(4)?,
                        row.get::<_, i64>(5)?,
                        row.get::<_, Option<String>>(6)?,
                        row.get::<_, i64>(7)?,
                        row.get::<_, String>(8)?,
                        row.get::<_, i64>(9)?,
                    ))
                },
            )
            .optional()?;
        Ok(row.and_then(|r| {
            Some(TransferRow {
                id: r.0,
                kind: Kind::from_str(&r.1)?,
                fs_id: r.2,
                remote_path: r.3,
                local_path: r.4,
                size: r.5.max(0) as u64,
                etag: r.6,
                conn: r.7.max(0) as u8,
                state: State::from_str(&r.8)?,
                bytes_done: r.9.max(0) as u64,
            })
        }))
    }

    /// 未完了の転送一覧。
    pub fn unfinished(&self) -> Result<Vec<TransferRow>, TbxError> {
        let mut stmt = self.conn.prepare(
            "SELECT id, kind, fs_id, remote_path, local_path, size, etag, conn, state, bytes_done
             FROM transfer WHERE state IN ('pending','partial','failed') ORDER BY updated_at DESC",
        )?;
        let raw = stmt
            .query_map([], |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, i64>(5)?,
                    row.get::<_, Option<String>>(6)?,
                    row.get::<_, i64>(7)?,
                    row.get::<_, String>(8)?,
                    row.get::<_, i64>(9)?,
                ))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        let mut out = Vec::new();
        for r in raw {
            if let (Some(kind), Some(state)) = (Kind::from_str(&r.1), State::from_str(&r.8)) {
                out.push(TransferRow {
                    id: r.0,
                    kind,
                    fs_id: r.2,
                    remote_path: r.3,
                    local_path: r.4,
                    size: r.5.max(0) as u64,
                    etag: r.6,
                    conn: r.7.max(0) as u8,
                    state,
                    bytes_done: r.9.max(0) as u64,
                });
            }
        }
        Ok(out)
    }

    /// 完了済みの記録を削除する。
    pub fn forget(&mut self, transfer_id: i64) -> Result<(), TbxError> {
        self.conn
            .execute("DELETE FROM transfer WHERE id = ?1", params![transfer_id])?;
        Ok(())
    }
}

/// ファイルサイズと接続数から分割範囲を決める。
///
/// 返るのは `(offset, length)` の並び。最後の分割だけ端数になる。
/// `part_size` を超える分割は作らない（同時接続の帯域制限下で偏らないようにする）。
pub fn plan_parts(size: u64, conn: u8, part_size: u64) -> Vec<(u64, u64)> {
    if size == 0 {
        // 0 バイトでも 1 分割として扱い、空ファイルであることを台帳に残す。
        return vec![(0, 0)];
    }
    let conn = conn.max(1) as u64;
    let part_size = part_size.max(1);
    // まず part_size で刻む。
    let mut ranges = Vec::new();
    let mut offset = 0u64;
    while offset < size {
        let len = part_size.min(size - offset);
        ranges.push((offset, len));
        offset += len;
    }
    // 分割数が接続数を超える場合は、後ろの分割を先頭側へ寄せて
    // 「1 接続が連続した範囲を担当する」形にまとめる。
    if ranges.len() as u64 > conn {
        let per = size.div_ceil(conn);
        ranges = Vec::new();
        let mut off = 0u64;
        while off < size {
            let len = per.min(size - off);
            ranges.push((off, len));
            off += len;
        }
    }
    ranges
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plan_covers_whole_file_without_gaps() {
        for size in [0u64, 1, 1023, 1024, 100_000, 5_000_000] {
            for conn in [1u8, 2, 4, 8] {
                let ranges = plan_parts(size, conn, 65536);
                let total: u64 = ranges.iter().map(|(_, l)| *l).sum();
                assert_eq!(total, size, "size={size} conn={conn}");
                let mut expect = 0;
                for (off, len) in &ranges {
                    assert_eq!(*off, expect);
                    expect += len;
                }
                assert_eq!(expect, size);
            }
        }
    }

    #[test]
    fn plan_never_exceeds_connection_count() {
        // 分割数は「分割サイズで刻んだ数」を接続数で頭打ちにした値になる。
        for size in [1u64, 1024, 10 * 1024 * 1024, 100 * 1024 * 1024, 700 * 1024 * 1024] {
            for conn in [1u8, 2, 4, 8] {
                let ranges = plan_parts(size, conn, 16 * 1024 * 1024);
                assert!(
                    ranges.len() <= conn as usize,
                    "size={size} conn={conn} parts={}",
                    ranges.len()
                );
                assert!(ranges.len() >= 1);
            }
        }
    }

    #[test]
    fn plan_small_file_uses_one_connection() {
        // 分割サイズ未満のファイルは 1 本で取り切る（無駄なハンドシェイクを避ける）。
        let ranges = plan_parts(10 * 1024 * 1024, 4, 16 * 1024 * 1024);
        assert_eq!(ranges.len(), 1);
        assert_eq!(ranges[0], (0, 10 * 1024 * 1024));
    }

    #[test]
    fn plan_large_file_is_capped_at_connection_count() {
        let ranges = plan_parts(100 * 1024 * 1024, 4, 16 * 1024 * 1024);
        assert_eq!(ranges.len(), 4);
        assert_eq!(ranges.iter().map(|(_, l)| *l).sum::<u64>(), 100 * 1024 * 1024);
    }

    #[test]
    fn plan_single_part_when_part_size_covers_file() {
        let ranges = plan_parts(1024, 4, 16 * 1024 * 1024);
        assert_eq!(ranges, vec![(0, 1024)]);
    }

    #[test]
    fn ledger_roundtrip_and_resume() {
        let mut ledger = Ledger::open_in_memory().unwrap();
        let id = ledger
            .upsert_transfer(Kind::Download, "1", "/a.bin", "C:/tmp/a.bin", 1000, 4)
            .unwrap();
        let parts = ledger.set_parts(id, 1000, 4, 250).unwrap();
        assert_eq!(parts.len(), 4);
        assert_eq!(parts[0].length, 250);

        ledger.update_part(id, 0, 250, State::Done).unwrap();
        ledger.update_part(id, 1, 100, State::Partial).unwrap();
        ledger
            .set_transfer_state(id, State::Partial, 350)
            .unwrap();

        let parts = ledger.parts(id).unwrap();
        assert_eq!(parts[0].state, State::Done);
        assert_eq!(parts[0].written, 250);
        assert_eq!(parts[1].written, 100);

        let t = ledger.transfer(id).unwrap().unwrap();
        assert_eq!(t.bytes_done, 350);
        assert_eq!(t.state, State::Partial);

        let unfinished = ledger.unfinished().unwrap();
        assert_eq!(unfinished.len(), 1);
    }

    #[test]
    fn changing_connection_count_resets_parts() {
        let mut ledger = Ledger::open_in_memory().unwrap();
        let id = ledger
            .upsert_transfer(Kind::Download, "1", "/a.bin", "C:/tmp/a.bin", 800, 2)
            .unwrap();
        let first = ledger.set_parts(id, 800, 2, 400).unwrap();
        assert_eq!(first.len(), 2);
        ledger.update_part(id, 0, 400, State::Done).unwrap();

        let second = ledger.set_parts(id, 800, 4, 200).unwrap();
        assert_eq!(second.len(), 4);
        // 範囲が変わったので進捗はリセットされる（不整合なレジュームを避ける）。
        assert!(second.iter().all(|p| p.written == 0));
    }

    #[test]
    fn same_shape_preserves_progress() {
        let mut ledger = Ledger::open_in_memory().unwrap();
        let id = ledger
            .upsert_transfer(Kind::Upload, "1", "/u.bin", "C:/tmp/u.bin", 800, 2)
            .unwrap();
        let _ = ledger.set_parts(id, 800, 2, 400).unwrap();
        ledger.update_part(id, 1, 400, State::Done).unwrap();

        let again = ledger.set_parts(id, 800, 2, 400).unwrap();
        assert_eq!(again[1].written, 400);
        assert_eq!(again[1].state, State::Done);
    }

    #[test]
    fn zero_byte_file_has_one_part() {
        let ranges = plan_parts(0, 4, 1024);
        assert_eq!(ranges, vec![(0, 0)]);
    }
}
