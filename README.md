# tbx — TeraBox の非公式ネイティブクライアント

広告なし・並列転送・ドライブマウントを備えた TeraBox クライアント。Rust 製で、
CLI と TUI と OS ネイティブのマウントを同じコアから提供する。

公式アプリ（`com.dubox.drive` 4.28.2）を解析してプロトコルを移植した別実装で
あり、APK の改変・再配布は行わない。解析の根拠は `docs/protocol.md` にある。

## できること

- **広告ゼロ**。公式アプリは 8 系統の広告 SDK（Pangle, Moloco, Mintegral,
  Smaato, Vungle, AppLovin, InMobi, Facebook）を同梱しているが、本クライアントは
  それらを一切含まない。
- **並列ダウンロード**。`Range` 指定で分割し、中断しても続きから再開する。
  実測では 4 接続が最良（無料枠は 8 接続で逆に遅くなる）。
- **分割アップロード**。パート単位で送り、送信済みパートは飛ばす。
- **フォルダ一括ダウンロード**。`tbx get /dir` で階層を再現してまとめて取る。
- **複数アカウント**。名前付きで保持し、`--account` で切り替える。
- **共有リンク**。`/s/1xxxx` を解決して一覧表示し、自分の領域へ保存する。
- **外部プレーヤ再生**。dlink を mpv / VLC / ffplay へ渡す。
- **容量・制限の可視化**。サーバが返した値だけを表示し、推測で埋めない。
  無料枠の内訳（恒久 + 期間限定）と期限も出す。
- **ドライブマウント**。Windows は WinFsp、Linux / macOS は FUSE、
  どこでも WebDAV。
- **TUI**。一覧・転送・共有・容量を 1 画面で扱う。

## 必要なもの

| 用途 | 必須 | 備考 |
|---|---|---|
| ビルド | Rust 1.90 以降 | 実測は 1.96.0（Windows）。1.82 では依存が解けない |
| ログイン | Chrome 系ブラウザ | CDP を話せる必要がある。Firefox は不可 |
| Windows のマウント | WinFsp 2.x | 無い場合は WebDAV モードで動く |
| Windows のビルド | MSVC ターゲット | `windows-gnu` では `winfsp-sys` が失敗する |
| Linux のマウント | `fuse3`（`fusermount3`）と `/dev/fuse` | カーネル側の FUSE も必要 |
| Linux の資格情報 | Secret Service（D-Bus） | 無ければ `--store file` |

## ビルド

```bash
cargo build --release

# Windows で WinFsp 込み
cargo +stable-x86_64-pc-windows-msvc build --release -p terabox-cli \
  --features winfsp-backend --target x86_64-pc-windows-msvc

# Linux / macOS で FUSE 込み
cargo build --release -p terabox-cli --features fuse-backend
```

`winfsp-x64.dll` は exe の隣、または WinFsp の `InstallDir\bin` から読み込む。
無いと起動時に 1285 で終了する。

musl で静的ビルドする場合は資格情報ストアを積まない（D-Bus の静的リンクが
現実的でない）。`tbx login --store file` を使う。

```bash
cargo build --release -p terabox-cli --target x86_64-unknown-linux-musl
```

## 使い方

```bash
tbx login                       # ブラウザが開く。ログインすると自動で閉じる
tbx login --name work           # 2 つ目以降のアカウントを追加
tbx accounts                    # 登録済みの一覧（* が既定、> が使用中）
tbx use work                    # 既定を切り替え
tbx --account work ls /         # この 1 回だけ別アカウントで
tbx doctor                      # 環境と接続の診断
tbx ls /                        # 一覧
tbx quota                       # 容量・無料枠の内訳・期限
tbx get "/a/b.bin" -o ./dl      # 分割ダウンロード（中断しても再実行で再開）
tbx get "/photos" -o ./photos   # フォルダ一括（--jobs で同時数）
tbx put ./big.iso --remote /bk  # 分割アップロード
tbx share "https://www.terabox.com/s/1xxxx" --save
tbx play "/Movies/a.mp4"
tbx mount T:                    # Windows: WinFsp（無ければ WebDAV）
tbx mount ~/tbx                 # Linux: FUSE（無ければ WebDAV）
tbx tui
```

すべてのコマンドは `--json` で機械可読な出力になる。

### 複数アカウント

アカウント名は英数字・`_`・`-` の 32 文字まで。解決順は
`--account` > 環境変数 `TBX_ACCOUNT` > 索引の既定 > `default` の順。

- 秘密は OS の資格情報ストア（`account:<名前>`）に置く。
- `--store file` を明示したときだけ `secrets/<名前>.json` に 0600 で書く。
  権限が緩いファイルは読む前に拒否する。
- 台帳は `state-<名前>.db` に分かれる。アカウントを跨いでレジューム状態が
  混ざらない。
- 旧版の `session` 1 件は、初回に `default` として自動で引き継ぐ。

## 構成

```
crates/
  terabox-core/   API クライアント・セッション・転送エンジン・台帳・VFS 抽象
  terabox-cli/    bin `tbx`（サブコマンド、ログイン、アカウント、診断、走査）
  terabox-tui/    ratatui の画面
  terabox-mount/  WinFsp / FUSE / WebDAV バックエンド
docs/
  protocol.md     解析結果と根拠（Evidence ID 付き）
  limitations.md  未確定事項と制約
analysis/         REA の出力（JSON）と DEX 文字列
tools/            REA 用の JDK・JADX と環境設定スクリプト
.github/workflows CI とタグ連動リリース
.agents/skills/   このプロジェクト用のスキル
```

`terabox-core` は TUI にも OS 固有 API にも依存しない。CLI・TUI・マウントは
すべてこのコアを共有する。

## 設計上の判断

**セッション Cookie 方式の Web API を使う。**
公式アプリの API にはネイティブ署名（`libnetdisk-security-rand.so` の
`get_signature_md5`）が必要な経路がある。署名生成を再実装すると難読化との
イタチごっこになり保守できないため、署名不要の Web API だけを使う。
`/api/streaming` と `/share/streaming` は対象外。

**トークンは HTML から取る。**
`/main` の HTML に `jsToken` と `templateData`（`bdstoken` / `uk`）が
埋まっている。JS を実行せず正規表現で取り出す。構造が変われば明示的に失敗する。

**秘密情報は OS の資格情報ストアだけに置く。**
設定ファイルにも台帳 DB にも書かない。`Session` の `Debug` は値を伏せる。
平文保存は明示指定時のみで、既定にはしない。

**サーバが返さない値を作らない。**
単一ファイル上限・同時接続数・VIP 状態は、取得できたときだけ表示する。
未取得は「未取得」と出す。マウントの容量も取れなければ 0（不明）を返す。

## 暗号化して保存する（既定はオフ）

保存先には暗号文だけを置き、手元のマウントでは普通のファイルとして扱える。
**rclone crypt 互換**なので、同じパスワードと salt があれば rclone でも復号できる。

```bash
tbx crypt setup                      # パスワードを設定して有効化（対話入力）
tbx crypt status                     # 状態（秘密は出さない）
tbx mount T: --crypt                 # 暗号化してマウント（WebDAV / FUSE）
tbx crypt-migrate / --apply          # 既存の平文を暗号化形式へ移す（下見が既定）
tbx crypt disable                    # 無効化（保存済みの暗号文は残る）
```

- 方式: 内容は NaCl SecretBox（XSalsa20-Poly1305）の 64 KiB チャンク、
  名前は AES-256 の EME + base32hex、鍵は scrypt(N=16384, r=8, p=1)。
- 平文のまま置かれたファイルは一覧に出さない（混在を「暗号化済み」と誤認させない）。
- **パスワードを失うと復号できない。** 資格情報ストア（Windows / macOS / Linux gnu）に
  保存し、設定ファイルには書かない。musl ビルドでは `secrets/crypt-<key>.json`（0600）へ
  保存する。`tbx crypt disable --purge` で削除できる。
- マウント中だけ平文で見える。公式アプリや Web では暗号文だけが見える。
- T: が空に見えるときは、次の 2 つを順に確かめる。
  1. 書き込みがサーバに拒否されていないか。`need verify`（errno 4000023、
     削除は 450016）が返る間は、コピーしても何も保存されない。
     `tbx login` で再ログインすると解除できる（実測）。
  2. 既存ファイルが平文のまま残っていないか。平文は一覧に出さないので、
     `tbx crypt-migrate / --apply --delete-source` で暗号化形式へ移す。
- 暗号化と WinFsp の組み合わせに対応した（Windows でも `--crypt` で `T:` に
  マウントできる）。暗号化層を挟んだままドライブとして見せ、サーバには
  暗号文だけが残る。
- 既存の平文ファイルは暗号化マウントの一覧に出ない。平文のマウントで見える。

## PC 起動時に自動でマウントする

```bash
tbx autostart status     # 既定は無効
tbx autostart enable     # 登録（Windows は HKCU\...\Run、Linux は systemd --user）
tbx autostart enable --crypt   # 暗号化してマウントする
tbx autostart disable
```

登録するのはマウントだけ。転送やログインは自動で走らせない。
暗号化を有効にしている場合は、`--crypt` を付けなくても暗号化付きで登録する
（平文で生えると意図と違うため）。

## インストール

- Windows: Release の `tbx-<version>-setup.exe`。管理者権限は不要で
  `%LOCALAPPDATA%\silentbox` に入る。構成画面で「WinFsp も入れる」を選ぶと、
  同梱された WinFsp の公式インストーラ（未改変）を起動する。選ばなければ
  何も入れない（`tbx mount --mode webdav` で使える）。インストーラは
  ユーザーの PATH へ `%LOCALAPPDATA%\silentbox` を追記する。既存の PATH は
  1 件も失わない（1024 文字を超える PATH でも実測で確認）。アンインストール
  すると PATH からも外れる。
- Linux: Release の `tar.gz` に `install.sh` が入っている。
  `PREFIX`（既定 `~/.local`）へ置く。

## マウントの使い方

```bash
# Windows
tbx mount T: --mode winfsp     # WinFsp がある環境（推奨）
tbx mount T: --mode webdav     # インストーラ不要のフォールバック
tbx mount T:                   # auto: WinFsp があれば WinFsp、無ければ WebDAV

# Linux / macOS
tbx mount ~/tbx --mode fuse    # FUSE（--features fuse-backend が要る）
tbx mount ~/tbx --mode webdav  # WebDAV サーバだけ立てる
tbx mount ~/tbx                # auto: FUSE が使えれば FUSE、無ければ WebDAV
```

`--mode winfsp` は MSVC ターゲットでビルドした `tbx` が必要。
`winfsp-x64.dll` は exe の隣、または WinFsp の `InstallDir\bin` から読み込む。

`--mode fuse` は `fusermount3` と `/dev/fuse` が必要（実測: 無いと
`No such file or directory` で失敗する）。Unix の WebDAV モードは
接続コマンドを実行せず、URL と使える手段だけを表示する（`davfs2` は
root 権限と `/etc/fstab` を要求するため、勝手に走らせない）。

Windows の WinFsp、Linux の FUSE とも、一覧・読み出し・書き込み・上書き・
フォルダ作成/削除を実アカウントで確認済み。書き込みは API が全体置換しか
提供しないため、開いたハンドルごとにバッファへ溜めて閉じるときに 1 回送る。
失敗は `flush` / `release` から返す（黙って成功を装わない）。

## リリース

`v*` タグを push すると、4 ターゲットのビルドが走り Release に添付される。

| ターゲット | ランナー |
|---|---|
| `x86_64-pc-windows-msvc`（WinFsp 込み） | `windows-latest` |
| `x86_64-unknown-linux-gnu`（FUSE 込み） | `ubuntu-latest` |
| `x86_64-unknown-linux-musl` | `ubuntu-latest` |
| `aarch64-unknown-linux-gnu`（FUSE 込み） | `ubuntu-24.04-arm` |

タグと `Cargo.toml` の版が一致しないと失敗する。WinFsp は公式の
インストーラ（MSI）を**未改変のまま**インストーラに同梱する。構成画面で
選んだ人だけが導入する。ライセンスと同梱物の詳細は
`docs/THIRD-PARTY-NOTICES.md` を参照。

## テスト

```bash
cargo test --workspace --bins --lib
cargo test -p terabox-core --test live_anonymous -- --ignored --nocapture
```

前者はネットワーク不要。後者は認証不要の実通信（jsToken 抽出、未ログインの
`errno -6`、不正共有リンクの `errno 105`）を確認する。

## 注意

- TeraBox は End-to-End 暗号化ではない。運営側が内容へアクセスできる設計で、
  機密文書の保管先には向かない。この性質はクライアントでは変えられない。
- API は非公開で予告なく変わる。`docs/limitations.md` に未確定事項をまとめてある。
- WebDAV は `127.0.0.1` のみで待ち受ける。外部へ公開しない。
- 無料枠は恒久 30GiB と期間限定 994GiB の合計で、期限がある（実測）。

## 第三者ソフトウェア

Windows の配置には WinFsp の公式インストーラを同梱しています。

- WinFsp - Windows File System Proxy, Copyright (C) Bill Zissimopoulos
- https://github.com/winfsp/winfsp

同梱物・ライセンス条件・ハッシュは `docs/THIRD-PARTY-NOTICES.md` に記載しています。
