# 第三者ソフトウェアの表示

本プロジェクト（silentbox / tbx）は MIT ライセンスで配布しています。
Windows の配置では、以下の第三者ソフトウェアを同梱または参照します。

## WinFsp - Windows File System Proxy

- 著作権: Copyright (C) Bill Zissimopoulos
- リポジトリ: https://github.com/winfsp/winfsp
- 配布ページ: https://winfsp.dev/rel/
- ライセンス: GPLv3（ただし FLOSS 向けの特例付き）

### 同梱するもの

Windows のインストーラ `tbx-<version>-setup.exe` には、WinFsp 公式の
インストーラ `winfsp-2.1.25156.msi` を**未改変のまま**同梱しています。
構成画面で「WinFsp も入れる」を選んだときだけ、その MSI を起動します。

- ファイル名: `winfsp-2.1.25156.msi`
- SHA-256: `073A70E00F77423E34BED98B86E600DEF93393BA5822204FAC57A29324DB9F7A`
- 置き場所: このリポジトリの `install/winfsp.msi`

公式リリースから都度ダウンロードする方式は、GitHub が 504/503 を返すと
ビルドとリリースが通らなくなる（実測: 2 回連続で失敗）。そのため未改変の
MSI をリポジトリに置き、CI が SHA-256 を照合してから同梱する。
- サイズ: 2,191,360 バイト
- 署名: NAVIMATICS LLC（有効）

インストーラは起動前に MSI の署名を検証します。署名が無効または欠けている
場合は実行せず、公式の配布ページを案内します。

### ライセンス特例の条件への対応

WinFsp の `License.txt` は、GPLv3 に加えて次の 2 つを許します。

1. プラットフォーム別の WinFsp DLL（`winfsp-x64.dll` など）とのリンク
2. WinFsp 公式インストーラを**未改変のバイナリのまま**配布すること

本プロジェクトは上記 2 に従い、MSI を書き換えずに同梱しています。特例の条件は
次のとおりで、いずれも満たしています。

- 本プロジェクトは MIT ライセンス（FLOSS）で配布している。
- 本ファイルと README に、上記の著作権表示とリポジトリへのリンクを記載している。
- 非 FLOSS の独自ソフトウェアと混在させていない。

### 同梱しない場合

Linux / macOS の成果物には WinFsp を含みません。Windows でも、MSI を同梱せずに
ビルドする場合は、利用者が https://winfsp.dev/rel/ から導入します。

## FUSE（Linux）

Linux の配置では、実行環境の FUSE（`fuser` クレート経由）を利用します。
ビルド時に libfuse を要求しないため、バイナリには同梱していません。
実行には `/dev/fuse` と `fusermount3`（`fuse3` パッケージ）が必要です。
