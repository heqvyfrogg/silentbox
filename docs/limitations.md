# 未確定事項と制約

この文書は「できていないこと」「確かめていないこと」を隠さずに並べる。
実装はここに書かれた範囲でしか実力を保証しない。

## 解析で確定していないこと

| 項目 | 状況 | 影響 |
|---|---|---|
| 署名検証 | **未実施**。REA の JADX プロバイダは APK 署名を検証しない。APKM の `META-INF/APKMIRRO.RSA` は存在するが中身は検証していない | 配布物の真正性は保証できない |
| 認証済み API の応答形状 | `/api/list` `/api/quota` `/api/user/getinfo` は未ログイン時の応答しか実測していない。フィールド名は APK 内モデルと既知の Web API 仕様からの推定を含む | 初回ログイン後に実応答で確定させる必要がある |
| QR ログイン | `QrCodeLoginService` の存在は確認したが、`/passport/qrcode/...` は匿名アクセスで 404。完全なエンドポイントを特定できていない | QR ログインは実装していない |
| `errno 140` | 共有リンクで返るが、意味は「期限切れ」と推定 | 表示文言が推定を含む |
| `errno 105` / `460030` | 共有リンクで実測。`1test` は 460030「dragdrop」、`1zzzzzzzzz` は 105「share link error stat」。どちらもリンク不正として扱う | 入力により値が変わるため、特定値へ依存した分岐はしない |
| 単一ファイル上限 | サーバが明示値を返す場合のみ表示。ユーザー提示の「4GB / 128GB」の食い違いは未検証 | 上限判定はサーバ値があるときだけ働く |
| 同時接続数の制限 | サーバ側の制限値を取得していない | `--conn` を上げすぎると失敗する可能性がある |
| 解像度制限（480p） | サーバ判定。クライアント側で偽装しない | 取得できた dlink をそのままプレーヤへ渡す |
| `PreCreateDataBean.returnType == 2` | 「重複排除」と推定 | 誤推定なら送信をスキップして失敗する |
| `partseq` の起点 | 0 始まりと仮定 | 1 始まりなら初回アップロードが失敗する |
| 推奨パートサイズ | 4MiB と仮定 | 大きすぎると失敗、小さすぎると低速 |
| 共有リンク内の直接ダウンロード | `/share/create` は HTTP 405。共有内の dlink は署名必須の `/share/streaming` 経由でしか引けない | 共有は一覧表示と領域への保存まで（直接 DL は作らない） |

## 実装していない機能

- **署名必須 API**（`/api/streaming`, `/share/streaming`）。ネイティブの
  `get_signature_md5` と `libnetapi-security-rand` に依存するため再実装しない。
  共有リンクの**中身を直接ダウンロードする経路はこれに依存するため作らない**。
  実測: `/share/create` は `www.terabox.com` でも `/api/share/create` でも
  HTTP 405 だった。共有内の dlink を引く手段が署名必須の `/share/streaming`
  だけなので、共有は「一覧表示」と「自分の領域へ保存」までとする。
- **QR ログイン**。エンドポイント未特定のため。
- **フォルダ改名**。`opera=rename` は 6 通りのパラメータ形すべてで `errno 2`。
  ファイルは読み出し→再アップロード→削除で代替するが、フォルダは未対応。
- **部分書き込み・サイズ変更**（マウント経由）。TeraBox の API はオブジェクト
  全体の置き換えしか提供しないため、開いたハンドル全体を送り直す。
- **オフラインキャッシュ**。マウントは毎回サーバへ取りに行く（一覧は 15 秒キャッシュ）。

## ビルドと実行の制約

| 事象 | 実測結果 | 対処 |
|---|---|---|
| `x86_64-pc-windows-gnu` で WinFsp | `winfsp-sys` が `unsupported triple` で停止 | MSVC ターゲットでビルドする |
| WinFsp の DLL が見つからない | 起動時に 1285（0x505） | `winfsp-x64.dll` を exe の隣に置くか、WinFsp の `bin` を PATH へ |
| WinFsp の `system` 機能 | `winfsp/winfsp.h` が見つからず失敗 | 既定（同梱インポートライブラリ）を使う |
| `which` クレート | `winsafe` 経由で `ktmw32` を要求し GNU リンクが失敗 | 自前の `which` 実装に置換済み |
| 未昇格での WinFsp マウント | 未検証（この環境は非管理者） | 失敗する場合は管理者で実行する |

## セキュリティとプライバシーの前提

- `ndus` / `jsToken` / `bdstoken` は Windows 資格情報マネージャだけに保存する。
  設定ファイル・ログ・台帳 DB には書かない。`Session` の `Debug` は値を伏せる。
- WebDAV バックエンドは `127.0.0.1` のみで待ち受ける。Windows の WebDAV
  クライアントは Basic 認証を平文で送るため、外部公開は絶対にしない。
- ログインブラウザは専用の `--user-data-dir` を使い、終了時に落とす。
  利用者の常用プロファイルには触れない。
- TeraBox は End-to-End 暗号化ではない。運営側が内容へアクセスできる設計で
  あり、機密文書の保管先としては不適。この前提はクライアントでは変えられない。
- APK に含まれる危険な権限（`MANAGE_EXTERNAL_STORAGE`, `READ_PHONE_STATE`,
  `GET_ACCOUNTS`, `REQUEST_INSTALL_PACKAGES`, `SYSTEM_ALERT_WINDOW`,
  `RECORD_AUDIO`, `DUMP`, `DETECT_SCREEN_CAPTURE` ほか計 78 件）は、
  デスクトップ実装では要求しない。

## API 変更への備え

TeraBox の API は非公開で、予告なく変わる。

- `tbx doctor` が `/api/list` と `/api/quota` の応答を確認する。
- `errno -6` は再ログイン要求として扱い、無限リトライしない。
- トークン抽出は HTML の 2 箇所（`templateData` と `fn("<hex>")`）に依存する。
  どちらかが変われば `BootstrapFieldMissing` で明示的に落ちる（空値で続行しない）。
- 未知の `errno` は「未分類」と表示し、生値を隠さない。

## 2026-11-07 実測: 容量表示と無料枠の内訳

エクスプローラの「PC」欄に出る容量は、当初 `1024GB / 512GB 空き` の固定値だった
（`get_volume_info` にハードコードしていた）。実測で `/api/quota` が本物の値を
返すことを確認し、実値へ差し替えた。

| 項目 | 実測値 |
|---|---|
| 総容量 | 1,099,511,627,776 バイト（1 TiB） |
| 使用量 | 5,369,813 バイト |
| 残量 | 1,099,506,257,920 バイト |
| 無料枠上限（恒久） | 32,212,254,720 バイト（30 GiB） |
| 期間限定枠 | 1,067,299,373,056 バイト（994 GiB） |
| 期間限定枠の期限 | 2026-12-07 00:06（`time_limit_quota_expire_time` = 1796569611） |

`/api/quota` の生応答には `extra` があり、枠の内訳を持っている。

```json
"extra": {
  "init_quota_type": "permanent_30g_temp_994g",
  "time_limit_quota_expire_time": 1796569611
}
```

`permanent_30g_temp_994g` は「恒久 30GB + 期間限定 994GB」を意味する。
つまり**無料 1TB は恒久枠ではなく、994GB が期限付き**である。期限は約 30 日後。
公式ページの「期間限定（limited-time）」表記と整合する。

この内訳は `tbx quota` で表示する。合計だけを見せると「永久に 1TB」と
誤解されるため、恒久と期間限定を分けて出す。

容量が取れない場合は `get_volume_info` が 0 を返す（推測値で埋めない）。
0 はエクスプローラ上で「不明」として見える。

## 2026-11-07 実測: Windows マウント（WinFsp）

初回ログイン後の実アカウントで `tbx mount T: --mode winfsp` を実行し、
以下を実測した。

| 項目 | 結果 |
|---|---|
| ドライブ割り当て | 成功（`T:` が `dir` に出る、ボリューム名 `TeraBox`） |
| 一覧 | 成功（`TeraBox初心者ガイド.pdf` 5,364,374 バイトと `tbx-probe` が出た） |
| 読み出し | 成功（5,364,374 バイトを全量読み、先頭が `%PDF`） |
| フォルダ作成 | 成功（`T:\` 直下、サブフォルダ両方） |
| フォルダ削除 | 成功（サーバ側からも消えた） |
| ファイル書き込み | **制限あり**（下記） |
| ファイル上書き | **制限あり**（下記） |

### 実装中に直した不具合（すべて実測で再現 → 修正 → 再確認）

1. **一覧が 2 件目以降欠落**
   WinFsp のディレクトリ・マーカーは 8.3 短縮名（`tbx-prob`）で返る。
   フルネーム比較では一致せず、2 ページ目が丸ごと落ちていた。
   `crates/terabox-mount/src/winname.rs` で短縮名を生成し、両方で一致させる。
2. **削除直後の一覧に残骸**
   一覧キャッシュ（15 秒）を無効化していなかった。
   書き込み・削除・改名・フォルダ作成の後に親ディレクトリのキャッシュを捨てる。
3. **EOF 位置の読み出しで I/O エラー**
   WinFsp はサイズちょうどの offset で read を投げる。dlink サーバは範囲外の
   `Range` に HTTP 400 を返すため、`read_range` で終端をクランプして空を返す。
4. **アップロードが HTTP 405**
   `superfile2` は PCS ノード（`c-jp.terabox.com`）へ送る必要がある。
   `/rest/2.0/pcs/file?method=locateupload` が返す `host` を使う。
5. **上書きで別名ファイルが増える**
   `/api/create` は同名パスを上書きせず `name_20261106_234426.bin` を作る。
   書き込み前に既存を削除してから送る（削除→作成は実測で同パスに置けた）。
6. **`WriteAllText` が "Incorrect function"**
   WinFsp は既存ファイルを開き直すとき `overwrite` を呼ぶ。既定実装が
   `STATUS_INVALID_DEVICE_REQUEST` を返すため、自前で実装した。
7. **書き込み失敗が成功に見える**
   送信を `cleanup` まで遅らせていたが、`cleanup` は失敗を返せない。
   `write` の時点で送信し、失敗を呼び出し元へ返すようにした。

### 未解決: 書き込み系のサーバ側拒否（`need verify`）

一定量の書き込み後、サーバが書き込み系 API に `errno 4000023`、削除に
`errno 450016` を返すようになった（どちらもサーバ文言は `need verify`）。
- セッションは有効（`/api/list` `/api/quota` は正常）
- 解除条件は未特定（再ログインで解除されるかは未検証）
- クライアント側では解除できないため、`errno` を日本語で明示し、
  マウントの書き込みは**失敗として返す**（黙って成功を装わない）

読み取り（`/api/list` `/api/quota` `/api/recycle/list`）と、フォルダ作成
（`/api/create` の isdir）、ゴミ箱からの完全削除（`/api/recycle/delete`）は
通り続ける。

### ゴミ箱 API の `fidlist`（実測）

`/api/recycle/restore` は **数値の配列**（`[1089841491208719]`）しか受けません。
文字列の配列（`["1089841491208719"]`）、単体値、`fs_id`/`fid` という別キー名は
すべて `errno 2`（param err）でした。実装は数値配列に直しました。

復元先に同名の項目があると `errno -8`（`file already exist`、`faillist` に該当 ID）
を返します。通常の削除が止まっている間でも、テスト残骸の掃除は
`/api/recycle/delete` で進められます。
## 検証済みの範囲（2026-11-06 時点）

- `cargo test --workspace`: 76 件 green（実ネットワーク 3 件は `ignored` で別枠）
- `cargo test -p terabox-core --test live_anonymous -- --ignored`: 3 件 green
  （jsToken 192 桁の抽出、未ログイン errno -6、不正共有リンクの業務エラー）
- MSVC + `winfsp-backend` での tbx ビルドと `tbx --version` / `tbx doctor` の実行
- WebDAV ハンドラの単体試験（PROPFIND / GET / HEAD / Range / 404 / Depth 制限）
- 分割ダウンロードのスタブ試験（全バイト一致、中断からのレジューム、範囲取得）
- Chrome のリモートデバッグポート起動と `/json/list` からのページターゲット取得
  （CDP 経路の前提が成立すること）
- `scripts/verify.ps1` による受け入れ確認（全項目成功、終了コード 0）

## 検証していない範囲

- **共有リンクの解決**。有効な共有 URL を用意していないため、実 URL での
  一覧取得と保存は未実施（不正 URL の `errno 105` は実測済み）。共有の作成は
  `/share/create` `/api/share/create` とも HTTP 405 で作れないため、
  自分で作ったリンクでの確認もできていない。
- **WebDAV の `net use` 接続**。Windows の `WebClient` は停止中で、非管理者では
  起動できない（実測: `Cannot open WebClient service`）。Windows の WebDAV
  クライアントは Basic 認証を要求するが、実装は認証を持たない。
- **TUI の実端末での描画**。作業環境が非対話のため確認できない。共有ペインの
  入力・解決は実装済みだが、端末での見た目は未確認。
- **aarch64 の実行**。CI のランナーでビルドするだけで、実機での起動は未確認。
- **musl ビルドの資格情報ストア**。musl では keyring を積まないため、
  `--store file` が必須。この経路の実機確認は x86_64-gnu のみ。

アカウントでのログイン・一覧・ダウンロード・アップロード・削除・復元・
フォルダ一括取得・複数アカウントの切替は実測済み。Windows の WinFsp と
Linux の FUSE はマウント・読み書き・statfs まで実測済み。

これらは初回の実ログイン後に確認し、結果をこの文書へ追記する。

## 2026-11-07 実測: Linux の FUSE

Debian 13（`6.12.113+deb13-amd64`）実機で `tbx mount ~/mnt --mode fuse` を実行し、
以下を実測した。

| 項目 | 結果 |
|---|---|
| マウント | 成功（`silentbox on /tmp/fusemnt type fuse (rw,nosuid,nodev,relatime,user_id=1001,default_permissions)`） |
| 一覧 | 成功（`TeraBox初心者ガイド.pdf` が出た） |
| 読み出し | 成功（5,364,374 バイトを `cp` で取得、先頭 `%PDF-`） |
| 書き込み | 成功（`hello from linux fuse` を書き、サーバ側の一覧にも 22 B で反映） |
| `statfs` | 実値（Blocks Total 268,435,456 × 4 KiB = 1 TiB、Free 268,434,146） |

### 実測で判明した制約

- `fusermount3` が無いと `No such file or directory (os error 2)` で失敗する。
  `fuse3` パッケージの導入が要る（`/dev/fuse` だけでは足りない）。
- `MountOption::AutoUnmount` は `acl != Owner` を要求する。既定の `Owner` のまま
  付けると `auto_unmount requires acl != Owner` でマウントできない。実装では
  付けていない。
- `Config` は `non_exhaustive` なので構造体リテラルで組み立てられない。
  既定を作って項目を差し替える。
- 資格情報ストア（Secret Service）が無い環境では
  `Platform secure storage failure: DBus error: The name org.freedesktop.secrets
  was not provided by any .service files` になる。`--store file` を使う。
## 2026-11-07 実測: 暗号化マウント（rclone crypt 互換）

既定はオフ。`tbx crypt setup` を明示したときだけ有効になる。

### 互換性（実測）

| 方向 | 結果 |
|---|---|
| rclone → tbx | rclone が書いた暗号文を tbx が復号できた（本文一致） |
| tbx → rclone | tbx が書いた暗号文を `rclone cat` が復号できた（日本語含む内容一致） |
| ファイル名 | 両者が同じ暗号名を生成（`ac967he4f77fg0aqdv61ui4evs` 等） |
| 移行後のファイル | rclone から `secret.txt` として読め、内容も一致 |

### 実装中に実測で確定したこと

- **認証タグの位置**: Go の `secretbox.Seal` も Rust の `aead` も
  「暗号文 || タグ」。並べ替えは不要（当初は「タグ先頭」と誤認していた）。
- **scrypt の `Params::new`**: `len` は 10..=64 しか許さないが、`scrypt()` は
  出力長を `out` スライスから取る。rclone は 80 バイト必要なので `len=32` を
  渡し、80 バイトの出力スライスを渡す。
- **EME の既知ベクタ**: rfjakob/eme の 16 バイト・2048 バイトのベクタで一致。
- **WebDAV の `net use`**: `WebClient` サービスが停止していると
  `System error 67`（ネットワーク名が見つからない）で失敗する。暗号化そのもの
  ではなくドライブ割り当ての問題。

### 未対応

- **暗号化 + WinFsp**: WinFsp のホストは具象 `TeraBoxFs` を要求するため、
  暗号化層を差し込めない。`--mode webdav` か FUSE を使う。依頼時は明示的に
  失敗する（平文でマウントしない）。
- **暗号化されたファイルの部分書き込み**: API が全体置換しか提供しないため、
  開いたハンドル全体を送り直す。
- **`obfuscate` のファイル名方式**: `standard` と `off` のみ実装。

## PC 起動時の自動マウント（実測）

- Windows: HKCU の `Run` キーへ `"<exe>" mount T:` を書く。`reg query` で
  登録・削除を確認済み。
- Linux: `systemd --user` のユニット。`daemon-reload` を省くと
  `Unit silentbox.service does not exist` で失敗する（実測 → 修正）。
- **独自の `XDG_CONFIG_HOME`**: `systemd --user` は起動時の値を固定して持つため、
  プロセスごとに変えた場所のユニットは認識されない（実測）。警告を出す。
- 登録するのはマウントだけ。転送やログインは自動で走らせない。
## 2026-11-07 実測: インストーラの日本語が文字化けした

アンインストール確認のダイアログで、本文だけが
`è¨­å®šãƒ»ã‚­ãƒ£ãƒƒã‚·ãƒ¥...` のような列になった。見出し
（「silentbox (tbx) 0.2.0 アンインストール」）は正常だった。

### 根本原因

`install/installer.nsi` を **UTF-8 BOM なし**で保存していた（先頭 3 バイトが
`3B 20 74`）。NSIS は BOM の無い `.nsi` を ANSI として読むため、ランナーの
既定コードページ（en-US の cp1252）で UTF-8 の日本語が Latin-1 として解釈され、
上記の列になった。MUI の標準文言は `Japanese.lang` から読まれるので無事だった
（本文だけが壊れたのはこれで説明できる）。

### 対策

- `installer.nsi` を UTF-8 **BOM あり**で保存する。
- CI に `test-installer` ジョブを追加し、毎 push でインストーラをビルドする。
  BOM の有無を `head -c 3 | xxd` で機械的に確認し、無ければ失敗させる。
  リリース時だけ作ると、壊れたまま配布される（今回それが起きた）。
## 2026-11-07 実測: インストーラの検証で分かったこと

### NSIS の落とし穴（すべて実測で踏んだ）

| 症状 | 原因 | 対策 |
|---|---|---|
| 日本語が `è¨­å®š...` になる | installer.nsi に UTF-8 BOM が無い。NSIS が ANSI（cp1252）として読む | BOM を付ける。CI で `head -c 3 \\| xxd` を確認 |
| `invalid VIProductVersion format` | `/DVERSION=0.0.0-ci` を 4 桁数値に使った | 表示用と `VERSION4` を分ける |
| `makensis が見つかりません` | ランナー同梱の NSIS が PATH/所定パスに無い | Chocolatey で導入し、実体パスを確定 |
| `/DEXE` に絶対パスが渡らない | `/D` は引用符なしの 1 トークン | 相対パスで渡す |
| 入る exe 名が `tbx-installer-input.exe` | `File` はソースのベース名を使う | `File /oname=${APP_EXE}` |
| アンインストールで空ディレクトリが残る | `RMDir` 非再帰 + 作業ディレクトリが `$INSTDIR` | `SetOutPath $TEMP` + `RMDir /r` |
| `/S` でも確認 MessageBox が出て止まる | `MessageBox` が無音を考慮していない | `IfSilent` で飛ばす（既定はデータを残す） |

### 検証できたこと

- 無音インストール → `tbx 0.2.0` が起動。
- 無音アンインストール → `%LOCALAPPDATA%\silentbox` が完全に消える。
- 対話アンインストールの確認本文が
  「設定・キャッシュ・保存済みパスワードも削除しますか？」と正しく表示される
  （UI オートメーションで文字列を取得）。
- Linux の `install.sh` が Debian 上で `PREFIX` を尊重して入り、`tbx 0.2.0` が起動。

### 検証できていないこと

- 対話アンインストールの「はい」を押した場合のデータ削除は、GUI 自動操作が
  NSIS のウィンドウ生成タイミングと噛み合わず確認できていない。無音時の
  「残す」経路だけが実測済み。
- インストーラのデジタル署名は無い（SmartScreen の警告が出る可能性がある）。
## 2026-11-07 実測: WinFsp が「利用可能」と表示されたのにマウントが失敗した

`tbx doctor` が `[ok] WinFsp: 利用可能` を出した直後、`tbx mount T:` は
`WinFsp を初期化できませんでした（WIN32_ERROR）` で失敗した。

### 根本原因

`winfsp_init`（winfsp 0.13）は次の順で DLL を探す。

1. `LoadLibraryW("winfsp-x64.dll")`（名前だけの読み込み）
2. `system` feature が有効な場合のみ、レジストリ `HKLM\SOFTWARE\WOW6432Node\WinFsp`
   （または `HKLM\SOFTWARE\WinFsp`）の `InstallDir\bin\winfsp-x64.dll`

一方 `winfsp_available()` は `%ProgramFiles(x86)%\WinFsp\bin\winfsp-x64.dll` の
存在だけを見ていた。この環境には DLL が実在したが、`InstallDir` は未登録で、
PATH にも exe の隣にも無かったため、判定は真・実行は失敗という食い違いになった。

### 対策

- `winfsp_available()` を、`winfsp_init` が実際に試す位置（exe の隣 →
  `InstallDir\bin` → 既定のインストール先）の列挙に揃えた。
- `preload_winfsp_dll()` で解決済みの DLL を絶対パスで読み込む。
  `LoadLibraryW` は読み込み済みモジュールを返すので、その後の名前読み込みが
  解決できる。実測: DLL を exe の隣に置かずに `T:` へマウントでき、
  読み込んだパスは `C:\Program Files (x86)\WinFsp\bin\winfsp-x64.dll` だった。
- `system` feature は使わない。有効にすると `winfsp-sys` がビルド時に
  `winfsp/inc/winfsp.h` を要求するが、WinFsp の MSI は `inc` を含まない
  （実測: `fatal error: 'winfsp/winfsp.h' file not found`）。ランタイムだけを
  入れた環境でビルドできないため、DLL の探索は自前で行う。

### 未検証

WinFsp を導入していない環境で `preload_winfsp_dll` が確実に失敗し、
案内メッセージが出ることは、この環境では再現できていない（既に導入済み）。

## 2026-11-07 実測: 無音インストールが任意セクションを実行した

`tbx-0.2.0-setup.exe /S` で入れたところ、構成画面で何も選んでいないのに
HKCU の `Run` に自動起動が登録され、WinFsp の配布ページも開いた。
NSIS の `Section` は既定で「選択済み」になるため、任意のつもりの項目が
無音でも実行されていた。

対策として任意セクションを `Section /o` にし、既定で未選択にした。
`/S` でも何も起きない。実測: 0.2.1 のインストーラを `/S` で実行した後、
`Run` キーは空で、`chrome` の新規起動も無かった。
## 2026-11-07 実測: MSI の署名検証だけでは改変を検出できない

インストーラは WinFsp の MSI を起動する前に `Get-AuthenticodeSignature` で
署名を確かめている。この挙動を単体で実測した。

| 入力 | Status | 期待 |
|---|---|---|
| 公式の MSI（未改変） | `Valid` | 実行する |
| 途中の 1 バイトを書き換えた MSI | `Valid` | 止めたいが止まらない |
| 半分に切り詰めた MSI | `NotSigned` | 止める（実際に止まる） |

MSI は署名対象外の領域（チェックサムや証明書テーブルなど）を含むため、
一部の改変では署名が有効なままになる。したがって署名検証は「明らかに
壊れた・未署名のもの」を止める二次防御であって、改変検出の主力ではない。

改変検出の主力は CI の SHA-256 照合。CI は公式リリースから MSI を取得し、
`073A70E00F77423E34BED98B86E600DEF93393BA5822204FAC57A29324DB9F7A` と
一致することを確かめてから同梱する。配布されるインストーラに含まれる MSI は、
この検証を通ったものに限られる。

利用者側で同梱 MSI のハッシュを確認する手段は用意していない（インストーラは
NSIS でハッシュ計算をしない）。必要なら `certutil -hashfile` で確認できる。