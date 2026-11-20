# ヘッドレス Linux サーバー / VM での運用ガイド

GUI（X / Wayland）と D-Bus Secret Service が無い Linux サーバーや VM で、`tbx` を完結して使うための手順です。

## 1. 導入と作業ディレクトリ

配布物のうち、glibc 依存が無い静的ビルド（`tbx-<version>-x86_64-unknown-linux-musl.tar.gz`）または環境に合った Linux ビルドを配置します。

```bash
tar xzf tbx-<version>-x86_64-unknown-linux-musl.tar.gz
./install.sh
tbx --version
```

設定・アカウント索引・秘密ファイル・転送台帳の配置先を 1 ディレクトリへまとめたい場合は、`TBX_HOME`（または `--home`）を指定します。

```bash
export TBX_HOME="$HOME/.local/share/tbx"
```

## 2. 秘密情報の保存先（ストア選択）

`tbx` は `--store`（または環境変数 `TBX_STORE`）で保存先を制御できます。

| 値 | 動作 |
|---|---|
| `auto`（既定） | OS の資格情報ストアを優先し、musl ビルドや D-Bus 不在の環境では `$TBX_HOME/secrets/<account>.json`（`0600`）へ自動フォールバックし、警告と保存先を stderr に出します。 |
| `file` | 最初からファイル保存（ディレクトリ `0700`、ファイル `0600`）を使います。 |
| `keyring` | OS の資格情報ストアを強制します。使えない環境ではフォールバックせず失敗します。 |

```bash
export TBX_STORE=file
```

他人が読める権限（例: `0644`）になっている秘密ファイルは読み込み時に拒否されます。`chmod 600` で権限を絞ってください。

## 3. 非対話ログイン（シェル履歴に `ndus` を残さない方法）

GUI ディスプレイが無い環境では、`tbx login` はブラウザを起動せず非対話手段を案内して終了します。次のいずれかを使います。

### 方法 A: 起動済み Chromium の CDP から取得する

既に起動しているヘッドレス Chromium（またはリモートデバッグ有効の Chromium）へ接続し、`ndus` Cookie を取り込みます。接続先のブラウザは閉じません。

```bash
tbx login --cdp-url http://127.0.0.1:9222
# または環境変数で指定
export TBX_CDP_URL=http://127.0.0.1:9222
tbx login
```

`http://...` 指定時は `/json/list` を優先し、ページターゲットが無い場合は `/json/version`（ブラウザレベル）と `Storage.getCookies` へフォールバックします。`ws://...` を直接渡すこともできます。ループバック（`127.0.0.1` / `localhost` / `::1`）宛ての CDP 通信は `HTTP_PROXY` / `HTTPS_PROXY` を自動でバイパスします。

### 方法 B: 標準入力パイプから取り込む（`--ndus-stdin`）

```bash
# ファイルやシークレットマネージャの出力をそのまま渡す
cat /run/secrets/terabox_ndus | tbx login --paste --ndus-stdin
```

`ndus` の素の値だけでなく、`ndus=<value>` や `Cookie: PANWEB=1; ndus=<value>` 形式も受け付けます。入力値自体は stdout・stderr・ログ・JSON 出力に出ません。

### 方法 C: ファイルまたは環境変数から取り込む

```bash
# 0600 ファイルから読み込み、取り込み後に元ファイルを削除
chmod 600 /tmp/ndus.txt
tbx login --paste --ndus-file /tmp/ndus.txt
rm -f /tmp/ndus.txt
```

環境変数を使う場合は、シェル履歴に残らない形で渡します。

```bash
# 例: サブシェル内でファイルから読み込んで渡す
(export TBX_NDUS="$(cat /run/secrets/terabox_ndus)"; tbx login --paste)
```

任意で `TBX_JS_TOKEN` を設定すると `jsToken` を上書き指定できます（省略時は `/main` から自動取得します）。

保存後は GUI なしで通常コマンドが使えます。

```bash
tbx whoami
tbx quota
tbx ls /
```

## 4. TLS 中継（カスタム CA）とプロキシ

`tbx` は OS のネイティブ CA ストアと埋め込み CA（webpki-roots）の両方を信頼し、さらに `SSL_CERT_FILE` で指定された PEM バンドルをルート証明書へ追加します。

- 出口プロキシや VM ゲートウェイで TLS が中継される環境では、OS の CA ストアへ導入するか `SSL_CERT_FILE` に PEM バンドルを指定します。
  ```bash
  export SSL_CERT_FILE=/path/to/ca-bundle.pem
  ```
- Linux では `SSL_CERT_DIR` も OS ネイティブ CA 読み込み経路から参照されます。
- プロキシは標準の環境変数（`HTTPS_PROXY` / `HTTP_PROXY` / `ALL_PROXY` / `NO_PROXY` と小文字版）に従います。ローカル CDP（`127.0.0.1` 等）はプロキシを通しません。

## 5. 環境診断（`tbx doctor`）

接続や認証が通らない場合は、まず `tbx doctor`（機械可読なら `--json`）を実行します。

```bash
tbx doctor
tbx doctor --cdp-url http://127.0.0.1:9222 --json
```

次の項目を一度に確認できます。

- **表示環境**: `DISPLAY` / `WAYLAND_DISPLAY` の有無（ヘッドレス判定）
- **秘密ストア**: キーリング・D-Bus の状態、`TBX_STORE`、自動フォールバックの有無
- **TLS / CA**: `SSL_CERT_FILE` / `SSL_CERT_DIR` の読取結果・証明書件数と、`base_url` への実 HTTPS 疎通結果
- **プロキシ**: 設定されているプロキシ環境変数（URL 内の認証情報は `***@` でマスク）
- **CDP**: 指定（または既定 `http://127.0.0.1:9222`）の Chromium デバッグポート到達性
- **セッション / API**: 保存済みアカウントと `/api/list`・`/api/quota` の応答
