# 変更履歴

## 0.2.1 — 2026-11-07

### 変更点

- WinFsp の DLL 探査を実挙動に合わせた。`winfsp_init` は exe の隣を先に、
  次にレジストリ `HKLM\SOFTWARE\WOW6432Node\WinFsp` の `InstallDir` を読む。
  以前は `%ProgramFiles(x86)%` の固定パスだけを見ていたため、DLL があっても
  「利用可能」と誤報告し、実際のマウントは失敗していた。
- DLL を絶対パスで先に読み込むようにした（`preload_winfsp_dll`）。
  名前だけの `LoadLibraryW` は PATH に無いと失敗するため。
- 配布ビルドに暗号化を含めた。`--features crypt` を全ターゲットに追加する。
  機能自体は既定オフなので、`tbx crypt setup` を実行するまで挙動は変わらない。

### 新機能

- Windows のインストーラに WinFsp の公式インストーラ（未改変）を同梱した。
  構成画面の「WinFsp も入れる」を選ぶと、署名を確認した上で MSI を起動する。
  署名が無い・壊れている場合は実行せず、公式の配布ページを案内する。
- 任意セクション（WinFsp・自動起動）は既定で未選択にした。`/S` でも
  何も起きない。
- `docs/THIRD-PARTY-NOTICES.md` を追加し、WinFsp の著作権表示・
  リポジトリへのリンク・同梱 MSI のハッシュを記載した。

### 注意点

- WinFsp の MSI を同梱するため、Windows のインストーラが約 2.2 MB 大きくなった。
- 「WinFsp も入れる」を選ぶと、ドライバの導入のために管理者権限の確認が出る。
  キャンセルした場合は WinFsp が入らず、`tbx mount` は WebDAV にフォールバック
  しない（明示的に失敗する）。
- 暗号化と WinFsp の組み合わせは引き続き未対応。`--mode webdav` を使う。

## 0.2.0 — 2026-11-07

### 追加: 暗号化マウント（rclone crypt 互換）

「保存先には暗号文だけが残り、手元のマウントでは普通のファイルとして見える」
を実現する。**既定はオフ**で、`tbx crypt setup` を明示したときだけ有効になる。

- 内容は NaCl SecretBox（XSalsa20-Poly1305）で 64 KiB チャンク暗号。
- ファイル名も暗号化する（AES-256 の EME、base32hex）。
- 鍵導出は scrypt(N=16384, r=8, p=1) で 80 バイト。salt は任意。
- **rclone と双方向に互換**（実測）:
  - rclone が作った暗号文を tbx が復号できた。
  - tbx が作った暗号文を rclone が復号できた（`rclone cat m:secret.txt` が
    元の日本語を含む内容を返した）。
  - ファイル名も両者が同じ値になった。
- 平文のまま置かれたファイルや、別の鍵で作られたファイルは**一覧に出さない**
  （出した件数は stderr に 1 行で示す）。
- `tbx crypt status` / `setup` / `disable` を追加。
- `tbx mount <target> --crypt` で暗号化マウント。WebDAV と FUSE で動く。
  WinFsp は具象型を要求するため未対応で、**黙って平文に落とさず明示的に失敗**する。
- `tbx crypt-migrate <path>` で既存の平文を暗号化形式へ移す。既定は下見で、
  `--apply` を付けるまで何もしない。送信後にサーバ上のサイズを確認し、
  一致しなければ元を残す。元の削除は `--delete-source` を明示したときだけ。

### 追加: PC 起動時の自動マウント

- `tbx autostart status` / `enable` / `disable`。**既定は無効**。
- Windows は HKCU の `Run` キー（管理者権限不要）。
- Linux は `systemd --user` のユニット。`daemon-reload` を先に実行する
  （省くと `Unit silentbox.service does not exist` で失敗する。実測）。
- 登録するのはマウントだけ。転送やログインは自動で走らせない。

### 追加: インストーラ

- Windows: NSIS のインストーラ `tbx-<version>-setup.exe`。管理者権限を要求せず
  `$LOCALAPPDATA` に置く。WinFsp の入手先を開く任意セクションと、
  自動マウントの任意セクションがある（どちらも既定でチェックしない）。
  アンインストール時に利用者のデータを勝手に消さない（確認ダイアログで選ぶ）。
- Linux: `tar.gz` に `install.sh` を同梱。`PREFIX`（既定 `~/.local`）へ入れる。

### 追加: 診断

- `tbx doctor` に「暗号化」「自動マウント」の状態を追加。
  どちらも既定が無効であることを明示する。

### 修正

- ゴミ箱の `fidlist` は数値配列で送る（文字列配列では `errno 2`）。
- マウントの容量表示を実値にした（`get_volume_info`）。
- フォルダ一括ダウンロード、TUI の共有画面、複数アカウント、Linux の FUSE は
  0.1.1 で入っている。

## 0.1.1 — 2026-11-07

### 追加

- **複数アカウント**。`--account` / `TBX_ACCOUNT` / 索引の既定 / `default` の順で
  解決する。`tbx accounts` で一覧、`tbx use` で既定を切替、`tbx login --name` で
  追加、`tbx logout [<名前>] [--all]` で削除。秘密は資格情報ストアの
  `account:<名前>` に置き、`--store file` を明示したときだけ
  `secrets/<名前>.json` へ 0600 で書く（緩い権限のファイルは読む前に拒否）。
  台帳は `state-<名前>.db` に分離。旧 `session` 1 件は初回に `default` へ移行。
- **Linux / macOS の FUSE バックエンド**（`--features fuse-backend`）。
  純 Rust の `fuser` を使い、libfuse 無しでビルドできる。`--mode fuse` と
  `--mode auto`（Unix では FUSE 優先）。読み書き・`statfs` は実値。
- **フォルダ一括ダウンロード**。`tbx get /dir`。階層を再現し、`--jobs` で同時数。
  空フォルダも作る。
- **TUI の共有画面**。URL を入力して Enter で解決し、一覧とサイズを表示する。
- **タグ連動リリース**。`v*` タグで Windows（WinFsp）、Linux gnu（FUSE）、
  Linux musl、aarch64 の 4 本をビルドし Release に添付する。タグと
  `Cargo.toml` の版が食い違えば失敗する。CI は 3 OS でビルドと起動確認を行う。

### 変更

- 依存をターゲット別に分けた。資格情報ストアは Windows / macOS / Linux-gnu で
  有効、musl では積まない（`--store file` 前提）。
- `which` が Unix では実行ビットを見る。ブラウザ探索は Chrome 系の
  ディストリ別実行名（`google-chrome` / `chromium` / `brave-browser` など）に対応。
- WebDAV のドライブ接続は Windows のみ実行。Unix では接続コマンドを走らせず、
  URL と使える手段を表示する（`davfs2` は root 権限を要するため）。
- `rust-version` を 1.90 にした（1.82 では `clap_builder` などが
  `edition2024` を要求して解けない）。

### 修正

- **使われていない `share_dlink` を削除**。共有の直接ダウンロードは署名必須の
  `/share/streaming` に依存するため作らない。`/share/create` は
  `www.terabox.com` でも `/api/share/create` でも HTTP 405 だった（実測）。
- **宣言だけの `cfapi-backend` を撤去**。実装が 1 行も無く、依存だけを引いていた。
- FUSE の `AutoUnmount` を外した。既定の ACL（Owner）では
  `auto_unmount requires acl != Owner` でマウントできない（実測）。

### 実測

- Windows: 2 アカウントの切替、台帳の分離、旧セッションの移行、フォルダ一括取得
  （3 件・26.1 KiB・内容一致）、20 MiB の分割アップロード（5 パート・9.2 MiB/s）。
- Linux（Debian 13）: FUSE のマウント・一覧・全量読み出し・書き込み・`statfs`。
- 速度: 20 MiB の分割ダウンロードで 1 接続 1.80、2 接続 1.98、4 接続 2.20、
  8 接続 1.41 MiB/s。4 接続が最良。

## 0.1.0 — 2026-11-06

Windows マウント（WinFsp）の実アカウント検証で見つかった不具合を修正。

### 修正

- **一覧の欠落**: WinFsp のディレクトリ・マーカーは 8.3 短縮名（`tbx-prob`）で
  返る。フルネーム比較では一致せず 2 ページ目以降が落ちていた。
  `crates/terabox-mount/src/winname.rs` を追加し、短縮名でも一致させる。
- **削除直後の残骸**: 一覧キャッシュ（15 秒）を無効化していなかった。
  書き込み・削除・改名・フォルダ作成の後に親キャッシュを捨てる。
- **EOF での読み出し失敗**: WinFsp がサイズちょうどの offset で read を投げ、
  dlink サーバが範囲外の `Range` に HTTP 400 を返していた。
  `read_range` で終端をクランプして空を返す。
- **アップロード 405**: `superfile2` は PCS ノード（`c-jp.terabox.com`）へ送る。
  `method=locateupload` が返す `host` を使う。
- **上書きで別名が増える**: `/api/create` は同名を上書きしない。
  書き込み前に既存を削除してから送る。
- **`WriteAllText` の "Incorrect function"**: `overwrite` の既定実装が
  `STATUS_INVALID_DEVICE_REQUEST` を返すため、自前で実装した。
- **書き込み失敗が成功に見える**: 送信を `cleanup` まで遅らせていたが、
  `cleanup` は失敗を返せない。`write` の時点で送り、失敗を返す。

### 追加

- `errno 4000023` / `450016`（`need verify`）を日本語で表示する。
- `docs/protocol.md` にアップロードの実経路と上書き・削除・改名の実測を追記。
- `docs/limitations.md` にマウント検証の結果と未解決事項を追記。
- 試験 76 件（うち実ネットワーク 3 件は `ignored`）。

### 追加（2026-11-07 追記）

- ゴミ箱の復元が通るようになった。`fidlist` を **数値配列**（`[123]`）で送る。
  文字列配列では `errno 2`（param err）だった。完全削除も同じ形式に直した。
- `errno -8`（復元先に同名あり）を日本語で表示する。
- マウントの容量表示を実値にした。`/api/quota` の `total` / `used` を読み、
  取れなければ 0（不明）を返す。`tbx quota` は枠の内訳（恒久 30GiB +
  期間限定 994GiB）と期限も表示する。
- テスト残骸をゴミ箱から完全削除した（11 件）。

### 未解決

- 一定量の書き込み後、サーバが書き込み系 API に `need verify` を返す。
  読み取りとフォルダ作成は通る。解除条件は未特定。
- `opera=rename` は 6 通りのパラメータ形すべてで `errno 2`。
  クライアントは読み出し→再アップロード→削除で代替する（フォルダ改名は未対応）。

## 0.1.0 — 2026-11-06

初回実装。TeraBox 4.28.2（versionCode 707 / APKM SHA-256
`D81978F3…72EE5B`）を解析して作った非公式クライアント。

### 追加

- `terabox-core`
  - `/main` の HTML から `jsToken` / `bdstoken` / `uk` を取り出すブートストラップ
  - `/api/list`, `/api/filemetas`, `/api/quota`, `/api/user/getinfo`,
    `/api/shorturlinfo`, `/share/list`, `/share/transfer`,
    `/rest/2.0/pcs/file`, `/api/precreate`, `/rest/2.0/pcs/superfile2`,
    `/api/create`, `/api/recycle/*`, `/api/filemanager`
  - `errno` を 1 箇所で日本語化（未知の値は「未分類」として生値を保持）
  - 分割ダウンロード（レジューム・接続開始の分散）
  - 分割アップロード（MD5 計算・重複排除の検出・再試行）
  - SQLite の転送台帳（分割単位の進捗）
  - `RemoteFs` 抽象（WinFsp / WebDAV 共通）
- `terabox-cli`（`tbx`）
  - `login`（Chrome/Edge を専用プロファイルで起動し CDP で Cookie 取得 / `--paste`）
  - `logout` `whoami` `ls` `quota` `get` `put` `share` `play` `trash` `rm`
    `mkdir` `mount` `tui` `doctor` `transfers`
  - 秘密情報は Windows 資格情報マネージャのみ
- `terabox-tui`
  - 一覧 / 転送ログ / 容量 / ヘルプの 2 ペイン画面
  - ダウンロード・外部プレーヤ起動をキーから実行
- `terabox-mount`
  - WebDAV（`127.0.0.1` 限定、PROPFIND/GET/HEAD/PUT/DELETE/MKCOL/MOVE、Range 対応）
  - WinFsp（`winfsp` 0.13、`--features winfsp-backend` + MSVC ターゲット）
- プロジェクト固有スキル `.agents/skills/terabox-native/`
- 解析記録 `docs/protocol.md` と制約 `docs/limitations.md`

### 実測で確定した事項

- `windows-gnu` ターゲットでは `winfsp-sys` がビルドできない（MSVC 必須）
- WinFsp は遅延リンクが必須で、DLL が無いと 1288/1285 で落ちる
- `/main` の jsToken は 192 桁の hex
- 未ログインの `/api/list` は `errno -6`、不正共有リンクは `errno 105`

### 未対応（`docs/limitations.md` に詳細）

- 署名必須 API（`/api/streaming` 系）
- QR ログイン（エンドポイント未特定）
- 認証済み API の実応答による検証（初回ログイン後に確定させる）
- フォルダ一括ダウンロード
