# TeraBox プロトコル解析メモ

対象: `com.dubox.drive` 4.28.2（versionCode 707）

- 配布物: `com.dubox.drive_4.28.2-707_2arch_7dpi_11lang_3feat_084ec5f76fca54e60ebff78767078b8a_apkmirror.com.apkm`
- SHA-256: `D81978F30775B4542819A2DADF09F764BE788331F9C6737B36D1611AFE72EE5B`
- サイズ: 237,913,310 バイト
- APKM 内の `base.apk`: 189,627,275 バイト、ZIP エントリ 12,040 件、DEX 16 本
- 署名検証: **未実施**（REA の JADX プロバイダは署名を検証しない。APKM 側の `META-INF/APKMIRRO.RSA` は存在するが内容検証はしていない）

このメモは「観測した事実 / 推論 / 未確定」を分けて書く。数値と文字列はすべて
同梱 APK か実ネットワーク応答から採取したもので、推測値は置かない。

## 0. 解析環境と証跡

| 項目 | 値 |
|---|---|
| REA | 6.7.0 |
| JADX プロバイダ | `jadx-headless-mcp` 0.7.1（JAR SHA-256 `6e5eacf500b64292bfb73c49797c1958f6ee44646e43e868039ae7feb573ff75`） |
| JDK | Temurin 21.0.13+9 |
| ヒープ上限 | 6144–8192 MiB |
| クラス総数 | 73,313 |
| リソース総数 | 12,040 |

主な Evidence ID:

| 対象 | Evidence |
|---|---|
| APK 全体（マニフェスト・権限） | `ev_96ec61856c9950c2715653c2cd32e7bab65a9b5dd6eb422a486bd77db63ce6aa` |
| `KmpFileRepository` | `ev_5056c6bda6120d3343ef177aecf10323af04ec167d0bfe32a3ebbe77b048cbc2` |
| `PreCreateDataBean` | `ev_f75c7d1faf876e807a72fbf674cd496a8d175a1881374cf3d6a4edca9fa5f605` |
| `Quota` | `ev_6ed54b6421d84ff7729bebdae04d5331e8201028c4608aaf387ec7e94dd4d7a6` |
| `QuotaExtraInfo` | `ev_c6066f657a41d4aa1ac052037ef022bf5bdd8f786a069c7a28f97e1e264086a7` |
| `LoginResponse` | `ev_52572def1c17fb4097b0ed2123e8f5fe66f6a8367f6b0a7a04308111862cbfcf` |
| `QrCodeInfoResponse` | `ev_78b5c8122e814cf9bd1c64270f4144b9a4841582e5e76e7212d03f18c1f67345` |
| `OauthCheckQrcodeResponse` | `ev_1063c3d7d4fae04951caa6a6382e2c5d6b3f2305f748b67f407896dce3902b19` |
| クラス名検索 | `ev_ebb78652f135d1766f208fbe3fdd94d40374b1d227000a849180d93ebdd27115` |

生データは `analysis/*.json`（REA のエンベロープ全体）にある。

## 1. アプリの構造

- マニフェスト: `minSdk 23` / `targetSdk 36` / `compileSdk 36`。
- 権限 78 件。特徴的なもの:
  `MANAGE_EXTERNAL_STORAGE`, `READ_PHONE_STATE`, `READ_CALENDAR`, `WRITE_CALENDAR`,
  `GET_ACCOUNTS`, `MANAGE_ACCOUNTS`, `REQUEST_INSTALL_PACKAGES`,
  `SYSTEM_ALERT_WINDOW`, `RECORD_AUDIO`, `DUMP`, `DETECT_SCREEN_CAPTURE`,
  `READ_APP_BADGE`, `KILL_BACKGROUND_PROCESSES`, `ACCESS_ADSERVICES_*`。
- `android.permission.DUMP` と `DETECT_SCREEN_CAPTURE` は、広告・分析・
  スクリーンショット検知のいずれかを目的としていると推測できるが、
  コード上の利用箇所までは追っていない（**未確定**）。
- ネイティブライブラリは `split_config.arm64_v8a.apk` 側:
  `libjni-kservice.so`(32.4MB), `libvastplayer.so`, `libjni-kernel.so`,
  `libdubox-security.so`, `libnetdisk-security-rand.so`, `libbaiduprotect_sec.so`,
  `libflutter.so` + `libapp.so`（Flutter 部分がある）。

広告 SDK の名前空間出現数（DEX 文字列）:

| SDK | 出現数 |
|---|---|
| Pangle / bytedance openadsdk | 2,231 |
| Moloco | 2,198 |
| Mintegral (mbridge) | 1,902 |
| Smaato | 1,658 |
| Vungle | 1,003 |
| AppLovin | 571 |
| InMobi | 112 |
| Facebook Audience Network | 79 |

8 系統が同居する。広告だけを剥がす改変は現実的でなく、**クライアントを自作する
以外に広告ゼロを達成する方法はない**という判断の根拠になる。

## 2. API 面（観測）

すべて DEX 文字列テーブル（607,036 件）と Web 版の応答から採取した実在のパス。

### 2.1 共通パラメータ

```
app_id=250528&web=1&channel=dubox&clienttype=0&jsToken=<192桁hex>
Cookie: ndus=<セッションID>; PANWEB=1
```

`app_id` は 2 系統ある。Web/PCS 系が `250528`、アプリ系が `16892322`
（`/rest/2.0/pcs/superfile2?...&app_id=16892322` の形で DEX に存在）。
本クライアントはセッション Cookie 方式の `250528` を使う。

### 2.2 エンドポイント

| 用途 | メソッド/パス | 追加の必須項目 |
|---|---|---|
| 一覧 | `GET /api/list` | `dir, order, desc, page, num` |
| メタ一括 | `GET /api/filemetas` | `target`/`fsids` |
| 容量 | `GET /api/quota` | `checkexpire=1, checkfree=1` |
| ユーザー | `GET /api/user/getinfo` | — |
| 共有情報 | `GET /api/shorturlinfo` | `shorturl, type=0, root=1` |
| 共有一覧 | `GET /share/list` | `shorturl, dir, root` |
| 共有保存 | `POST /share/transfer` | `shareid, from=1`, 本文 `fsidlist, path` |
| DL リンク | `GET /rest/2.0/pcs/file?method=download` | `path` |
| UL 受付 | `POST /api/precreate` | `path, size, block_list, autoinit=1` |
| UL 分割 | `POST /rest/2.0/pcs/superfile2?method=upload` | `uploadid, partseq, partoffset, path` |
| UL 確定 | `POST /api/create` | `a=commit, path, size, uploadid, block_list` |
| ゴミ箱一覧 | `GET /api/recycle/list` | `page, num` |
| ゴミ箱復元 | `POST /api/recycle/restore` | 本文 `fidlist` |
| ゴミ箱削除 | `POST /api/recycle/delete` | 本文 `fidlist` |
| ゴミ箱空 | `POST /api/recycle/clear` | `async=1` |
| 削除 | `POST /api/filemanager?opera=delete` | 本文 `filelist` |
| 名前変更 | `POST /api/filemanager?opera=rename` | 本文 `filelist, newname` |
| フォルダ作成 | `POST /api/create` | `a=commit, path, isdir=1` |
| ストリーミング | `GET /api/streaming` ほか | **ネイティブ `sign=` 必須** |

### 2.3 アプリ固有 API を採用しない理由

`libdubox-security.so` に `_ZN7netdisk17get_signature_md5E...`（JNI 経由の署名生成）が
あり、`libnetdisk-security-rand.so` には OpenSSL 由来の `AES_*`, `ASN1_*`,
`CMS_*` 群が同居する。`com/dubox/drive/anticheating/*` も存在する。

- `/api/streaming` と `/share/streaming` は `sign=`/`secret_token=` を要求し、
  これはネイティブ側でしか生成できない。
- したがって **署名必須の API は実装対象外**とし、セッション Cookie で通る
  Web API だけを使う。署名生成の再実装はしない（難読化と難読化解除の
  イタチごっこになり、保守できない）。

## 3. 認証（観測 + 実測）

### 3.1 トークンの入手

`GET https://www.terabox.com/main` は未ログインでも 776,444 バイトの HTML を返す
（2026-11-06 実測）。この HTML には 2 種類の埋め込みがある。

1. `var templateData = {"bdstoken":"","pcftoken":"...","uk":0,...};`
2. ``decodeURIComponent(`function%20fn%28a%29%7Bwindow.jsToken%20%3D%20a%7D%3Bfn%28%22<192桁hex>%22%29`)``

2 は JS を実行せずに正規表現で `fn("<hex>")` を拾える。実測で長さ 192、
16 進のみであることを確認済み（`tests/live_anonymous.rs`）。

`ndus` は Cookie。ブラウザのログイン後にしか得られない。

### 3.2 未ログイン時の実応答（実測）

| 要求 | 応答 |
|---|---|
| `GET /api/list?app_id=250528&dir=/` | `{"errno":-6,"request_id":...}` |
| `GET /api/quota?checkexpire=1&checkfree=1` | `{"errmsg":"user not login","errno":-6,...}` |
| `GET /api/shorturlinfo?shorturl=1test` | `{"errno":460030,"errmsg":"dragdrop"}` |
| `GET /api/shorturlinfo?shorturl=1zzzzzzzzz` | `{"errno":105,"show_msg":"share link error stat"}` |
| `GET /share/list?shorturl=1test&root=1` | `{"errno":140,...}` |
| `POST /api/precreate`（引数なし） | `{"errmsg":"param err","errno":2}` |
| `POST /api/create?a=commit` | `{"errmsg":"user not login","errno":-6}` |
| `GET /api/recycle/list` | `{"errmsg":"user not login","errno":-6}` |
| `GET /api/recycle/restore` | `{"errmsg":"user not login","errno":-6}` |
| `POST /share/transfer`（引数なし） | `{"errno":2,"show_msg":""}` |

`errno -6` は未ログイン。共有リンクは入力によって値が変わる:
`1test` は `460030`（`dragdrop`）、`1zzzzzzzzz` は `105`
（`share link error stat`）。どちらもリンク不正として扱う。`140` は
`/share/list` 側で返る値で、期限切れ相当と推定しているが **確定は未実施**
（有効な共有リンクを用意しての検証が要る）。

`/api/precreate` は引数なしでも `errno 2`（param err）を返す。つまり
**エンドポイント自体は存在する**ことが確認できた。`/api/create`,
`/api/recycle/*`, `/share/transfer` も同様に存在を確認済み。

### 3.3 QR ログインは未検証

`com.dubox.drive.login.zxing.QrCodeLoginService` と
`OauthCheckQrcodeResponse` / `QrCodeInfoResponse` の存在は確認したが、
匿名で `/passport/qrcode/...` を叩くと 404 になる。認証済み状態や
適切なパラメータが必要で、**エンドポイントの完全な形は特定できていない**。
本クライアントは QR ログインを実装しない。

### 3.4 本クライアントが採る方式

1. Chrome / Edge を `--remote-debugging-port` + 専用 `--user-data-dir` で起動。
2. 利用者が通常のログインを完了する。
3. CDP の `Network.getAllCookies` で `ndus` を読む。
4. 同じ Cookie で `/main` を取得し `jsToken` / `bdstoken` を得る。
5. Windows 資格情報マネージャへ保存（平文ファイルには書かない）。

専用プロファイルなので利用者の常用ブラウザには触れない。CDP が使えない環境では
`--paste` で `ndus` を手入力する経路に落ちる。

## 4. データ構造（観測）

`com.dubox.drive.cloudfile.io.model.Quota`（JADX 8 フィールド + CREATOR）:

| フィールド | 型 |
|---|---|
| `total` | long |
| `used` | long |
| `free` | long |
| `sboxTotal` | long |
| `sboxUsed` | long |
| `recycleStatus` | int |
| `recmdVip` | String |
| `extra` | `QuotaExtraInfo` |

`QuotaExtraInfo`:

| フィールド | 型 |
|---|---|
| `initQuotaType` | String |
| `timeLimitQuotaExpireTime` | long |

`timeLimitQuotaExpireTime` の存在は、無料 1TB が「期間限定」であるという
公式表記と整合する。本クライアントはこの値を**表示にだけ**使い、
期限切れを勝手に判定しない。

`com.dubox.drive.transfer.io.model.PreCreateDataBean`:

| フィールド | 型 |
|---|---|
| `path` | String |
| `uploadId` | String |
| `returnType` | Integer |
| `blockList` | `List<Integer>` |

`returnType == 2` は「同一内容が既にあるので送信不要」を意味すると解釈している
（アップロードの重複排除）。**推論であり、実測での確認は未実施**。

`LoginResponse`（`com.mars.united.international.passport.domain.model`）:
`ndus, token, userid, displayName, headUrl, curCountry, regCountry, regTime,
isNew, forceReg, skipBind, thridToken, psMemberProduct, psMemberUinfo,
regionDomainPrefix`。`ndus` がログイン応答に直接含まれることを確認した。

## 5. 本クライアントが使わないもの

- `/api/streaming`（署名必須）
- `/share/streaming`（署名必須）
- QR ログイン（エンドポイント未特定）
- アプリ固有の暗号化・署名（`libdubox-security.so`）
- 広告・分析 SDK（そもそも組み込まない）

## 5.1 アップロードの実経路（実測）

初回ログイン後に実アカウントで確認した。

```
1. GET  /rest/2.0/pcs/file?method=locateupload&app_id=250528
        → {"host":"c-jp.terabox.com","server":["c-jp...","c1-jp...","c2-jp..."],"expire":600}
2. POST /api/precreate
        → {"uploadid":"N1-MTM4LjY0LjE5OC4yMjoxNzkzOTc4MjMzOjI3MTU1NzI2NTMzMzI1NDUzMw==","return_type":1}
3. POST https://<host>/rest/2.0/pcs/superfile2?method=upload&type=tmpfile&path=...&uploadid=...&partseq=0&partoffset=0
        → {"md5":"...","partseq":0,"uploadid":"..."}
4. POST /api/create?a=commit&path=...&size=...&uploadid=...&block_list=["<md5>"]
        → {"errno":0,"fs_id":...,"path":"/...","size":...}
```

`uploadid` は Base64 で、中身は `<PCS の IP>:<epoch>:<request_id>`
（例: `138.64.198.22:1793978233:271557265333254533`）。ただし送信先は
この IP ではなく `locateupload` が返すホスト名を使う。

`www.terabox.com` の `/rest/2.0/pcs/superfile2` へ直接 POST すると
openresty が **405 Not Allowed** を返す。ホストを分ける必要がある。

### 上書きの挙動（実測）

`/api/create` は同名パスを上書きしない。同じパスへ 2 回 create すると
`name_20261106_234426.bin` のように日時付きの別名が作られる。
`ondup=overwrite` を付けても同じだった。**先に削除してから create すると
同じパスに置ける**（実測で確認済み）。

### 共有の作成（実測）

`/share/create` と `/api/share/create` の両方に POST したが、どちらも
**HTTP 405 Method Not Allowed**（openresty）だった。`filelist` を
`[{"fs_id":"..."}]` の形で送っても同じ。

したがってクライアントから共有リンクを作る手段は特定できていない。共有内の
dlink も `/share/streaming`（署名必須）経由でしか引けないため、共有は
「一覧表示」と「自分の領域への保存」までとする。

### ゴミ箱（実測）

`fidlist` は数値の配列でなければ通らない。

| 送り方 | 結果 |
|---|---|
| `fidlist=["<id>"]`（文字列配列） | `errno 2` param err |
| `fidlist=[<id>]`（数値配列） | **通る** |
| `fidlist=<id>`（単体） | `errno 2` |
| `fs_id=[<id>]` / `fid=[<id>]` | `errno 2` |

復元先に同名の項目があると `errno -8`（`file already exist`、`faillist` に該当 ID）。
完全削除（`/api/recycle/delete`）は `need verify` 中でも通る。

### 削除・改名

`/api/filemanager?opera=delete` は `filelist=["<path>"]` で通り、
`errno 0` と `info[].path` を返す。

`/api/filemanager?opera=rename` は、`filelist` を配列/オブジェクト、
`newname` を名前/絶対パス、`async` あり/なしの 6 通り試しても
**すべて `errno 2`（param err）** だった。仕様を特定できていないため、
クライアントは読み出し→再アップロード→削除で代替する。

### 書き込み系の一時拒否（実測）

一定量の書き込み後、サーバが次を返すようになった。

| API | errno | 文言 |
|---|---|---|
| `/api/precreate` | 4000023 | `need verify` |
| `/api/filemanager?opera=delete` | 450016 | `need verify` |

読み取り（`/api/list`, `/api/quota`）と `/api/create` のフォルダ作成は
通り続ける。解除条件は未特定。

## 6. 未確定事項

1. 認証済みでの `/api/list` `/api/quota` `/api/user/getinfo` の実応答形状。
   フィールド名は APK 内モデルと既知の Web API 仕様に基づく推定を含む。
2. 単一ファイル上限・同時接続数・解像度制限の実値。サーバが返した場合のみ表示する。
3. `errno 140` の意味（期限切れ推定）。
4. `PreCreateDataBean.returnType == 2` の意味（重複排除推定）。
5. `partseq` の起点（0 始まりと仮定）と推奨パートサイズ（4MiB と仮定）。
   エンドポイントの存在自体は匿名アクセスで確認済み（`/api/precreate` は
   引数なしで `errno 2`、`/api/create` は `errno -6` を返す）。
6. 共有リンク内のファイルを直接ダウンロードする経路（`share_dlink` は
   パスを返すだけで、`pcs/file` で解決できるかは未確認）。

これらは `docs/limitations.md` にも列挙し、`tbx doctor` で検知できる範囲を
明示する。
