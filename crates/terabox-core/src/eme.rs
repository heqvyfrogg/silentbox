//! EME（ECB-Mix-ECB）。Halevi–Rogaway 2003 "A Parallelizable Enciphering Mode"。
//!
//! rclone crypt のファイル名暗号化がこれを使う。AES-256 の単一ブロック暗号を
//! 束ねて 16 バイト単位の任意長（最大 2048 バイト = 128 ブロック）の広ブロック
//! 暗号を作る。同じ平文と同じ tweak からは常に同じ暗号文が出る（決定論的）。
//! ファイル名を同じ名前で引けるようにするために必須の性質。
//!
//! 実装は <https://github.com/rfjakob/eme> の `eme.go` を仕様として移植した。
//! 同リポジトリのテストベクタ（全ゼロ鍵・全ゼロ tweak・全ゼロ入力 16 バイトと
//! 2048 バイト）で照合する。

use aes::cipher::{BlockDecrypt, BlockEncrypt, KeyInit};
use aes::Aes256;

/// 1 ブロックのバイト数。
pub const BLOCK: usize = 16;

/// 扱える最大ブロック数（rclone は 2048 バイト = 128 ブロックまで）。
pub const MAX_BLOCKS: usize = 16 * 8;

/// 方向。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    Encrypt,
    Decrypt,
}

/// GF(2^128) 上の 2 倍算（EME-32 ドラフトの `multByTwo`）。
fn mult_by_two(input: &[u8; BLOCK]) -> [u8; BLOCK] {
    let mut out = [0u8; BLOCK];
    out[0] = input[0].wrapping_mul(2);
    // 定数時間版: `in[15] >= 128` なら 135 を xor する。
    let high_bit = input[BLOCK - 1] >> 7;
    out[0] ^= 135 & high_bit.wrapping_neg();
    for j in 1..BLOCK {
        out[j] = input[j].wrapping_mul(2);
        out[j] = out[j].wrapping_add(input[j - 1] >> 7);
    }
    out
}

/// `LTable[i]`（i=0..m）を作る。L0 = 2 * AES(K; 0)。
fn tabulate_l(cipher: &Aes256, m: usize) -> Vec<[u8; BLOCK]> {
    let zero = [0u8; BLOCK];
    let mut li = zero;
    cipher.encrypt_block((&mut li).into());

    let mut table = Vec::with_capacity(m);
    for _ in 0..m {
        li = mult_by_two(&li);
        table.push(li);
    }
    table
}

fn aes_block(cipher: &Aes256, input: &[u8; BLOCK], direction: Direction) -> [u8; BLOCK] {
    let mut block = *input;
    match direction {
        Direction::Encrypt => cipher.encrypt_block((&mut block).into()),
        Direction::Decrypt => cipher.decrypt_block((&mut block).into()),
    }
    block
}

fn xor(a: &[u8; BLOCK], b: &[u8; BLOCK]) -> [u8; BLOCK] {
    let mut out = [0u8; BLOCK];
    for i in 0..BLOCK {
        out[i] = a[i] ^ b[i];
    }
    out
}

/// EME の変換。`data` は 16 バイトの倍数で 16..=2048 バイト。
///
/// 長さが不正なら `None`（呼び出し元が明示的に失敗させる。パニックしない）。
pub fn transform(
    key: &[u8; 32],
    tweak: &[u8; BLOCK],
    data: &[u8],
    direction: Direction,
) -> Option<Vec<u8>> {
    if data.len() % BLOCK != 0 {
        return None;
    }
    let m = data.len() / BLOCK;
    if m == 0 || m > MAX_BLOCKS {
        return None;
    }

    let cipher = Aes256::new(key.into());
    let table = tabulate_l(&cipher, m);

    // PPPj = AES(K; Pj xor L[j]) を C へ入れる。
    let mut c = vec![0u8; data.len()];
    for j in 0..m {
        let pj: [u8; BLOCK] = data[j * BLOCK..(j + 1) * BLOCK].try_into().ok()?;
        let ppj = xor(&pj, &table[j]);
        let pppj = aes_block(&cipher, &ppj, direction);
        c[j * BLOCK..(j + 1) * BLOCK].copy_from_slice(&pppj);
    }

    // MP = (xorSum PPPj) xor T
    let first: [u8; BLOCK] = c[0..BLOCK].try_into().ok()?;
    let mut mp = xor(&first, tweak);
    for j in 1..m {
        let block: [u8; BLOCK] = c[j * BLOCK..(j + 1) * BLOCK].try_into().ok()?;
        mp = xor(&mp, &block);
    }

    // MC = AES(K; MP), M = MP xor MC
    let mc = aes_block(&cipher, &mp, direction);
    let mut m_val = xor(&mp, &mc);

    // j >= 1: CCCj = (2^(j-1) * M) xor PPPj
    for j in 1..m {
        m_val = mult_by_two(&m_val);
        let ppp: [u8; BLOCK] = c[j * BLOCK..(j + 1) * BLOCK].try_into().ok()?;
        let ccc = xor(&ppp, &m_val);
        c[j * BLOCK..(j + 1) * BLOCK].copy_from_slice(&ccc);
    }

    // CCC1 = (xorSum CCCj) xor T xor MC
    let mut ccc1 = xor(&mc, tweak);
    for j in 1..m {
        let block: [u8; BLOCK] = c[j * BLOCK..(j + 1) * BLOCK].try_into().ok()?;
        ccc1 = xor(&ccc1, &block);
    }
    c[0..BLOCK].copy_from_slice(&ccc1);

    // 最後にもう一度 AES を通し、L を xor で戻す。
    for j in 0..m {
        let block: [u8; BLOCK] = c[j * BLOCK..(j + 1) * BLOCK].try_into().ok()?;
        let cc = aes_block(&cipher, &block, direction);
        let out = xor(&cc, &table[j]);
        c[j * BLOCK..(j + 1) * BLOCK].copy_from_slice(&out);
    }

    Some(c)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hex(s: &str) -> Vec<u8> {
        s.trim()
            .chars()
            .filter(|c| !c.is_whitespace())
            .collect::<String>()
            .as_bytes()
            .chunks(2)
            .map(|p| u8::from_str_radix(std::str::from_utf8(p).unwrap(), 16).unwrap())
            .collect()
    }

    /// rfjakob/eme の `TestEnc16` と同じベクタ。
    #[test]
    fn known_vector_16_bytes() {
        let key = [0u8; 32];
        let tweak = [0u8; BLOCK];
        let input = [0u8; BLOCK];
        let out = transform(&key, &tweak, &input, Direction::Encrypt).unwrap();
        assert_eq!(
            hex("f1b9ce8ca15a4ba9fb476905434b9fd3"),
            out,
            "16 バイトの既知ベクタと一致しない"
        );
    }

    /// rfjakob/eme の `TestEnc2048` と同じベクタ（2048 バイト = 128 ブロック）。
    #[test]
    fn known_vector_2048_bytes() {
        let key = [0u8; 32];
        let tweak = [0u8; BLOCK];
        let input = vec![0u8; 2048];
        let out = transform(&key, &tweak, &input, Direction::Encrypt).unwrap();
        let expected_hex = "\
            630500884158a8d41216aaa6351e92ea7ca540a949e73f1d6069afef372eae22\
            6d426d8f4dbbce505051fcd596e7b32cdd1e44a40bcce0887ebbc160f779dafb\
            b68bc25a40040987cdb5aba19b956e11dd69d7ce74d1d8788903254217300f3a\
            fd9a92cea395a89fd842256d3919ac303c1d1a21ec44cb2898acabb3d0e04e05\
            845e9183a64b4148f9614f62f79fa971799c896c58cd2b285cb00e40720b5c11\
            8ef1c0dbf5e51f09cd10db672b9c22c53d4bf5dabf16cfcbc382e3209e7ba279\
            55b2afa055924d2155319205f964492e0e88172f4ffadca35cd3e694fd3d803f\
            610edc1696113996749bdb2a2dfb8fbd92f0ef723146f93d969d1de4cc1c19ef\
            a33785f135a1203005ba2208fcd6b9f0d0e942993671b0c5baefe7ed0bbfe070\
            a9e1d26eed37228ceda64b70c25a59fd040703acc385c6e4093a8e258984371c\
            2d811c01723b87c820a55470501b245d97a0c7564c44e1e31a89457c1125198d\
            27ac2dec67f7237c781f9f622e420da471af577fa6ad8014bb1dad52dc450e37\
            ebeeb1f1b7ba3c3d5796c6646bd052fe66cf61d22f6e58efcbf0328d3b6e1088\
            603e54bfd2da241c970fec58c7d21fc2b2f1b4b79552a92500a7e6848a22ec47\
            ee362cef9787af3b26439470992c8d103997395aefeff10802673d4630009e4e\
            5ed26038d5f05c589e64046274d5ff133398e4bab08ca71c0ce93dbd0657785b\
            150c9e418339e8e47d02ae7322b5b55364dfe10dd02d54dc5f2dbe82e55bcf05\
            03b30d709301fc7f63cd15e7e1245189405b1364b6074c0e493f7beb27385986\
            4e95643c7d914279d95f220247618b7a5c896d604e9ce88e0a3ab434cfe97cab\
            c520b20e2fffee958972e82e98b952cbf3567be560a8385e520b636829252cae\
            16ac882d1d3dde447a90af52be7967f357a36382c9672a659ed0ffae9ee7b969\
            c8c788f982fbd524efb6b852f65be5f371032691e9ef6e5a217607cf990e5351\
            b251f841834045d9bd54633e8bb0d19569377f805857a503a2642dfc06e2063d\
            fd465dcae83b1e362394cdf20bd6a6d5dd0bacad7f2d407e597b004e90683bfc\
            23b92796bb0cdf67752765dbba2d1332848adcb83e60b7f3f8c397a3aa5de08b\
            f491676a18ee4a6ed9b342b33c081794365a72668f273a655e150fa36686aa5d\
            fbc87ccda9cbe7982e531044cd9fcea68ca1a5034ee64e0558b7c6af56808c03\
            43506dccf39e4ca8634518e6420c107b7d5fe89943e9f40044b2ae7f047d9727\
            5ec4763b41416fb3faff897e6ee0047f97b215b02ee09a3f533cba2ee131a0aa\
            68772860759325ea4dc7ab86528d0aff55f96d00a908ea32f9fa8bd0640d632b\
            f0d812f43beb03e6c6d4a654b41f27143fde4aa7ed16ae4bba939fbed8ae28da\
            ce83fcb49fd3ca910512902fc696419968e260901796f3af75d059c6e93a8f53\
            b445f921e60d9429007c78107309a86102b923dbb2f1fa04f84539c7c58ed45b\
            40d4b267541d8f2f932ae75dfc13ef906089e1c847635aaada201e32301f326a\
            733f4a99caa56fa00ccd6de2752a4495408bb4c930eaf84c97a28767a036f656\
            27efa868adeee1cf47840e67ac4489381da1513dd3d1707a5ad91ac4ba1c3571\
            4b1cd77e05551612cdfb88187a5a39afd5b2a54f88aeb9605c1e01cd7a976a16\
            8c6e40eddcb20f09fe54a07a7c971eda08789b41e450f46a30c2b3494f30a060\
            73631d151ae048793fe32ff08a08b8693d1da71451f7ff2438d7a521f8c97e4b\
            c8ee744c1574d296c799d3f94e81522ce81def780660cee061d3110c1f18133d\
            ca1daabb2268aad76327f3cb6ae8c96fc66b5506f5644f2652d022d7b6162099\
            70f78b8e5c9a74f5d1fc31037d01733bff097a661d4331adc087b84e32afb9d8\
            2a0614ea459022918f8ca60e74b7c6865d3e86e6ae4acf9546985dc5ccf242dd\
            9ab0486bde99a1e656499cbfceef8de5beafda89965bf208c70f4a0f8f677792\
            962b09e0a355d53bbacb4a7acc16e50aa483994d85ca7e28a34ee52843d4f27a\
            cfb3b5347b89099fd01642b605f87f82283bb146aa319048462fe11a71d0fa06\
            4e825f69a502ae539240b13a2568383f80c30ebb9a29d2de4c45e6405bd8d158\
            6fe5eef0477d3c21434b3471c0f864dd4c2f135278645ef1b1ebb4d6954f9276\
            3d787ad3d0bb48409782cd803a877027ae105511cdaefe0110f9f6c65f2a34ad\
            12532f4715c0341346dc06bcc4bb0e5c04ffdfd3f625e7e3811a760a2cfa9cbe\
            1c2914882c192b793aeeb3ab8b03fd8be7e248fc9e5aa0c651d4bb5318c8a266\
            b735ddc167d837499a3b38dcdfe188e25172eb974eb0eff20cc5c6e42e505fea\
            7a3b6edfdf93b7062737ea00f72a415c4dfd3cd4fab2dad812d88d02690f606b\
            52dcfb0ab220dc65a7dee6f31e951324a1c442adfe9cf672646876bedc2c2cb7\
            1bac583f3cf4ea6769eb54d22bf22b0d908a297caa13767ed14c44b738799e82\
            1885b2b2988a9b88a2f35bf8d307543af9007fadcfcc273f8029c2fe3d59449c\
            5f193ca7f8986ca7451d9a3ee14335b10868421b64ea1605d48d4fc22e378f82\
            b7fe3e88aada30b4c545408b87b24511e371f25d1a94e7b78a6a7aec848e6f82\
            614f96ef5ed8705e0adec0f889c1860cf8211d716fddf319862290019a4387fb\
            77c89a5ae7d0074fa72024f2e5096d39ebac8e8ed02191879585394987fea3a0\
            7a720d6880910629cdcd83a02ffd98830d2f57ee6c53b5cfedb42ec7c3d925cf\
            3080c343f21711a90f117e9eaf91402d09b83e83bd18d1c4e3d165a8b9bac7ad\
            ca12cb0147bfc4c6e2166c57b8182e63fbc698881ed5b329eb491eb98050a922\
            c15804021013bf08942db9ee6d8f2a2c4eb93771340ed9e323d09e4256b7e5ac";
        let expected = hex(expected_hex);
        assert_eq!(expected.len(), 2048, "ベクタの長さが 2048 でない");
        assert_eq!(out, expected, "2048 バイトの既知ベクタと一致しない");
    }

    #[test]
    fn roundtrip_various_lengths() {
        let key = [7u8; 32];
        let tweak = [9u8; BLOCK];
        for blocks in [1usize, 2, 7, 16, 64, 128] {
            let plain = vec![0xA5u8; blocks * BLOCK];
            let enc = transform(&key, &tweak, &plain, Direction::Encrypt).unwrap();
            let dec = transform(&key, &tweak, &enc, Direction::Decrypt).unwrap();
            assert_eq!(plain, dec, "{blocks} ブロックの往復が壊れている");
        }
    }

    #[test]
    fn deterministic_for_same_input() {
        let key = [3u8; 32];
        let tweak = [5u8; BLOCK];
        let plain = vec![0x11u8; 32];
        let a = transform(&key, &tweak, &plain, Direction::Encrypt).unwrap();
        let b = transform(&key, &tweak, &plain, Direction::Encrypt).unwrap();
        assert_eq!(a, b, "同じ入力から違う暗号文が出た");
    }

    #[test]
    fn different_tweak_gives_different_output() {
        let key = [3u8; 32];
        let plain = vec![0x11u8; 32];
        let a = transform(&key, &[0u8; BLOCK], &plain, Direction::Encrypt).unwrap();
        let b = transform(&key, &[1u8; BLOCK], &plain, Direction::Encrypt).unwrap();
        assert_ne!(a, b, "tweak が違うのに同じ暗号文になった");
    }

    #[test]
    fn rejects_bad_lengths() {
        let key = [0u8; 32];
        let tweak = [0u8; BLOCK];
        assert!(transform(&key, &tweak, &[], Direction::Encrypt).is_none());
        assert!(transform(&key, &tweak, &[0u8; 15], Direction::Encrypt).is_none());
        assert!(transform(&key, &tweak, &[0u8; 2049], Direction::Encrypt).is_none());
    }

    #[test]
    fn mult_by_two_matches_reference() {
        // Go 実装の `multByTwo` と同じ計算を手で確認する。
        let mut input = [0u8; BLOCK];
        input[0] = 1;
        let out = mult_by_two(&input);
        assert_eq!(out[0], 2);
        assert_eq!(out[1], 0);

        // 最上位ビットが立っていると 135 が入る。
        let mut input = [0u8; BLOCK];
        input[0] = 0x80;
        input[BLOCK - 1] = 0x80;
        let out = mult_by_two(&input);
        assert_eq!(out[0], 0x00 ^ 135, "還元多項式が効いていない");
        assert_eq!(out[1], 1);
    }
}
