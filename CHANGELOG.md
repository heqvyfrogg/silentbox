# 変更履歴

## 0.1.1 — 2026-11-07

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
