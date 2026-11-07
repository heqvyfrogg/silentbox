//! rclone 互換の検証用。暗号化・復号をコマンドで行い、rclone とファイルを
//! 受け渡して突き合わせる。
//!
//! 使い方:
//!   crypt_check encrypt <パスワード> <salt|-> <平文> <出力>
//!   crypt_check decrypt <パスワード> <salt|-> <暗号文> <出力>
//!   crypt_check name-enc <パスワード> <salt|-> <平文セグメント>
//!   crypt_check name-dec <パスワード> <salt|-> <暗号セグメント>
//!   crypt_check path-enc <パスワード> <salt|-> <平文パス>
//!   crypt_check path-dec <パスワード> <salt|-> <暗号パス>

use terabox_core::crypt::{Cipher, CryptConfig};

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.len() < 4 {
        eprintln!("引数が足りません");
        std::process::exit(2);
    }
    let command = args[0].as_str();
    let password = args[1].as_str();
    let salt = if args[2] == "-" { "" } else { args[2].as_str() };

    let config = CryptConfig::default();
    let cipher = match Cipher::new(password, salt, &config) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("鍵を作れませんでした: {e}");
            std::process::exit(1);
        }
    };

    match command {
        "encrypt" => {
            let src = std::fs::read(&args[3]).expect("平文を読めません");
            let sealed = cipher.encrypt_data(&src);
            std::fs::write(&args[4], &sealed).expect("暗号文を書けません");
            println!(
                "暗号化: {} バイト -> {} バイト",
                src.len(),
                sealed.len()
            );
        }
        "decrypt" => {
            let sealed = std::fs::read(&args[3]).expect("暗号文を読めません");
            match cipher.decrypt_data(&sealed) {
                Ok(plain) => {
                    std::fs::write(&args[4], &plain).expect("平文を書けません");
                    println!("復号: {} バイト -> {} バイト", sealed.len(), plain.len());
                }
                Err(e) => {
                    eprintln!("復号できませんでした: {e}");
                    std::process::exit(1);
                }
            }
        }
        "name-enc" => println!("{}", cipher.encrypt_segment(&args[3])),
        "name-dec" => match cipher.decrypt_segment(&args[3]) {
            Ok(v) => println!("{v}"),
            Err(e) => {
                eprintln!("復号できませんでした: {e}");
                std::process::exit(1);
            }
        },
        "path-enc" => println!("{}", cipher.encrypt_path(&args[3], false)),
        "path-dec" => match cipher.decrypt_path(&args[3], false) {
            Ok(v) => println!("{v}"),
            Err(e) => {
                eprintln!("復号できませんでした: {e}");
                std::process::exit(1);
            }
        },
        other => {
            eprintln!("不明なコマンド: {other}");
            std::process::exit(2);
        }
    }
}
