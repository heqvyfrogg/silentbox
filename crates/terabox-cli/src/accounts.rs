//! アカウントの索引。
//!
//! 秘密情報はここに置かない。名前・`uk`（表示用の識別子）・登録時刻だけを持つ。
//! 実体（`ndus` / `jsToken` / `bdstoken`）は [`crate::secret`] が預かる。

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// 索引に載る 1 件。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AccountEntry {
    /// 利用者が付けた名前。
    pub name: String,
    /// サーバが返した `uk`。未取得なら `None`。
    #[serde(default)]
    pub uk: Option<String>,
    /// 登録時刻（epoch 秒）。
    #[serde(default)]
    pub added_at: i64,
}

/// 索引ファイル全体。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Index {
    /// 既定で使うアカウント名。
    #[serde(default)]
    pub current: String,
    /// 登録済みのアカウント。
    #[serde(default)]
    pub accounts: Vec<AccountEntry>,
}

/// 名前として許す文字数。
const MAX_NAME_LEN: usize = 32;

/// 旧版が使っていた既定名。移行の受け皿として予約する。
pub const DEFAULT_NAME: &str = "default";

/// 名前を検証する。空白・制御文字・パス区切りを受けない。
///
/// 名前は資格情報マネージャの「アカウント名」とファイル名の両方に使うため、
/// ここで一度だけ厳しく弾く（後段で別々の検証を書かない）。
pub fn validate_name(name: &str) -> Result<(), String> {
    if name.is_empty() {
        return Err("アカウント名が空です".to_string());
    }
    if name.chars().count() > MAX_NAME_LEN {
        return Err(format!(
            "アカウント名が長すぎます（{MAX_NAME_LEN} 文字まで）: {name}"
        ));
    }
    if !name
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-'))
    {
        return Err(format!(
            "アカウント名に使えない文字が含まれています（英数字・_・- のみ）: {name}"
        ));
    }
    if name == "." || name == ".." {
        return Err(format!("アカウント名として使えません: {name}"));
    }
    Ok(())
}

/// 索引ファイルのパス。
pub fn index_path(home: &Path) -> PathBuf {
    home.join("accounts.toml")
}

/// 索引を読む。無ければ空の索引（`current` は空）を返す。
pub fn load(home: &Path) -> Result<Index, String> {
    let path = index_path(home);
    if !path.exists() {
        return Ok(Index::default());
    }
    let text = std::fs::read_to_string(&path)
        .map_err(|e| format!("{} を読めませんでした: {e}", path.display()))?;
    let mut index: Index = toml::from_str(&text)
        .map_err(|e| format!("{} の解釈に失敗しました: {e}", path.display()))?;
    // 壊れた名前が混じっていても起動は止めないが、索引からは落とす。
    index.accounts.retain(|a| validate_name(&a.name).is_ok());
    Ok(index)
}

/// 索引を書く。ディレクトリが無ければ作る。
pub fn save(home: &Path, index: &Index) -> Result<(), String> {
    std::fs::create_dir_all(home)
        .map_err(|e| format!("{} を作れませんでした: {e}", home.display()))?;
    let text = toml::to_string_pretty(index)
        .map_err(|e| format!("索引を組み立てられませんでした: {e}"))?;
    let path = index_path(home);
    std::fs::write(&path, text)
        .map_err(|e| format!("{} へ書けませんでした: {e}", path.display()))
}

impl Index {
    /// 名前で引く。
    pub fn find(&self, name: &str) -> Option<&AccountEntry> {
        self.accounts.iter().find(|a| a.name == name)
    }

    /// 登録する。同じ名前があれば `uk` と時刻を更新する。
    ///
    /// 戻り値は「新規だったか」。呼び出し側が上書きを表示できるようにする。
    pub fn upsert(&mut self, name: &str, uk: Option<String>, now: i64) -> bool {
        match self.accounts.iter_mut().find(|a| a.name == name) {
            Some(existing) => {
                existing.uk = uk;
                existing.added_at = now;
                false
            }
            None => {
                self.accounts.push(AccountEntry {
                    name: name.to_string(),
                    uk,
                    added_at: now,
                });
                true
            }
        }
    }

    /// 索引から外す。戻り値は外せたか。
    pub fn remove(&mut self, name: &str) -> bool {
        let before = self.accounts.len();
        self.accounts.retain(|a| a.name != name);
        let removed = self.accounts.len() != before;
        if removed && self.current == name {
            // 既定が消えたら、残っている先頭へ寄せる（空なら空文字）。
            self.current = self.accounts.first().map(|a| a.name.clone()).unwrap_or_default();
        }
        removed
    }

    /// 既定を切り替える。登録済みでない名前は拒む（綴り間違いで既定が
    /// 消えるのを防ぐ）。
    pub fn set_current(&mut self, name: &str) -> Result<(), String> {
        validate_name(name)?;
        if self.find(name).is_none() {
            return Err(format!(
                "アカウント {name} は登録されていません。`tbx login --name {name}` で追加してください。"
            ));
        }
        self.current = name.to_string();
        Ok(())
    }

    /// 登録済みの名前の集合（重複検出用）。
    #[allow(dead_code)]
    pub fn names(&self) -> BTreeSet<&str> {
        self.accounts.iter().map(|a| a.name.as_str()).collect()
    }
}

/// 使うアカウント名を決める。
///
/// 優先順: `--account` > 環境変数 `TBX_ACCOUNT` > 索引の `current` > `default`。
/// 明示指定（flag / env）は登録済みでなくてもそのまま返す。まだログインして
/// いない名前へ `login --name` できるようにするため。
pub fn resolve(home: &Path, flag: Option<&str>) -> Result<String, String> {
    if let Some(name) = flag {
        validate_name(name)?;
        return Ok(name.to_string());
    }
    if let Ok(from_env) = std::env::var("TBX_ACCOUNT") {
        let trimmed = from_env.trim();
        if !trimmed.is_empty() {
            validate_name(trimmed)?;
            return Ok(trimmed.to_string());
        }
    }
    let index = load(home)?;
    if !index.current.is_empty() {
        return Ok(index.current);
    }
    Ok(DEFAULT_NAME.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_home(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "tbx-accounts-test-{}-{tag}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn rejects_bad_names() {
        assert!(validate_name("").is_err());
        assert!(validate_name("with space").is_err());
        assert!(validate_name("a/b").is_err());
        assert!(validate_name("..").is_err());
        assert!(validate_name(&"x".repeat(33)).is_err());
    }

    #[test]
    fn accepts_expected_names() {
        for ok in ["work", "sub-1", "a_b", "abc123"] {
            assert!(validate_name(ok).is_ok(), "{ok} は通るべき");
        }
    }

    #[test]
    fn upsert_is_idempotent_and_reports_novelty() {
        let mut index = Index::default();
        assert!(index.upsert("work", Some("uk-1".into()), 100));
        assert!(!index.upsert("work", Some("uk-2".into()), 200));
        assert_eq!(index.accounts.len(), 1);
        assert_eq!(index.find("work").unwrap().uk.as_deref(), Some("uk-2"));
        assert_eq!(index.find("work").unwrap().added_at, 200);
    }

    #[test]
    fn removing_current_falls_back_to_remaining() {
        let mut index = Index::default();
        index.upsert("a", None, 1);
        index.upsert("b", None, 2);
        index.current = "a".into();
        assert!(index.remove("a"));
        assert_eq!(index.current, "b");
        assert!(!index.remove("a"));
    }

    #[test]
    fn set_current_requires_registration() {
        let mut index = Index::default();
        index.upsert("work", None, 1);
        assert!(index.set_current("work").is_ok());
        assert_eq!(index.current, "work");
        assert!(index.set_current("nope").is_err());
    }

    #[test]
    fn roundtrip_is_stable() {
        let home = temp_home("roundtrip");
        let mut index = Index::default();
        index.upsert("work", Some("uk-1".into()), 42);
        index.set_current("work").unwrap();
        save(&home, &index).unwrap();
        let back = load(&home).unwrap();
        assert_eq!(back.current, "work");
        assert_eq!(back.accounts.len(), 1);
        assert_eq!(back.accounts[0].uk.as_deref(), Some("uk-1"));
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn corrupt_names_are_dropped_on_load() {
        let home = temp_home("corrupt");
        std::fs::write(
            index_path(&home),
            "current = \"good\"\n\n[[accounts]]\nname = \"bad name\"\n\n[[accounts]]\nname = \"good\"\n",
        )
        .unwrap();
        let back = load(&home).unwrap();
        assert_eq!(back.accounts.len(), 1);
        assert_eq!(back.accounts[0].name, "good");
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn resolve_prefers_flag_then_current_then_default() {
        let home = temp_home("resolve");
        // 索引が無い状態では default。
        assert_eq!(resolve(&home, None).unwrap(), DEFAULT_NAME);
        // flag は索引を見ずに通る。
        assert_eq!(resolve(&home, Some("fresh")).unwrap(), "fresh");

        let mut index = Index::default();
        index.upsert("work", None, 1);
        index.set_current("work").unwrap();
        save(&home, &index).unwrap();
        assert_eq!(resolve(&home, None).unwrap(), "work");
        // flag は current より優先。
        assert_eq!(resolve(&home, Some("other")).unwrap(), "other");
        let _ = std::fs::remove_dir_all(&home);
    }
}
