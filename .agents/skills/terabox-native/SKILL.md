---
name: terabox-native
description: TeraBox を広告なしで操作する非公式ネイティブクライアント tbx（Rust 製 CLI/TUI/Windows マウント）の操作・解析・検証手順。APK 解析の証拠を根拠に API を叩き、転送・共有・マウントを実測で確認するときに使う。
---

# terabox-native

## このスキルが扱う対象

`com.dubox.drive` 4.28.2（APKM SHA-256 `D81978F30775B4542819A2DADF09F764BE788331F9C6737B36D1611AFE72EE5B`）を
解析して作った非公式クライアント `tbx` の操作・検証手順。

APK の改変や再配布はしない。プロトコル（HTTP API とトークン取得手順）だけを
別実装へ移植する。

## まず読むファイル

| 目的 | ファイル |
|---|---|
| 解析結果と根拠 | `docs/protocol.md` |
| 使えない機能と未確定事項 | `docs/limitations.md` |
| 全体の構成 | `README.md` |
| 解析の生データ | `analysis/*.json`（REA のエビデンス封筒） |

同じ内容を `references/` へ複製しない。正本は上記の `docs/` に一本化する。

## 前提の確認

```powershell
. .\tools\rea-env.ps1   # JAVA_HOME と REA_JADX_MCP_JAR をプロセス内だけ設定
rea doctor               # jadx が healthy か
cargo --version          # 1.96 系
```

`tools\jdk21\` と `tools\jadx-headless-mcp-0.7.1-all.jar` はリポジトリに含めない
（サイズが大きい）。無い場合は再取得する:

- JDK: Adoptium Temurin 21 Windows x64 zip を `tools\jdk21` へ展開
- JADX: `1013503897/jadx-headless-mcp` v0.7.1 の `-all.jar`
  （SHA-256 `6e5eacf500b64292bfb73c49797c1958f6ee44646e43e868039ae7feb573ff75`）

## ビルド

```powershell
# 既定（WebDAV マウントのみ）
cargo build --release

# WinFsp マウント込み（MSVC ターゲット必須・実測）
cargo +stable-x86_64-pc-windows-msvc build --release -p terabox-cli `
  --features winfsp-backend --target x86_64-pc-windows-msvc
```

WinFsp 版は `winfsp-x64.dll` を exe と同じ場所に置くか、WinFsp の `bin` を
PATH に追加する。無いと起動時に 1285 で落ちる。

## 操作

```powershell
tbx login                # 専用プロファイルの Chrome/Edge を開いて Cookie を取る
tbx login --paste        # ブラウザが使えない環境向け
tbx doctor               # 設定・セッション・API・プレーヤ・ブラウザ・WinFsp
tbx ls / --long
tbx quota
tbx get "/path/file.bin" -o D:\Downloads --conn 4
tbx put .\big.iso --remote /backup
tbx share "https://www.terabox.com/s/1xxxx" --save --dest /
tbx play "/Movies/a.mp4"          # mpv → VLC → ffplay の順に探す
tbx trash list / restore <fs_id> / delete <fs_id> / clear --yes
tbx mount T: --mode auto           # WinFsp があれば WinFsp、無ければ WebDAV
tbx tui
```

## 検証

```powershell
cargo test --workspace                                   # ローカル完結
cargo test -p terabox-core --test live_anonymous -- --ignored --nocapture
```

`live_anonymous` は認証不要の実ネットワーク検証。確認する内容:

- `/main` から jsToken が 192 桁の hex として取れること
- 未ログインの `/api/list` が `errno -6` を返すこと
- 不正な共有リンクが `errno 105` を返すこと

認証が必要な経路は、実際にログインするまで **未検証** と扱う。

## 注意

- `ndus` / `jsToken` / `bdstoken` は Windows 資格情報マネージャだけに保存する。
  設定ファイル・ログ・DB へ書かない。`Session` の `Debug` は値を伏せる。
- サーバが返さない値（単一ファイル上限、同時接続数、解像度）を推測で埋めない。
  未取得は「未取得」と表示する。
- API は非公開で予告なく変わる。`docs/limitations.md` の未確定事項を更新しながら使う。
