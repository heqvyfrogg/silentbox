# tbx — TeraBox の非公式ネイティブクライアント

広告なし・並列転送・ドライブマウントを備えた TeraBox クライアント。Rust 製で、
CLI と TUI と Windows のドライブマウントを同じコアから提供する。

公式アプリ（`com.dubox.drive` 4.28.2）を解析してプロトコルを移植した別実装で
あり、APK の改変・再配布は行わない。解析の根拠は `docs/protocol.md` にある。

## できること

- **広告ゼロ**。公式アプリは 8 系統の広告 SDK（Pangle, Moloco, Mintegral,
  Smaato, Vungle, AppLovin, InMobi, Facebook）を同梱しているが、本クライアントは
  それらを一切含まない。
- **並列ダウンロード**。`Range` 指定で分割し、中断しても続きから再開する。
- **分割アップロード**。パート単位で送り、送信済みパートは飛ばす。
- **共有リンク**。`/s/1xxxx` を解決して一覧表示し、自分の領域へ保存する。
- **外部プレーヤ再生**。dlink を mpv / VLC / ffplay へ渡す。
- **容量・VIP・制限の可視化**。サーバが返した値だけを表示し、推測で埋めない。
- **ドライブマウント**。WinFsp（要ランタイム）または WebDAV（インストーラ不要）。
- **TUI**。一覧・転送・共有・容量を 1 画面で扱う。

## 必要なもの

| 用途 | 必須 | 備考 |
|---|---|---|
| ビルド | Rust 1.82 以降 | 実測は 1.96.0 |
| ログイン | Chrome または Edge | 専用プロファイルで起動する |
| WinFsp マウント | WinFsp 2.x | 無い場合は WebDAV モードで動く |
| WinFsp のビルド | MSVC ターゲット | `windows-gnu` では `winfsp-sys` が失敗する |

## ビルド

```powershell
cargo build --release
```

WinFsp マウントを含める場合:

```powershell
cargo +stable-x86_64-pc-windows-msvc build --release -p terabox-cli `
  --features winfsp-backend --target x86_64-pc-windows-msvc
```

`winfsp-x64.dll` を exe と同じ場所へ置くか、WinFsp の `bin` を PATH に足す。
無いと起動時に 1285 で終了する。

## 使い方

```powershell
tbx login                       # ブラウザが開く。ログインすると自動で閉じる
tbx doctor                      # 環境と接続の診断
tbx ls /                        # 一覧
tbx quota                       # 容量・VIP・上限
tbx get "/a/b.bin" -o D:\dl     # 分割ダウンロード（中断しても再実行で再開）
tbx put .\big.iso --remote /bk  # 分割アップロード
tbx share "https://www.terabox.com/s/1xxxx" --save
tbx play "/Movies/a.mp4"
tbx mount T:                    # WinFsp があれば WinFsp、無ければ WebDAV
tbx tui
```

すべてのコマンドは `--json` で機械可読な出力になる。

## 構成

```
crates/
  terabox-core/   API クライアント・セッション・転送エンジン・台帳・VFS 抽象
  terabox-cli/    bin `tbx`（サブコマンド、ログイン、診断）
  terabox-tui/    ratatui の画面
  terabox-mount/  WinFsp / WebDAV バックエンド
docs/
  protocol.md     解析結果と根拠（Evidence ID 付き）
  limitations.md  未確定事項と制約
analysis/         REA の出力（JSON）と DEX 文字列
tools/            REA 用の JDK・JADX と環境設定スクリプト
.agents/skills/   このプロジェクト用のスキル
```

`terabox-core` は TUI にも Windows API にも依存しない。CLI・TUI・マウントは
すべてこのコアを共有する。

## 設計上の判断

**セッション Cookie 方式の Web API を使う。**
公式アプリの API にはネイティブ署名（`libnetdisk-security-rand.so` の
`get_signature_md5`）が必要な経路がある。署名生成を再実装すると難読化との
イタチごっこになり保守できないため、署名不要の Web API だけを使う。
`/api/streaming` は対象外。

**トークンは HTML から取る。**
`/main` の HTML に `jsToken`（192 桁 hex）と `templateData`（`bdstoken` / `uk`）が
埋まっている。JS を実行せず正規表現で取り出す。構造が変われば明示的に失敗する。

**秘密情報は資格情報マネージャだけに置く。**
設定ファイルにも台帳 DB にも書かない。`Session` の `Debug` は値を伏せる。

**サーバが返さない値を作らない。**
単一ファイル上限・同時接続数・解像度は、取得できたときだけ表示する。
未取得は「未取得」と出す。

## マウントの使い方

```powershell
tbx mount T: --mode winfsp     # WinFsp がある環境（推奨）
tbx mount T: --mode webdav     # インストーラ不要のフォールバック
tbx mount T:                   # auto: WinFsp があれば WinFsp、無ければ WebDAV
```

`--mode winfsp` は MSVC ターゲットでビルドした `tbx` が必要。
`winfsp-x64.dll` を exe の隣に置くか、WinFsp の `bin` を PATH に足す。

読み取り（一覧・ダウンロード）、フォルダ作成・削除、ファイル書き込み・上書きは
実アカウントで動作確認済み。ただし一定量の書き込み後、サーバが書き込み系 API に
`need verify`（`errno 4000023` / `450016`）を返す状態になる。解除条件は未特定で、
クライアントからは解除できない。この状態ではマウントの書き込みが
**失敗として返る**（黙って成功を装わない）。詳細は `docs/limitations.md`。

## テスト

```powershell
cargo test --workspace
cargo test -p terabox-core --test live_anonymous -- --ignored --nocapture
```

前者はネットワーク不要。後者は認証不要の実通信（jsToken 抽出、未ログインの
`errno -6`、不正共有リンクの `errno 105`）を確認する。

## 注意

- TeraBox は End-to-End 暗号化ではない。運営側が内容へアクセスできる設計で、
  機密文書の保管先には向かない。この性質はクライアントでは変えられない。
- API は非公開で予告なく変わる。`docs/limitations.md` に未確定事項をまとめてある。
- WebDAV は `127.0.0.1` のみで待ち受ける。外部へ公開しない。
