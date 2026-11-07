//! rclone crypt 互換の暗号化。
//!
//! 仕様は rclone の `backend/crypt/cipher.go` と `docs/content/crypt.md`、
//! EME は `github.com/rfjakob/eme` を根拠にしている。互換であることの意味は
//! 「同じパスワードと同じ salt で、rclone が復号できる」ことなので、実装の
//! 正否は rclone 本体との突き合わせで決める（自己申告では検証にならない）。
//!
//! - 鍵導出: scrypt(N=16384, r=8, p=1) で 80 バイト。
//!   `dataKey`(32) + `nameKey`(32) + `nameTweak`(16) に分割する。
//! - 内容: NaCl SecretBox（XSalsa20-Poly1305）。64 KiB のチャンク。
//!   ファイル先頭に `RCLONE\0\0` + 24 バイト nonce。
//! - 名前: AES-256 の EME。16 バイト単位に PKCS#7 でパディングし、
//!   base32hex（小文字・パディング無し）で表す。
//!
//! ## バイト順（実測で確定）
//! Go の `secretbox.Seal` も Rust の `aead` も「暗号文 || タグ」の順で返す。
//! rclone が作った実ファイルをそのまま復号できたため、並べ替えは行わない。

use std::path::PathBuf;

use data_encoding::{BASE32HEX_NOPAD, HEXLOWER};
use scrypt::{scrypt, Params};
use zeroize::Zeroize;

use crate::error::TbxError;

/// ファイル先頭の識別子（8 バイト）。
pub const FILE_MAGIC: &[u8; 8] = b"RCLONE\x00\x00";

/// nonce の長さ。
pub const NONCE_SIZE: usize = 24;

/// ヘッダ全体（magic + nonce）。
pub const HEADER_SIZE: usize = 8 + NONCE_SIZE;

/// 認証タグの長さ（SecretBox のオーバーヘッド）。
pub const TAG_SIZE: usize = 16;

/// 1 チャンクの平文バイト数。
pub const CHUNK_SIZE: usize = 64 * 1024;

/// 暗号化された 1 チャンクの最大長（タグ + 平文）。
pub const CHUNK_MAX: usize = TAG_SIZE + CHUNK_SIZE;

/// 名前の暗号ブロック長。
pub const NAME_BLOCK: usize = 16;

/// salt を省いたときに rclone が使う固定値。
const DEFAULT_SALT: [u8; 16] = [
    0xA8, 0x0D, 0xF4, 0x3A, 0x8F, 0xBD, 0x03, 0x08, 0xA7, 0xCA, 0xB8, 0x3E, 0x58, 0x1F, 0x86, 0xB1,
];

/// 暗号化されたファイル名の末尾に付く拡張子（`off` モードのみ）。
pub const DEFAULT_SUFFIX: &str = ".bin";

/// 設定の保存形式。秘密（パスワード）は含めない。
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
pub struct CryptConfig {
    /// 暗号化を有効にするか。**既定は false**。
    #[serde(default)]
    pub enabled: bool,
    /// パスワードを預ける資格情報ストアのキー名。
    #[serde(default = "default_secret_key")]
    pub secret_key: String,
    /// salt（`password2`）。空なら rclone の既定 salt を使う。
    #[serde(default)]
    pub salt: String,
    /// ファイル名の暗号化方式（`standard` / `off` / `obfuscate`）。
    #[serde(default = "default_filename_mode")]
    pub filename_encryption: String,
    /// ディレクトリ名も暗号化するか。
    #[serde(default = "default_true")]
    pub directory_name_encryption: bool,
}

fn default_secret_key() -> String {
    "crypt-password".to_string()
}

fn default_filename_mode() -> String {
    "standard".to_string()
}

fn default_true() -> bool {
    true
}

impl Default for CryptConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            secret_key: default_secret_key(),
            salt: String::new(),
            filename_encryption: default_filename_mode(),
            directory_name_encryption: true,
        }
    }
}

impl CryptConfig {
    /// 設定ファイルのパス。
    pub fn path() -> PathBuf {
        crate::config::tbx_home().join("crypt.toml")
    }

    /// 読み込む。無ければ既定（無効）。
    pub fn load() -> Result<Self, TbxError> {
        let path = Self::path();
        if !path.exists() {
            return Ok(Self::default());
        }
        let text = std::fs::read_to_string(&path)?;
        let cfg: Self = toml::from_str(&text)
            .map_err(|e| TbxError::Other(format!("{} の解釈に失敗しました: {e}", path.display())))?;
        Ok(cfg)
    }

    /// 保存する。秘密は書かない。
    pub fn save(&self) -> Result<(), TbxError> {
        let path = Self::path();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let text = toml::to_string_pretty(self)
            .map_err(|e| TbxError::Other(format!("暗号化設定を組み立てられませんでした: {e}")))?;
        std::fs::write(&path, text)?;
        Ok(())
    }

    /// 名前の暗号化方式を解釈する。
    pub fn name_mode(&self) -> Result<NameMode, TbxError> {
        match self.filename_encryption.as_str() {
            "standard" => Ok(NameMode::Standard),
            "off" => Ok(NameMode::Off),
            other => Err(TbxError::Other(format!(
                "filename_encryption は standard か off を指定してください（受け取った値: {other}）。obfuscate は未対応です"
            ))),
        }
    }
}

/// ファイル名の暗号化方式。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NameMode {
    /// 暗号化する（既定）。
    Standard,
    /// 暗号化せず、`.bin` を付けるだけ。
    Off,
}

/// 鍵素材から組み立てた暗号器。
///
/// `Debug` は鍵を出さない。
pub struct Cipher {
    data_key: [u8; 32],
    name_key: [u8; 32],
    name_tweak: [u8; 16],
    mode: NameMode,
    encrypt_dir_names: bool,
    suffix: String,
}

impl std::fmt::Debug for Cipher {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Cipher")
            .field("data_key", &"<伏せた>")
            .field("name_key", &"<伏せた>")
            .field("name_tweak", &"<伏せた>")
            .field("mode", &self.mode)
            .field("encrypt_dir_names", &self.encrypt_dir_names)
            .field("suffix", &self.suffix)
            .finish()
    }
}

impl Drop for Cipher {
    fn drop(&mut self) {
        self.data_key.zeroize();
        self.name_key.zeroize();
        self.name_tweak.zeroize();
    }
}

impl Cipher {
    /// パスワードと salt から鍵を作る。
    pub fn new(password: &str, salt: &str, config: &CryptConfig) -> Result<Self, TbxError> {
        let mode = config.name_mode()?;
        let mut key = [0u8; 80];

        // `Params::new` の `len` は 10..=64 しか許さないが、`scrypt()` は
        // 出力長を `output` スライスから取る（`len` は計算に使われない）。
        // rclone は 80 バイト必要なので、妥当な `len` を渡して 80 バイトの
        // 出力スライスを渡す。
        let params = Params::new(14, 8, 1, 32)
            .map_err(|e| TbxError::Other(format!("scrypt のパラメータが不正です: {e}")))?;
        let salt_bytes: Vec<u8> = if salt.is_empty() {
            DEFAULT_SALT.to_vec()
        } else {
            salt.as_bytes().to_vec()
        };
        scrypt(password.as_bytes(), &salt_bytes, &params, &mut key)
            .map_err(|e| TbxError::Other(format!("鍵を導出できませんでした: {e}")))?;

        let mut data_key = [0u8; 32];
        let mut name_key = [0u8; 32];
        let mut name_tweak = [0u8; 16];
        data_key.copy_from_slice(&key[0..32]);
        name_key.copy_from_slice(&key[32..64]);
        name_tweak.copy_from_slice(&key[64..80]);
        key.zeroize();


        Ok(Self {
            data_key,
            name_key,
            name_tweak,
            mode,
            encrypt_dir_names: config.directory_name_encryption,
            suffix: DEFAULT_SUFFIX.to_string(),
        })
    }

    // ---- 名前 ----

    /// 1 セグメントを暗号化する。
    pub fn encrypt_segment(&self, plaintext: &str) -> String {
        if plaintext.is_empty() {
            return String::new();
        }
        let padded = pkcs7_pad(plaintext.as_bytes());
        let Some(ciphertext) = crate::eme::transform(
            &self.name_key,
            &self.name_tweak,
            &padded,
            crate::eme::Direction::Encrypt,
        ) else {
            // パディング後は必ず 16 の倍数で、2048 バイト以内に収まる想定。
            // ここに来るのはセグメントが長すぎる場合だけ。空を返すと名前が
            // 消えるので、呼び出し側が検出できる形にする。
            return String::new();
        };
        BASE32HEX_NOPAD
            .encode(&ciphertext)
            .to_ascii_lowercase()
    }

    /// 1 セグメントを復号する。
    pub fn decrypt_segment(&self, ciphertext: &str) -> Result<String, TbxError> {
        if ciphertext.is_empty() {
            return Ok(String::new());
        }
        if ciphertext.ends_with('=') {
            return Err(TbxError::Other(
                "ファイル名の base32 表現が不正です（パディング付き）".to_string(),
            ));
        }
        let upper = ciphertext.to_ascii_uppercase();
        let raw = BASE32HEX_NOPAD.decode(upper.as_bytes()).map_err(|_| {
            TbxError::Other(format!(
                "ファイル名を base32 として読めませんでした: {ciphertext}"
            ))
        })?;
        if raw.len() % NAME_BLOCK != 0 || raw.is_empty() || raw.len() > 2048 {
            return Err(TbxError::Other(format!(
                "ファイル名の暗号文が不正な長さです（{} バイト）",
                raw.len()
            )));
        }
        let Some(padded) = crate::eme::transform(
            &self.name_key,
            &self.name_tweak,
            &raw,
            crate::eme::Direction::Decrypt,
        ) else {
            return Err(TbxError::Other(
                "ファイル名を復号できませんでした（EME）".to_string(),
            ));
        };
        let plain = pkcs7_unpad(&padded)?;
        let text = String::from_utf8(plain).map_err(|_| {
            TbxError::Other(format!(
                "復号したファイル名が UTF-8 ではありません: {ciphertext}"
            ))
        })?;
        Ok(text)
    }

    /// パスを暗号化する。`is_dir` は最後のセグメントの扱いだけに影響する。
    pub fn encrypt_path(&self, path: &str, is_dir: bool) -> String {
        if self.mode == NameMode::Off {
            return format!("{path}{}", self.suffix);
        }
        let segments: Vec<&str> = path.split('/').collect();
        let mut out: Vec<String> = Vec::with_capacity(segments.len());
        for (i, segment) in segments.iter().enumerate() {
            let is_last = i == segments.len() - 1;
            // ディレクトリ名を残す設定では、最後以外をそのまま通す。
            if !self.encrypt_dir_names && !is_last && is_dir {
                out.push(segment.to_string());
                continue;
            }
            if !self.encrypt_dir_names && !is_dir && !is_last {
                out.push(segment.to_string());
                continue;
            }
            out.push(self.encrypt_segment(segment));
        }
        out.join("/")
    }

    /// パスを復号する。
    pub fn decrypt_path(&self, path: &str, is_dir: bool) -> Result<String, TbxError> {
        if self.mode == NameMode::Off {
            let trimmed = path.strip_suffix(&self.suffix).unwrap_or(path);
            return Ok(trimmed.to_string());
        }
        let segments: Vec<&str> = path.split('/').collect();
        let mut out: Vec<String> = Vec::with_capacity(segments.len());
        for (i, segment) in segments.iter().enumerate() {
            let is_last = i == segments.len() - 1;
            if !self.encrypt_dir_names
                && ((is_dir && !is_last) || (!is_dir && !is_last))
            {
                out.push(segment.to_string());
                continue;
            }
            out.push(self.decrypt_segment(segment)?);
        }
        Ok(out.join("/"))
    }

    // ---- 内容 ----

    /// 平文サイズから暗号文サイズを求める（rclone と同じ式）。
    pub fn encrypted_size(&self, plain: u64) -> u64 {
        let blocks = plain / CHUNK_SIZE as u64;
        let residue = plain % CHUNK_SIZE as u64;
        let mut size = HEADER_SIZE as u64 + blocks * (TAG_SIZE + CHUNK_SIZE) as u64;
        if residue != 0 {
            size += TAG_SIZE as u64 + residue;
        }
        size
    }

    /// 暗号文サイズから平文サイズを求める。
    pub fn decrypted_size(&self, encrypted: u64) -> Result<u64, TbxError> {
        if encrypted < HEADER_SIZE as u64 {
            return Err(TbxError::Other(
                "暗号化ファイルが短すぎます（ヘッダすら無い）".to_string(),
            ));
        }
        let mut size = encrypted - HEADER_SIZE as u64;
        let block_total = (TAG_SIZE + CHUNK_SIZE) as u64;
        let blocks = size / block_total;
        let mut residue = size % block_total;
        let mut plain = blocks * CHUNK_SIZE as u64;
        if residue != 0 {
            if residue <= TAG_SIZE as u64 {
                return Err(TbxError::Other(
                    "暗号化ブロックのヘッダが切り詰められています".to_string(),
                ));
            }
            residue -= TAG_SIZE as u64;
            plain += residue;
        }
        size = 0;
        let _ = size;
        Ok(plain)
    }

    /// 平文全体を暗号化する。
    pub fn encrypt_data(&self, plain: &[u8]) -> Vec<u8> {
        let mut nonce = [0u8; NONCE_SIZE];
        crate::crypt::fill_random(&mut nonce);
        self.encrypt_data_with_nonce(plain, nonce)
    }

    /// nonce を指定して暗号化する（試験用）。
    pub fn encrypt_data_with_nonce(&self, plain: &[u8], mut nonce: [u8; NONCE_SIZE]) -> Vec<u8> {
        let mut out = Vec::with_capacity(self.encrypted_size(plain.len() as u64) as usize);
        out.extend_from_slice(FILE_MAGIC);
        out.extend_from_slice(&nonce);

        for chunk in plain.chunks(CHUNK_SIZE) {
            out.extend_from_slice(&seal(&self.data_key, &nonce, chunk));
            nonce_increment(&mut nonce);
        }
        out
    }

    /// 暗号文全体を復号する。
    pub fn decrypt_data(&self, sealed: &[u8]) -> Result<Vec<u8>, TbxError> {
        if sealed.len() < HEADER_SIZE {
            return Err(TbxError::Other(
                "暗号化ファイルが短すぎます".to_string(),
            ));
        }
        if &sealed[..8] != FILE_MAGIC {
            return Err(TbxError::Other(
                "このファイルは暗号化されていません（識別子が違う）".to_string(),
            ));
        }
        let mut nonce = [0u8; NONCE_SIZE];
        nonce.copy_from_slice(&sealed[8..HEADER_SIZE]);

        let body = &sealed[HEADER_SIZE..];
        let mut plain = Vec::with_capacity(self.decrypted_size(sealed.len() as u64)? as usize);
        let mut cursor = 0usize;
        while cursor < body.len() {
            let remaining = body.len() - cursor;
            let take = remaining.min(CHUNK_MAX);
            let chunk = &body[cursor..cursor + take];
            let opened = open(&self.data_key, &nonce, chunk).map_err(|e| {
                TbxError::Other(format!(
                    "ブロックを復号できませんでした（パスワードか salt が違う可能性があります）: {e}"
                ))
            })?;
            plain.extend_from_slice(&opened);
            nonce_increment(&mut nonce);
            cursor += take;
        }
        Ok(plain)
    }

    /// 範囲読み出しに必要な「暗号文の範囲」を返す。
    ///
    /// 戻り値は `(暗号文の開始, 暗号文の終了)`。ヘッダ（`0..HEADER_SIZE`）は
    /// 別途読む必要がある。I/O を非同期で行いたい呼び出し元が、先にこの
    /// 範囲を求めて取得し、[`Self::decrypt_range_from`] へ渡す。
    pub fn range_plan(&self, offset: u64, length: u64) -> (u64, u64) {
        if length == 0 {
            return (HEADER_SIZE as u64, HEADER_SIZE as u64);
        }
        let start_block = offset / CHUNK_SIZE as u64;
        let last_block = (offset + length - 1) / CHUNK_SIZE as u64;
        let block_total = (TAG_SIZE + CHUNK_SIZE) as u64;
        let start = HEADER_SIZE as u64 + start_block * block_total;
        let blocks = last_block - start_block + 1;
        (start, start + blocks * block_total)
    }

    /// 取得済みのヘッダと本文から、平文の範囲を組み立てる（I/O なし）。
    ///
    /// `header` は `0..HEADER_SIZE`、`body` は [`Self::range_plan`] が返した
    /// 範囲の暗号文。`offset` は平文側の開始位置。
    pub fn decrypt_range_from(
        &self,
        offset: u64,
        length: u64,
        header: &[u8],
        body: &[u8],
    ) -> Result<Vec<u8>, TbxError> {
        if length == 0 {
            return Ok(Vec::new());
        }
        if header.len() < HEADER_SIZE || &header[..8] != FILE_MAGIC {
            return Err(TbxError::Other(
                "このファイルは暗号化されていません（識別子が違う）".to_string(),
            ));
        }
        let start_block = (offset / CHUNK_SIZE as u64) as usize;
        let start_discard = (offset % CHUNK_SIZE as u64) as usize;

        let mut nonce = [0u8; NONCE_SIZE];
        nonce.copy_from_slice(&header[8..HEADER_SIZE]);
        for _ in 0..start_block {
            nonce_increment(&mut nonce);
        }

        let mut plain = Vec::with_capacity(CHUNK_MAX);
        let mut cursor = 0usize;
        while cursor < body.len() {
            let take = (body.len() - cursor).min(CHUNK_MAX);
            let chunk = &body[cursor..cursor + take];
            let opened = open(&self.data_key, &nonce, chunk)
                .map_err(|e| TbxError::Other(format!("ブロックを復号できませんでした: {e}")))?;
            plain.extend_from_slice(&opened);
            nonce_increment(&mut nonce);
            cursor += take;
        }

        if start_discard >= plain.len() {
            return Ok(Vec::new());
        }
        let end = (start_discard + length as usize).min(plain.len());
        Ok(plain[start_discard..end].to_vec())
    }

}

/// 乱数を埋める（OS の CSPRNG）。
pub(crate) fn fill_random(buf: &mut [u8]) {
    // `getrandom` を直接使わず、`rand` 経由で OS の乱数を取る。
    use rand::RngCore;
    rand::rngs::OsRng.fill_bytes(buf);
}

/// nonce を 1 進める。rclone は先頭バイトから桁上げする（リトルエンディアン的）。
fn nonce_increment(nonce: &mut [u8; NONCE_SIZE]) {
    for byte in nonce.iter_mut() {
        let digit = *byte;
        *byte = digit.wrapping_add(1);
        if *byte >= digit {
            break;
        }
    }
}

/// SecretBox で封印する。
///
/// Go の `secretbox.Seal` も Rust の `aead` も **「暗号文 || タグ」** の順で
/// 返す（実測: rclone が作った暗号文をそのまま復号できた）。並べ替えは不要。
fn seal(key: &[u8; 32], nonce: &[u8; NONCE_SIZE], plain: &[u8]) -> Vec<u8> {
    use xsalsa20poly1305::aead::{Aead, KeyInit as _};
    use xsalsa20poly1305::{Nonce, XSalsa20Poly1305};

    let cipher = XSalsa20Poly1305::new(key.into());
    cipher
        .encrypt(Nonce::from_slice(nonce), plain)
        .expect("SecretBox の暗号化は失敗しない")
}

/// 封印を開く。`sealed` は「暗号文 || タグ」。
fn open(key: &[u8; 32], nonce: &[u8; NONCE_SIZE], sealed: &[u8]) -> Result<Vec<u8>, String> {
    use xsalsa20poly1305::aead::{Aead, KeyInit as _};
    use xsalsa20poly1305::{Nonce, XSalsa20Poly1305};

    if sealed.len() < TAG_SIZE {
        return Err("ブロックが短すぎます".to_string());
    }
    let cipher = XSalsa20Poly1305::new(key.into());
    cipher
        .decrypt(Nonce::from_slice(nonce), sealed)
        .map_err(|_| "認証に失敗しました".to_string())
}

/// PKCS#7 で 16 の倍数へパディングする。空でも 16 バイトになる。
fn pkcs7_pad(input: &[u8]) -> Vec<u8> {
    let pad = NAME_BLOCK - (input.len() % NAME_BLOCK);
    let mut out = Vec::with_capacity(input.len() + pad);
    out.extend_from_slice(input);
    out.extend(std::iter::repeat(pad as u8).take(pad));
    out
}

/// PKCS#7 のパディングを外す。壊れていればエラー。
fn pkcs7_unpad(input: &[u8]) -> Result<Vec<u8>, TbxError> {
    if input.is_empty() || input.len() % NAME_BLOCK != 0 {
        return Err(TbxError::Other(
            "ファイル名のパディングが不正です".to_string(),
        ));
    }
    let pad = *input.last().unwrap() as usize;
    if pad == 0 || pad > NAME_BLOCK || pad > input.len() {
        return Err(TbxError::Other(
            "ファイル名のパディングが不正です".to_string(),
        ));
    }
    let body = &input[..input.len() - pad];
    if !input[input.len() - pad..].iter().all(|b| *b as usize == pad) {
        return Err(TbxError::Other(
            "ファイル名のパディングが不正です".to_string(),
        ));
    }
    Ok(body.to_vec())
}

/// 16 進ダンプ（診断用）。
pub fn hex(bytes: &[u8]) -> String {
    HEXLOWER.encode(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cipher() -> Cipher {
        Cipher::new("test-password", "test-salt", &CryptConfig::default()).unwrap()
    }

    #[test]
    fn default_config_is_disabled() {
        assert!(!CryptConfig::default().enabled, "既定はオフでなければならない");
    }

    #[test]
    fn segment_roundtrip() {
        let c = cipher();
        for name in ["file0.txt", "日本語のファイル名.pdf", "a", "x.bin"] {
            let enc = c.encrypt_segment(name);
            assert!(!enc.is_empty(), "{name} の暗号化が空になった");
            assert!(
                !enc.contains(name),
                "暗号化された名前が平文を含んでいる: {enc}"
            );
            assert_eq!(c.decrypt_segment(&enc).unwrap(), name, "{name} の往復");
        }
    }

    #[test]
    fn segment_is_deterministic() {
        let c = cipher();
        let a = c.encrypt_segment("same.txt");
        let b = c.encrypt_segment("same.txt");
        assert_eq!(a, b, "同じ名前から違う暗号名が出た");
    }

    #[test]
    fn different_password_gives_different_names() {
        let a = Cipher::new("pass-a", "salt", &CryptConfig::default()).unwrap();
        let b = Cipher::new("pass-b", "salt", &CryptConfig::default()).unwrap();
        assert_ne!(a.encrypt_segment("x.txt"), b.encrypt_segment("x.txt"));
    }

    #[test]
    fn path_roundtrip() {
        let c = cipher();
        let enc = c.encrypt_path("dir/sub/file.txt", false);
        assert_eq!(enc.split('/').count(), 3, "セグメント数が変わった");
        assert_eq!(
            c.decrypt_path(&enc, false).unwrap(),
            "dir/sub/file.txt"
        );
    }

    #[test]
    fn dir_names_can_stay_plain() {
        let mut cfg = CryptConfig::default();
        cfg.directory_name_encryption = false;
        let c = Cipher::new("p", "s", &cfg).unwrap();
        let enc = c.encrypt_path("dir/sub/file.txt", false);
        assert!(enc.starts_with("dir/sub/"), "ディレクトリ名が残っていない: {enc}");
        assert_eq!(c.decrypt_path(&enc, false).unwrap(), "dir/sub/file.txt");
    }

    #[test]
    fn data_roundtrip_various_sizes() {
        let c = cipher();
        for size in [0usize, 1, 100, CHUNK_SIZE - 1, CHUNK_SIZE, CHUNK_SIZE + 1, 3 * CHUNK_SIZE] {
            let plain: Vec<u8> = (0..size).map(|i| (i % 251) as u8).collect();
            let sealed = c.encrypt_data(&plain);
            assert_eq!(
                sealed.len() as u64,
                c.encrypted_size(size as u64),
                "{size} バイトの暗号文サイズが式と違う"
            );
            let back = c.decrypt_data(&sealed).unwrap();
            assert_eq!(plain, back, "{size} バイトの往復が壊れている");
        }
    }

    #[test]
    fn encrypted_output_starts_with_magic() {
        let c = cipher();
        let sealed = c.encrypt_data(b"hello");
        assert_eq!(&sealed[..8], FILE_MAGIC);
    }

    #[test]
    fn wrong_password_fails_to_decrypt() {
        let a = Cipher::new("pass-a", "salt", &CryptConfig::default()).unwrap();
        let b = Cipher::new("pass-b", "salt", &CryptConfig::default()).unwrap();
        let sealed = a.encrypt_data(b"secret content");
        assert!(b.decrypt_data(&sealed).is_err());
    }

    #[test]
    fn plain_file_is_rejected() {
        let c = cipher();
        let result = c.decrypt_data(b"not encrypted at all, just plain bytes here");
        assert!(result.is_err(), "平文を暗号文として受け入れてしまった");
    }

    #[test]
    fn decrypt_range_matches_full_decrypt() {
        let c = cipher();
        let size = 3 * CHUNK_SIZE + 12345;
        let plain: Vec<u8> = (0..size).map(|i| ((i * 7) % 253) as u8).collect();
        let sealed = c.encrypt_data(&plain);

        let ranges = [
            (0u64, 100u64),
            (CHUNK_SIZE as u64 - 10, 100),
            (CHUNK_SIZE as u64, 10),
            (2 * CHUNK_SIZE as u64 + 5, CHUNK_SIZE as u64),
            (size as u64 - 1, 1),
        ];
        for (offset, length) in ranges {
            let (start, end) = c.range_plan(offset, length);
            let s = start as usize;
            let e = (end as usize).min(sealed.len());
            let body = if s >= sealed.len() {
                Vec::new()
            } else {
                sealed[s..e].to_vec()
            };
            let got = c
                .decrypt_range_from(offset, length, &sealed[..HEADER_SIZE], &body)
                .unwrap();
            let expect_end = (offset as usize + length as usize).min(plain.len());
            assert_eq!(
                got,
                plain[offset as usize..expect_end].to_vec(),
                "offset={offset} length={length} の範囲が一致しない"
            );
        }
    }

    #[test]
    fn size_formulas_match_rclone_docs() {
        let c = cipher();
        // ドキュメントの例: 1 バイト -> 32 + 17 = 49 バイト。
        assert_eq!(c.encrypted_size(1), 49);
        // 1 MiB -> 1048864 バイト。
        assert_eq!(c.encrypted_size(1024 * 1024), 1048864);
        // 逆算も一致する。
        assert_eq!(c.decrypted_size(49).unwrap(), 1);
        assert_eq!(c.decrypted_size(1048864).unwrap(), 1024 * 1024);
    }

    #[test]
    fn nonce_increment_carries() {
        // 先頭バイトだけ 0xFF。1 バイト目が 0 に戻り、2 バイト目へ繰り上がる。
        let mut n = [0u8; NONCE_SIZE];
        n[0] = 0xFF;
        nonce_increment(&mut n);
        assert_eq!(n[0], 0x00, "先頭が 0 に戻らない");
        assert_eq!(n[1], 0x01, "繰り上がっていない");
        assert_eq!(n[2], 0x00, "3 バイト目まで繰り上がってはいけない");

        // 全バイト 0xFF なら全部 0 へ繰り上がる（rclone の carry と同じ挙動）。
        let mut all = [0xFFu8; NONCE_SIZE];
        nonce_increment(&mut all);
        assert!(all.iter().all(|b| *b == 0), "全 0xFF で全部 0 にならない");

        // 通常は 1 だけ増える。
        let mut plain = [0u8; NONCE_SIZE];
        nonce_increment(&mut plain);
        assert_eq!(plain[0], 1);
        assert_eq!(plain[1], 0);
    }

    #[test]
    fn name_mode_off_appends_suffix() {
        let mut cfg = CryptConfig::default();
        cfg.filename_encryption = "off".to_string();
        let c = Cipher::new("p", "s", &cfg).unwrap();
        assert_eq!(c.encrypt_path("a.txt", false), "a.txt.bin");
        assert_eq!(c.decrypt_path("a.txt.bin", false).unwrap(), "a.txt");
    }
}
