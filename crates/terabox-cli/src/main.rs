//! `tbx` のコマンドライン定義と実行。
//!
//! 標準出力はデータ、標準エラーは進捗とメッセージ。`--json` を付けた場合は
//! 標準出力に JSON だけを出す（パイプで使えるようにする）。

mod accounts;
mod autostart;
mod cdp;
mod format;
mod login;
mod secret;
mod walk;
mod which;

use std::path::PathBuf;

use anyhow::{anyhow, bail, Context, Result};
use clap::{Parser, Subcommand, ValueEnum};
use terabox_core::api::ApiClient;
use terabox_core::config::Config;
use terabox_core::store::Ledger;
use terabox_core::transfer::{
    DownloadRequest, Downloader, Progress, UploadRequest, Uploader,
};
use terabox_core::{Session, TbxError};

/// 非公式の TeraBox クライアント。
#[derive(Parser, Debug)]
#[command(
    name = "tbx",
    version,
    about = "TeraBox を操作する非公式クライアント（広告なし・並列転送・マウント対応）",
    long_about = None
)]
struct Cli {
    /// 設定ファイルの代わりに使う作業ディレクトリ。
    #[arg(long, global = true, env = "TBX_HOME")]
    home: Option<PathBuf>,

    /// 使うアカウント名。未指定なら索引の既定、それも無ければ `default`。
    #[arg(long, global = true, env = "TBX_ACCOUNT")]
    account: Option<String>,

    /// 機械可読な JSON を標準出力へ出す。
    #[arg(long, global = true)]
    json: bool,

    /// 詳細ログを標準エラーへ出す。
    #[arg(long, global = true)]
    verbose: bool,

    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand, Debug)]
enum Command {
    /// ログインする（ブラウザを開いて Cookie を取得）。
    Login {
        /// Cookie を手で貼り付ける。
        #[arg(long)]
        paste: bool,
        /// デバッグポート。
        #[arg(long, default_value_t = 9333)]
        port: u16,
        /// ブラウザのユーザーデータディレクトリ。
        #[arg(long)]
        profile: Option<PathBuf>,
        /// 登録するアカウント名。省略時は `--account` の解決結果。
        #[arg(long)]
        name: Option<String>,
        /// 秘密の保存先（keyring / file）。省略時は自動。
        #[arg(long)]
        store: Option<String>,
    },
    /// 保存済みセッションを削除する。
    Logout {
        /// 削除するアカウント名。省略時は現在のアカウントだけ。
        name: Option<String>,
        /// 登録済みを全部消す。
        #[arg(long)]
        all: bool,
    },
    /// 登録済みのアカウントを一覧する。
    Accounts,
    /// 既定で使うアカウントを切り替える。
    Use {
        /// 切り替え先の名前。
        name: String,
    },
    /// ログイン済みのアカウントを一覧する。
    Whoami,
    /// リモートの一覧を表示する。
    Ls {
        /// 表示するディレクトリ（`/` 始まり）。
        #[arg(default_value = "/")]
        path: String,
        /// 並び順（time / name / size）。
        #[arg(long, default_value = "name")]
        order: String,
        /// 降順。
        #[arg(long)]
        desc: bool,
        /// 1 ページの件数。
        #[arg(long, default_value_t = 1000)]
        num: u32,
        /// 長い形式（サイズ・更新時刻つき）。
        #[arg(long)]
        long: bool,
    },
    /// 容量・VIP・制限を表示する。
    Quota,
    /// ファイルをダウンロードする。
    Get {
        /// リモートの絶対パス。フォルダを指定すると中身をまとめて取る。
        path: String,
        /// 保存先ディレクトリ（既定は設定の download_dir）。
        #[arg(short, long)]
        output: Option<PathBuf>,
        /// 保存ファイル名の上書き（単一ファイルのときのみ）。
        #[arg(long)]
        name: Option<String>,
        /// 分割接続数。
        #[arg(long)]
        conn: Option<u8>,
        /// 既存ファイルがあっても最初から取り直す。
        #[arg(long)]
        restart: bool,
        /// フォルダ取得時に同時に落とすファイル数。
        #[arg(long, default_value_t = 2)]
        jobs: usize,
    },
    /// ファイルをアップロードする。
    Put {
        /// ローカルファイル。
        local: PathBuf,
        /// 保存先ディレクトリ。
        #[arg(long, default_value = "/")]
        remote: String,
        /// 保存名の上書き。
        #[arg(long)]
        name: Option<String>,
    },
    /// 共有リンクの中身を表示する。
    Share {
        /// `https://www.terabox.com/s/1xxxx` 形式の URL か ID。
        url: String,
        /// 自分の領域へ保存する。
        #[arg(long)]
        save: bool,
        /// 保存先ディレクトリ。
        #[arg(long, default_value = "/")]
        dest: String,
    },
    /// 動画を外部プレーヤで再生する。
    Play {
        /// リモートの絶対パス。
        path: String,
        /// 使用するプレーヤ。
        #[arg(long)]
        player: Option<String>,
        /// URL を表示するだけで起動しない。
        #[arg(long)]
        print_url: bool,
    },
    /// ゴミ箱を操作する。
    Trash {
        #[command(subcommand)]
        action: TrashAction,
    },
    /// 削除（ゴミ箱へ移動）。
    Rm {
        /// 削除するリモートパス。
        paths: Vec<String>,
    },
    /// フォルダを作る。
    Mkdir {
        /// 作成するリモートパス。
        path: String,
    },
    /// ドライブとしてマウントする。
    Mount {
        /// マウント先。Windows はドライブレター（例: `T:`）、
        /// Linux / macOS はディレクトリのパス（例: `~/tbx`）。
        target: String,
        /// バックエンド。
        #[arg(long, value_enum, default_value_t = MountMode::Auto)]
        mode: MountMode,
        /// WebDAV の待受ポート。
        #[arg(long, default_value_t = 8765)]
        port: u16,
        /// 暗号化してマウントする（rclone crypt 互換）。既定は無効。
        #[arg(long)]
        crypt: bool,
    },
    /// 暗号化の設定を確認・初期化する（rclone crypt 互換）。
    Crypt {
        #[command(subcommand)]
        action: CryptAction,
    },
    /// PC 起動時の自動マウントを設定する。
    Autostart {
        #[command(subcommand)]
        action: AutostartAction,
    },
    /// 既存のファイルを暗号化形式へ移す。
    CryptMigrate {
        /// 対象のディレクトリ（`/` 始まり）。
        #[arg(default_value = "/")]
        path: String,
        /// 実際に移動する（省略時は下見だけ）。
        #[arg(long)]
        apply: bool,
        /// 移動後に元を削除する（`--apply` と併用）。
        #[arg(long)]
        delete_source: bool,
    },
    /// 対話画面（TUI）を開く。
    Tui,
    /// 環境と接続を診断する。
    Doctor,
    /// 未完了の転送を表示する。
    Transfers,
}

#[derive(Subcommand, Debug)]
enum AutostartAction {
    /// 現在の状態を表示する。
    Status,
    /// 自動マウントを有効にする。
    Enable {
        /// マウント先（Windows はドライブレター、Unix はディレクトリ）。
        #[arg(long)]
        target: Option<String>,
        /// 暗号化してマウントする（`--crypt` を登録内容へ含める）。
        #[arg(long)]
        crypt: bool,
    },
    /// 自動マウントを無効にする。
    Disable,
}

#[derive(Subcommand, Debug)]
enum CryptAction {
    /// 現在の設定を表示する（秘密は出さない）。
    Status,
    /// 暗号化を初期化する。パスワードと salt を資格情報ストアへ保存する。
    Setup {
        /// パスワードを対話で入力する。
        #[arg(long)]
        interactive: bool,
        /// 平文のパスワード（シェルの履歴に残るため非推奨）。
        #[arg(long)]
        password: Option<String>,
        /// salt（`password2`）。省略時は rclone と同じ既定 salt を使う。
        #[arg(long)]
        salt: Option<String>,
        /// 確認なしで有効化する。
        #[arg(long)]
        yes: bool,
    },
    /// 暗号化を無効にする（保存済みのデータはそのまま残る）。
    Disable {
        #[arg(long)]
        yes: bool,
        /// 保存済みのパスワードも削除する（以後この鍵では復号できなくなる）。
        #[arg(long)]
        purge: bool,
    },
}

#[derive(Subcommand, Debug)]
enum TrashAction {
    /// ゴミ箱の中身を表示する。
    List {
        #[arg(long, default_value_t = 1)]
        page: u32,
        #[arg(long, default_value_t = 100)]
        num: u32,
    },
    /// ゴミ箱から復元する。
    Restore {
        /// `fs_id` を複数指定できる。
        ids: Vec<String>,
    },
    /// ゴミ箱から完全削除する。
    Delete {
        ids: Vec<String>,
    },
    /// ゴミ箱を空にする（取り消し不可）。
    Clear {
        /// 確認を省略する。
        #[arg(long)]
        yes: bool,
    },
}

#[derive(Copy, Clone, Debug, ValueEnum)]
enum MountMode {
    /// その OS で最も自然な方式を選ぶ（Windows は WinFsp、Unix は FUSE）。
    Auto,
    /// WinFsp を使う（Windows のみ）。
    Winfsp,
    /// FUSE を使う（Linux / macOS のみ）。
    Fuse,
    /// WebDAV を使う（全 OS）。
    Webdav,
}

fn main() -> Result<()> {
    let cli = Cli::parse();

    if let Some(home) = &cli.home {
        std::env::set_var("TBX_HOME", home);
    }

    // アカウント名を 1 回だけ解決し、以降は環境変数から読む。下位モジュールが
    // 独自に解釈しないよう、決定はここで閉じる。
    let home = terabox_core::config::tbx_home();
    let account = accounts::resolve(&home, cli.account.as_deref()).map_err(|e| anyhow!(e))?;
    std::env::set_var("TBX_ACCOUNT", &account);

    let filter = if cli.verbose {
        "tbx=debug,terabox_core=debug"
    } else {
        "tbx=warn,terabox_core=warn"
    };
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .without_time()
        .init();

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .context("非同期ランタイムを作れませんでした")?;

    runtime.block_on(run(cli))
}

async fn run(cli: Cli) -> Result<()> {
    let json = cli.json;
    match cli.command {
        Command::Login {
            paste,
            port,
            profile,
            name,
            store,
        } => cmd_login(paste, port, profile, name, store, json).await,
        Command::Logout { name, all } => cmd_logout(name, all, json),
        Command::Accounts => cmd_accounts(json),
        Command::Use { name } => cmd_use(&name, json),
        Command::Whoami => cmd_whoami(json).await,
        Command::Ls {
            path,
            order,
            desc,
            num,
            long,
        } => cmd_ls(&path, &order, desc, num, long, json).await,
        Command::Quota => cmd_quota(json).await,
        Command::Get {
            path,
            output,
            name,
            conn,
            restart,
            jobs,
        } => cmd_get(&path, output, name, conn, restart, jobs, json).await,
        Command::Put { local, remote, name } => cmd_put(local, &remote, name, json).await,
        Command::Share { url, save, dest } => cmd_share(&url, save, &dest, json).await,
        Command::Play {
            path,
            player,
            print_url,
        } => cmd_play(&path, player, print_url, json).await,
        Command::Trash { action } => cmd_trash(action, json).await,
        Command::Rm { paths } => cmd_rm(&paths, json).await,
        Command::Mkdir { path } => cmd_mkdir(&path, json).await,
        Command::Mount {
            target,
            mode,
            port,
            crypt,
        } => cmd_mount(&target, mode, port, crypt).await,
        Command::Crypt { action } => cmd_crypt(action, json).await,
        Command::Autostart { action } => cmd_autostart(action, json).await,
        Command::CryptMigrate {
            path,
            apply,
            delete_source,
        } => cmd_crypt_migrate(&path, apply, delete_source, json).await,
        Command::Tui => cmd_tui().await,
        Command::Doctor => cmd_doctor(json).await,
        Command::Transfers => cmd_transfers(json),
    }
}

fn config() -> Result<Config> {
    Config::load().map_err(|e| anyhow!(e))
}

/// 現在のアカウント名（`main` が解決済み）。
fn account_name() -> String {
    terabox_core::config::account_name()
}

fn load_session() -> Result<Session> {
    let home = terabox_core::config::tbx_home();
    let account = account_name();
    // 旧版の 1 件だけのセッションを、初回だけ `default` へ移す。
    migrate_legacy_once(&home, &account)?;
    let kind = secret::detect_kind(&home, &account);
    match secret::load(&home, &account, kind) {
        Ok(Some(s)) => {
            // 索引に載っていなければ載せる。移行の途中で落ちた場合や、
            // 索引だけ消えた場合に `accounts` が空のままになるのを防ぐ。
            ensure_indexed(&home, &account, s.uk.clone())?;
            Ok(s)
        }
        Ok(None) => bail!(
            "アカウント {account} の保存済みセッションがありません。`tbx login --name {account}` を実行してください。"
        ),
        Err(e) => bail!("{e}"),
    }
}

/// 索引に載っていなければ載せる（既にあれば触らない）。
fn ensure_indexed(
    home: &std::path::Path,
    account: &str,
    uk: Option<String>,
) -> Result<()> {
    let mut index = accounts::load(home).map_err(|e| anyhow!(e))?;
    if index.find(account).is_some() {
        return Ok(());
    }
    index.upsert(account, uk, chrono::Utc::now().timestamp());
    if index.current.is_empty() {
        index.current = account.to_string();
    }
    accounts::save(home, &index).map_err(|e| anyhow!(e))
}

/// 旧版の `session` エントリを `default` へ移す。
///
/// 新しいスロットへの保存が成功した後にだけ旧エントリを消す。失敗したら旧を
/// 残す（移行の途中でセッションを失わない）。2 回目以降は旧が無いので何もしない。
fn migrate_legacy_once(home: &std::path::Path, account: &str) -> Result<()> {
    if !account_matches_legacy(account) {
        return Ok(());
    }
    if !secret::has_legacy() {
        return Ok(());
    }
    // 既に新しいスロットがあるなら移行済み。旧だけ消す。
    if secret::load(home, account, secret::detect_kind(home, account)).ok().flatten().is_some() {
        let _ = secret::delete_legacy();
        return Ok(());
    }
    let legacy = match secret::load_legacy() {
        Ok(Some(s)) => s,
        Ok(None) => return Ok(()),
        Err(_) => return Ok(()),
    };
    match secret::save(home, account, secret::StoreKind::Keyring, &legacy) {
        Ok(()) => {
            let _ = secret::delete_legacy();
            // 索引にも載せる。載せないと `tbx accounts` に出てこない。
            let mut index = accounts::load(home).map_err(|e| anyhow!(e))?;
            index.upsert(account, legacy.uk.clone(), chrono::Utc::now().timestamp());
            if index.current.is_empty() {
                index.current = account.to_string();
            }
            accounts::save(home, &index).map_err(|e| anyhow!(e))?;
            eprintln!("旧形式のセッションをアカウント {account} として引き継ぎました。");
        }
        Err(e) => {
            eprintln!("旧形式のセッションを引き継げませんでした（旧はそのまま残します）: {e}");
        }
    }
    Ok(())
}

/// 移行の対象になるのは既定名のときだけ。他の名前へ勝手に配らない。
fn account_matches_legacy(account: &str) -> bool {
    account == accounts::DEFAULT_NAME
}

async fn api() -> Result<ApiClient> {
    let session = load_session()?;
    let cfg = config()?;
    ApiClient::with_base(session, cfg.base_url).map_err(|e| anyhow!(e))
}

fn map_session_error(e: TbxError) -> anyhow::Error {
    match e {
        TbxError::Api(ref err) if err.is_not_logged_in() => {
            anyhow!("セッションが失効しています。`tbx login` で再ログインしてください。")
        }
        other => anyhow!(other),
    }
}

// ---- 各コマンド ----

async fn cmd_login(
    paste: bool,
    port: u16,
    profile: Option<PathBuf>,
    name: Option<String>,
    store: Option<String>,
    json: bool,
) -> Result<()> {
    let home = terabox_core::config::tbx_home();
    let target = match name {
        Some(n) => {
            accounts::validate_name(&n).map_err(|e| anyhow!(e))?;
            n
        }
        None => account_name(),
    };
    let kind = match store {
        Some(s) => secret::StoreKind::parse(&s).map_err(|e| anyhow!(e))?,
        None => secret::StoreKind::Keyring,
    };

    let mut from_browser = None;
    if !paste {
        let profile = profile.unwrap_or_else(|| default_profile_dir(&target));
        match login::login_with_browser(profile, port).await {
            login::BrowserLogin::Success(session) => from_browser = Some(session),
            login::BrowserLogin::NotLoggedIn => {
                bail!("ログインを確認できませんでした。もう一度 `tbx login` を実行してください。")
            }
            login::BrowserLogin::Unavailable(reason) => {
                eprintln!("{reason}");
            }
        }
    }

    let session = match from_browser {
        Some(s) => s,
        None => {
            if paste {
                paste_login_interactive().await?
            } else {
                bail!(
                    "ブラウザでのログインを開始できませんでした。`tbx login --paste` で Cookie を貼り付けてください。"
                )
            }
        }
    };

    if let Err(e) = secret::save(&home, &target, kind, &session) {
        eprintln!("警告: {e}");
        eprintln!("このセッションは保存されていません。次の実行では再ログインが必要です。");
        bail!("アカウント {target} を保存できませんでした");
    }

    // 索引を更新する。保存に成功した後だけ載せる（載せたのに読めない状態を作らない）。
    let mut index = accounts::load(&home).map_err(|e| anyhow!(e))?;
    let was_new = index.upsert(&target, session.uk.clone(), chrono::Utc::now().timestamp());
    if index.current.is_empty() {
        index.current = target.clone();
    }
    accounts::save(&home, &index).map_err(|e| anyhow!(e))?;

    if json {
        println!(
            "{}",
            serde_json::json!({
                "logged_in": true,
                "name": target,
                "server_account": session.account_label(),
                "can_write": session.can_write(),
                "store": kind.label(),
                "new": was_new,
            })
        );
    } else {
        if was_new {
            println!("アカウント {target} を追加しました（{}）", session.account_label());
        } else {
            println!(
                "アカウント {target} のセッションを更新しました（{}）",
                session.account_label()
            );
        }
        println!("保存先: {}", kind.label());
        if !session.can_write() {
            println!(
                "注意: 書き込み用トークン（bdstoken）が取得できていません。アップロードや削除は失敗します。"
            );
        }
    }
    Ok(())
}

async fn paste_login_interactive() -> Result<Session> {
    println!(
        "ブラウザで https://www.terabox.com/main にログインし、開発者ツールで Cookie を確認してください。"
    );
    println!("  1. F12 を押して「アプリケーション」→「Cookie」→ https://www.terabox.com");
    println!("  2. 名前が `ndus` の値をコピー");
    println!();
    let ndus = rpassword::prompt_password("ndus を貼り付けて Enter（画面には表示されません）: ")
        .context("入力を受け取れませんでした")?;
    if ndus.trim().is_empty() {
        bail!("ndus が空です");
    }
    let js_token = rpassword::prompt_password(
        "jsToken を貼り付けて Enter（空でも可・その場合は自動取得します）: ",
    )
    .unwrap_or_default();
    let token = if js_token.trim().is_empty() {
        None
    } else {
        Some(js_token)
    };
    login::login_with_paste(ndus, token)
        .await
        .map_err(|e| anyhow!("{e}"))
}

fn default_profile_dir(account: &str) -> PathBuf {
    terabox_core::config::tbx_home()
        .join("browser-profile")
        .join(account)
}

fn cmd_logout(name: Option<String>, all: bool, json: bool) -> Result<()> {
    let home = terabox_core::config::tbx_home();
    let mut index = accounts::load(&home).map_err(|e| anyhow!(e))?;

    if all {
        let names: Vec<String> = index.accounts.iter().map(|a| a.name.clone()).collect();
        if names.is_empty() {
            if json {
                println!("{}", serde_json::json!({"logged_out": false, "reason": "no account"}));
            } else {
                println!("登録済みのアカウントはありません。");
            }
            return Ok(());
        }
        let mut removed = Vec::new();
        for entry_name in &names {
            let kind = secret::detect_kind(&home, entry_name);
            match secret::delete(&home, entry_name, kind) {
                Ok(true) => removed.push(entry_name.clone()),
                Ok(false) => {}
                Err(e) => eprintln!("アカウント {entry_name} を消せませんでした: {e}"),
            }
            index.remove(entry_name);
        }
        index.current.clear();
        accounts::save(&home, &index).map_err(|e| anyhow!(e))?;
        if json {
            println!("{}", serde_json::json!({"logged_out": true, "removed": removed}));
        } else {
            println!("{} 件のアカウントを削除しました。", removed.len());
        }
        return Ok(());
    }

    let target = match name {
        Some(n) => {
            accounts::validate_name(&n).map_err(|e| anyhow!(e))?;
            n
        }
        None => account_name(),
    };
    let kind = secret::detect_kind(&home, &target);
    let deleted = secret::delete(&home, &target, kind).map_err(|e| anyhow!(e))?;
    let removed_from_index = index.remove(&target);
    accounts::save(&home, &index).map_err(|e| anyhow!(e))?;

    if json {
        println!(
            "{}",
            serde_json::json!({
                "logged_out": deleted || removed_from_index,
                "name": target,
                "current": index.current,
            })
        );
    } else if deleted || removed_from_index {
        println!("アカウント {target} を削除しました。");
        if !index.current.is_empty() {
            println!("既定は {} に移りました。", index.current);
        }
    } else {
        println!("アカウント {target} は登録されていませんでした。");
    }
    Ok(())
}

fn cmd_accounts(json: bool) -> Result<()> {
    let home = terabox_core::config::tbx_home();
    let index = accounts::load(&home).map_err(|e| anyhow!(e))?;

    if json {
        println!("{}", serde_json::to_string_pretty(&index)?);
        return Ok(());
    }

    if index.accounts.is_empty() {
        println!("登録済みのアカウントはありません。`tbx login --name <名前>` で追加してください。");
        return Ok(());
    }
    let active = account_name();
    println!(
        "{:<4} {:<16} {:<16} {}",
        "", "名前", "uk", "登録時刻"
    );
    for entry in &index.accounts {
        let marks = [
            if entry.name == index.current { "*" } else { " " },
            if entry.name == active { ">" } else { " " },
        ]
        .concat();
        let when = chrono::DateTime::from_timestamp(entry.added_at, 0)
            .map(|d| d.with_timezone(&chrono::Local).format("%Y-%m-%d %H:%M").to_string())
            .unwrap_or_else(|| "-".to_string());
        println!(
            "{:<4} {:<16} {:<16} {}",
            marks,
            entry.name,
            entry.uk.as_deref().unwrap_or("-"),
            when
        );
    }
    println!();
    println!("* = 既定 / > = このコマンドで使用中");
    Ok(())
}

fn cmd_use(name: &str, json: bool) -> Result<()> {
    let home = terabox_core::config::tbx_home();
    let mut index = accounts::load(&home).map_err(|e| anyhow!(e))?;
    index.set_current(name).map_err(|e| anyhow!(e))?;
    accounts::save(&home, &index).map_err(|e| anyhow!(e))?;
    if json {
        println!("{}", serde_json::json!({"current": index.current}));
    } else {
        println!("既定のアカウントを {name} にしました。");
    }
    Ok(())
}

async fn cmd_whoami(json: bool) -> Result<()> {
    let client = api().await?;
    let session = client.session().await;
    let quota = client.quota().await.map_err(map_session_error)?;

    if json {
        println!(
            "{}",
            serde_json::json!({
                "account": session.account_label(),
                "can_write": session.can_write(),
                "quota": quota,
            })
        );
    } else {
        println!("アカウント: {}", session.account_label());
        println!(
            "書き込み権限: {}",
            if session.can_write() { "あり" } else { "なし" }
        );
        println!();
        print!("{}", format::quota_block(&quota));
    }
    Ok(())
}

async fn cmd_ls(
    path: &str,
    order: &str,
    desc: bool,
    num: u32,
    long: bool,
    json: bool,
) -> Result<()> {
    let client = api().await?;
    let normalized = terabox_core::vfs::normalize_path(path);
    let resp = client
        .list(&normalized, 1, num, order, desc)
        .await
        .map_err(map_session_error)?;

    if json {
        println!("{}", serde_json::to_string_pretty(&resp.list)?);
        return Ok(());
    }

    if resp.list.is_empty() {
        println!("{normalized} は空です。");
        return Ok(());
    }
    for entry in &resp.list {
        if long {
            println!("{}", format::entry_line(entry));
        } else {
            println!("{}", entry.display_name());
        }
    }
    println!();
    println!("{} 件", resp.list.len());
    Ok(())
}

async fn cmd_quota(json: bool) -> Result<()> {
    let client = api().await?;
    let quota = client.quota().await.map_err(map_session_error)?;
    if json {
        println!("{}", serde_json::to_string_pretty(&quota)?);
    } else {
        print!("{}", format::quota_block(&quota));
    }
    Ok(())
}

async fn cmd_get(
    path: &str,
    output: Option<PathBuf>,
    name: Option<String>,
    conn: Option<u8>,
    restart: bool,
    jobs: usize,
    json: bool,
) -> Result<()> {
    let client = api().await?;
    let cfg = config()?;
    let normalized = terabox_core::vfs::normalize_path(path);

    // 指定がフォルダなら一括取得へ回す。判定は「親の一覧に載っているか」で行う。
    let target_name = name
        .clone()
        .unwrap_or_else(|| terabox_core::vfs::base_name(&normalized));
    let parent = terabox_core::vfs::parent_path(&normalized);
    let entries = client
        .list(&parent, 1, 1000, "name", false)
        .await
        .map_err(map_session_error)?;
    let found = entries
        .list
        .iter()
        .find(|e| e.display_name() == target_name)
        .cloned();

    if let Some(entry) = &found {
        if entry.isdir {
            if name.is_some() {
                bail!("フォルダに `--name` は使えません。`--output` で保存先を指定してください。");
            }
            let dest_root = output.unwrap_or_else(|| {
                cfg.download_dir
                    .join(terabox_core::vfs::base_name(&normalized))
            });
            return download_folder(
                &client,
                &normalized,
                &dest_root,
                conn,
                restart,
                jobs.max(1),
                json,
            )
            .await;
        }
    }

    let file_name = if target_name.is_empty() {
        bail!("ダウンロードするファイル名を決められません。`--name` で指定してください。");
    } else {
        target_name
    };

    let meta = found;

    let dest_dir = output.unwrap_or(cfg.download_dir.clone());
    std::fs::create_dir_all(&dest_dir)?;
    let local_path = dest_dir.join(&file_name);

    if restart && local_path.exists() {
        std::fs::remove_file(&local_path)
            .with_context(|| format!("既存ファイル {} を消せませんでした", local_path.display()))?;
    }

    // 単一ファイル上限の事前判定。上限が取れている場合だけ止める。
    if let Ok(q) = client.quota().await {
        if let (Some(limit), Some(entry)) = (q.max_file_size_bytes, &meta) {
            if entry.size > limit {
                bail!(
                    "このファイルは {} で、アカウントの単一ファイル上限 {} を超えています。転送を開始しません。",
                    format::bytes(entry.size),
                    format::bytes(limit)
                );
            }
        }
    }

    let dlink = client.dlink(&normalized).await.map_err(map_session_error)?;
    let session = client.session().await;
    let downloader = Downloader::new(&session).map_err(|e| anyhow!(e))?;
    let mut ledger = Ledger::open(&terabox_core::config::ledger_path()).map_err(|e| anyhow!(e))?;

    let req = DownloadRequest {
        remote_path: normalized.clone(),
        fs_id: meta.as_ref().map(|e| e.fs_id.clone()).unwrap_or_default(),
        local_path: local_path.clone(),
        expected_size: meta.as_ref().map(|e| e.size),
        connections: conn.unwrap_or(cfg.connections),
        part_size: cfg.part_size_bytes(),
    };

    let outcome = downloader
        .download(&dlink, &req, &mut ledger, |p| match p {
            Progress::Note(text) => eprintln!("{text}"),
            other => eprint!("\r{}", format::progress_line(&other)),
        })
        .await;
    eprintln!();

    let done = outcome.map_err(|e| anyhow!(e))?;
    if json {
        println!(
            "{}",
            serde_json::json!({
                "local_path": done.local_path.display().to_string(),
                "bytes": done.bytes,
                "resumed_from": done.resumed_from,
            })
        );
    } else {
        println!(
            "保存しました: {}（{}）",
            done.local_path.display(),
            format::bytes(done.bytes)
        );
        if done.resumed_from > 0 {
            println!(
                "前回の続きから {} を再利用しました。",
                format::bytes(done.resumed_from)
            );
        }
    }
    Ok(())
}

/// フォルダを再帰的に辿り、配下のファイルをすべて落とす。
async fn download_folder(
    client: &ApiClient,
    root: &str,
    dest_root: &std::path::Path,
    conn: Option<u8>,
    restart: bool,
    jobs: usize,
    json: bool,
) -> Result<()> {
    let cfg = config()?;
    let mut plan = walk::Plan::default();

    // 幅優先で辿る。1 ディレクトリ 1000 件ずつページを送る。
    let mut queue: Vec<Vec<String>> = vec![Vec::new()];
    while let Some(current) = queue.pop() {
        let remote = if current.is_empty() {
            root.to_string()
        } else {
            format!("{}/{}", root.trim_end_matches('/'), current.join("/"))
        };
        let mut page = 1;
        loop {
            let resp = client
                .list(&remote, page, 1000, "name", false)
                .await
                .map_err(map_session_error)?;
            if resp.list.is_empty() {
                break;
            }
            walk::extend(&mut plan, &current, &resp.list);
            let has_more = resp.has_more.unwrap_or(0) != 0;
            if !has_more {
                break;
            }
            page += 1;
        }
        // 子ディレクトリを積む（深さ優先で回るが、並べ替えで順序を直す）。
        for dir in &plan.dirs {
            if dir.relative_dirs.len() == current.len() + 1
                && dir.relative_dirs[..current.len()] == current[..]
            {
                queue.push(dir.relative_dirs.clone());
            }
        }
    }
    walk::sort(&mut plan);

    if plan.files.is_empty() && plan.dirs.is_empty() {
        println!("{root} は空です。");
        return Ok(());
    }

    for dir in &plan.dirs {
        let mut path = dest_root.to_path_buf();
        for part in &dir.relative_dirs {
            path.push(part);
        }
        std::fs::create_dir_all(&path)
            .with_context(|| format!("{} を作れませんでした", path.display()))?;
    }

    let total = plan.files.len();
    println!(
        "{} 件のファイルを {} へ取得します（同時 {} 件）。",
        total,
        dest_root.display(),
        jobs
    );

    let session = client.session().await;
    let semaphore = std::sync::Arc::new(tokio::sync::Semaphore::new(jobs));
    let mut tasks = tokio::task::JoinSet::new();
    let mut failed: Vec<String> = Vec::new();
    let mut saved: Vec<(PathBuf, u64)> = Vec::new();

    for file in &plan.files {
        let permit = semaphore.clone().acquire_owned().await?;
        let client = client.clone();
        let session = session.clone();
        let cfg_conn = conn.unwrap_or(cfg.connections);
        let part_size = cfg.part_size_bytes();
        let dest = {
            let mut p = dest_root.to_path_buf();
            for part in &file.relative_dirs {
                p.push(part);
            }
            p
        };
        let file = file.clone();
        let restart = restart;
        tasks.spawn(async move {
            let _permit = permit;
            if let Err(e) = std::fs::create_dir_all(&dest) {
                return Err(format!("{} を作れませんでした: {e}", dest.display()));
            }
            let local = dest.join(&file.file_name);
            if restart && local.exists() {
                let _ = std::fs::remove_file(&local);
            }
            let dlink = client
                .dlink(&file.remote_path)
                .await
                .map_err(|e| format!("{}: {e}", file.remote_path))?;
            let downloader =
                Downloader::new(&session).map_err(|e| format!("転送を初期化できません: {e}"))?;
            let mut ledger = Ledger::open(&terabox_core::config::ledger_path())
                .map_err(|e| format!("台帳を開けません: {e}"))?;
            let req = DownloadRequest {
                remote_path: file.remote_path.clone(),
                fs_id: file.fs_id.clone(),
                local_path: local.clone(),
                expected_size: Some(file.size),
                connections: cfg_conn,
                part_size,
            };
            let done = downloader
                .download(&dlink, &req, &mut ledger, |_| {})
                .await
                .map_err(|e| format!("{}: {e}", file.remote_path))?;
            Ok((done.local_path, done.bytes))
        });
        while let Some(joined) = tasks.try_join_next() {
            match joined {
                Ok(Ok(pair)) => {
                    eprintln!("  取得: {}", pair.0.display());
                    saved.push(pair);
                }
                Ok(Err(msg)) => failed.push(msg),
                Err(e) => failed.push(format!("転送タスクが落ちました: {e}")),
            }
        }
    }
    while let Some(joined) = tasks.join_next().await {
        match joined {
            Ok(Ok(pair)) => saved.push(pair),
            Ok(Err(msg)) => failed.push(msg),
            Err(e) => failed.push(format!("転送タスクが落ちました: {e}")),
        }
    }

    if json {
        println!(
            "{}",
            serde_json::json!({
                "root": root,
                "dest": dest_root.display().to_string(),
                "files": total,
                "saved": saved.len(),
                "failed": failed,
            })
        );
    } else {
        let bytes: u64 = saved.iter().map(|(_, b)| *b).sum();
        println!(
            "{} 件中 {} 件を取得しました（{}）。",
            total,
            saved.len(),
            format::bytes(bytes)
        );
        if !failed.is_empty() {
            println!("失敗 {} 件:", failed.len());
            for msg in &failed {
                println!("  {msg}");
            }
        }
    }
    if !failed.is_empty() {
        bail!("{} 件の取得に失敗しました", failed.len());
    }
    Ok(())
}

async fn cmd_put(local: PathBuf, remote: &str, name: Option<String>, json: bool) -> Result<()> {
    if !local.exists() {
        bail!("{} が見つかりません。", local.display());
    }
    let client = api().await?;
    let mut uploader = Uploader::new(client);
    let mut ledger = Ledger::open(&terabox_core::config::ledger_path()).map_err(|e| anyhow!(e))?;

    let req = UploadRequest {
        local_path: local.clone(),
        remote_dir: terabox_core::vfs::normalize_path(remote),
        remote_name: name,
        part_size: 4 * 1024 * 1024,
    };

    let outcome = uploader
        .upload(&req, &mut ledger, |p| match p {
            Progress::Note(text) => eprintln!("{text}"),
            other => eprint!("\r{}", format::progress_line(&other)),
        })
        .await;
    eprintln!();

    let done = outcome.map_err(|e| anyhow!(e))?;
    if json {
        println!(
            "{}",
            serde_json::json!({
                "remote_path": done.remote_path,
                "bytes": done.bytes,
                "parts_uploaded": done.parts_uploaded,
                "deduplicated": done.deduplicated,
            })
        );
    } else {
        println!("アップロードしました: {}", done.remote_path);
    }
    Ok(())
}

async fn cmd_share(url: &str, save: bool, dest: &str, json: bool) -> Result<()> {
    let client = api().await?;
    let info = client.shorturl_info(url).await.map_err(map_session_error)?;

    let entries = if info.entries.is_empty() {
        client
            .share_list(url, "/", 1, 1000)
            .await
            .map_err(map_session_error)?
    } else {
        info.entries.clone()
    };

    if save {
        let ids: Vec<String> = entries.iter().map(|e| e.fs_id.clone()).collect();
        let dest = terabox_core::vfs::normalize_path(dest);
        client
            .share_transfer(url, &ids, &dest)
            .await
            .map_err(map_session_error)?;
        if json {
            println!(
                "{}",
                serde_json::json!({"saved": true, "dest": dest, "count": ids.len()})
            );
        } else {
            println!("{dest} へ保存しました（{} 件）", ids.len());
        }
        return Ok(());
    }

    if json {
        println!("{}", serde_json::to_string_pretty(&entries)?);
    } else {
        if entries.is_empty() {
            println!("共有リンクの中身を取得できませんでした（サーバが一覧を返していません）。");
        }
        for e in &entries {
            println!("{}", format::entry_line(e));
        }
        println!();
        println!("{} 件", entries.len());
    }
    Ok(())
}

async fn cmd_play(path: &str, player: Option<String>, print_url: bool, json: bool) -> Result<()> {
    let client = api().await?;
    let normalized = terabox_core::vfs::normalize_path(path);
    let dlink = client.dlink(&normalized).await.map_err(map_session_error)?;

    if print_url || json {
        if json {
            println!(
                "{}",
                serde_json::json!({"path": normalized, "dlink": dlink})
            );
        } else {
            println!("{dlink}");
        }
        return Ok(());
    }

    let cfg = config()?;
    let chosen = match player {
        Some(p) => p,
        None => pick_player(&cfg).ok_or_else(|| {
            anyhow!(
                "再生できるプレーヤが見つかりませんでした（mpv / VLC / ffplay のいずれか）。`--player` で指定してください。"
            )
        })?,
    };

    std::process::Command::new(&chosen)
        .arg(&dlink)
        .spawn()
        .with_context(|| format!("{chosen} を起動できませんでした"))?;
    println!("{chosen} で再生を開始しました。");
    Ok(())
}

/// 利用可能なプレーヤを設定の優先順で探す。
fn pick_player(cfg: &Config) -> Option<String> {
    for name in &cfg.players {
        if let Some(path) = which::which(name) {
            return Some(path.display().to_string());
        }
        if name.eq_ignore_ascii_case("vlc") {
            if let Ok(pf) = std::env::var("PROGRAMFILES") {
                let p = PathBuf::from(pf).join(r"VideoLAN\VLC\vlc.exe");
                if p.exists() {
                    return Some(p.display().to_string());
                }
            }
        }
        if name.eq_ignore_ascii_case("mpv") {
            if let Ok(pf) = std::env::var("PROGRAMFILES") {
                let p = PathBuf::from(pf).join(r"mpv\mpv.exe");
                if p.exists() {
                    return Some(p.display().to_string());
                }
            }
        }
    }
    None
}

async fn cmd_trash(action: TrashAction, json: bool) -> Result<()> {
    let client = api().await?;
    match action {
        TrashAction::List { page, num } => {
            let list = client
                .recycle_list(page, num)
                .await
                .map_err(map_session_error)?;
            if json {
                println!("{}", serde_json::to_string_pretty(&list)?);
            } else {
                if list.is_empty() {
                    println!("ゴミ箱は空です。");
                }
                for e in &list {
                    println!("{}", format::entry_line(e));
                }
                println!();
                println!("{} 件", list.len());
            }
            Ok(())
        }
        TrashAction::Restore { ids } => {
            if ids.is_empty() {
                bail!("復元する fs_id を指定してください。");
            }
            client
                .recycle_restore(&ids)
                .await
                .map_err(map_session_error)?;
            println!("{} 件を復元しました。", ids.len());
            Ok(())
        }
        TrashAction::Delete { ids } => {
            if ids.is_empty() {
                bail!("削除する fs_id を指定してください。");
            }
            client
                .recycle_delete(&ids)
                .await
                .map_err(map_session_error)?;
            println!("{} 件を完全削除しました（取り消せません）。", ids.len());
            Ok(())
        }
        TrashAction::Clear { yes } => {
            if !yes {
                println!("ゴミ箱を空にすると取り消せません。実行するには `--yes` を付けてください。");
                return Ok(());
            }
            client.recycle_clear().await.map_err(map_session_error)?;
            println!("ゴミ箱を空にしました。");
            Ok(())
        }
    }
}

async fn cmd_rm(paths: &[String], json: bool) -> Result<()> {
    if paths.is_empty() {
        bail!("削除するパスを指定してください。");
    }
    let client = api().await?;
    let normalized: Vec<String> = paths
        .iter()
        .map(|p| terabox_core::vfs::normalize_path(p))
        .collect();
    client
        .delete(&normalized)
        .await
        .map_err(map_session_error)?;
    if json {
        println!("{}", serde_json::json!({ "deleted": normalized }));
    } else {
        println!("{} 件をゴミ箱へ移動しました。", normalized.len());
    }
    Ok(())
}

async fn cmd_mkdir(path: &str, json: bool) -> Result<()> {
    let client = api().await?;
    let normalized = terabox_core::vfs::normalize_path(path);
    client
        .create_dir(&normalized)
        .await
        .map_err(map_session_error)?;
    if json {
        println!("{}", serde_json::json!({ "created": normalized }));
    } else {
        println!("作成しました: {normalized}");
    }
    Ok(())
}

/// 設定からマウントの調整値を組み立てる。
///
/// 設定が読めない場合でも既定値で動かす（マウントを止める理由にはしない）。
fn mount_tuning() -> terabox_mount::MountTuning {
    match Config::load() {
        Ok(cfg) => terabox_mount::MountTuning {
            write_buffer: (cfg.write_buffer_mib as usize).saturating_mul(1024 * 1024),
            threads: usize::from(cfg.mount_threads).clamp(1, 32),
            part_size: cfg.part_size_mib.saturating_mul(1024 * 1024).max(1024 * 1024),
        },
        Err(e) => {
            eprintln!("設定を読めないため既定値でマウントします: {e}");
            terabox_mount::MountTuning::default()
        }
    }
}

/// 暗号化が有効か（自動起動の登録内容を決めるのに使う）。
///
/// `crypt` feature が無いビルドでは常に false。設定が読めない場合も false
/// （読めないことを暗号化の指示と解釈しない）。
fn crypt_enabled_for_autostart() -> bool {
    #[cfg(feature = "crypt")]
    {
        return terabox_core::crypt::CryptConfig::load()
            .map(|c| c.enabled)
            .unwrap_or(false);
    }
    #[cfg(not(feature = "crypt"))]
    {
        false
    }
}

async fn cmd_mount(target: &str, mode: MountMode, port: u16, crypt: bool) -> Result<()> {
    let client = api().await?;
    let session = client.session().await;
    let tuning = mount_tuning();

    #[cfg(feature = "crypt")]
    {
        // `--crypt` か設定のいずれかが有効なら暗号化でマウントする。
        if crypt || crypt_config_enabled()? {
            return mount_encrypted(session, target, mode, port, tuning).await;
        }
    }
    #[cfg(not(feature = "crypt"))]
    if crypt {
        bail!(
            "このビルドは暗号化を含んでいません。`--features crypt` を付けてビルドしてください。"
        );
    }
    #[cfg(feature = "crypt")]
    if !crypt {
        // 設定で有効になっている場合も `mount_encrypted` へ回るので、ここは
        // 「crypt 引数も設定も無効」のときだけ通る。
    }

    match mode {
        MountMode::Auto => {
            // OS ごとに最も自然な方式を選ぶ。Windows は WinFsp、Unix は FUSE。
            if cfg!(windows) {
                if terabox_mount::winfsp_available() {
                    return terabox_mount::mount_winfsp_tuned(session, target, tuning).await;
                }
                eprintln!("WinFsp が見つからないため WebDAV モードで起動します。");
                eprintln!("（WinFsp を入れるとエクスプローラの仮想ファイルとして扱えます）");
                terabox_mount::mount_webdav(session, target, port).await
            } else {
                if terabox_mount::fuse_available() {
                    return terabox_mount::mount_fuse_tuned(session, target, tuning).await;
                }
                eprintln!("FUSE が使えないため WebDAV モードで起動します。");
                eprintln!("（--features fuse-backend でビルドし、/dev/fuse を用意してください）");
                terabox_mount::mount_webdav(session, target, port).await
            }
        }
        MountMode::Winfsp => terabox_mount::mount_winfsp_tuned(session, target, tuning).await,
        MountMode::Fuse => terabox_mount::mount_fuse_tuned(session, target, tuning).await,
        MountMode::Webdav => terabox_mount::mount_webdav(session, target, port).await,
    }
}

#[cfg(feature = "crypt")]
/// 設定で暗号化が有効か（`crypt` feature が無いビルドでは常に false）。
fn crypt_config_enabled() -> Result<bool> {
    #[cfg(feature = "crypt")]
    {
        Ok(terabox_core::crypt::CryptConfig::load()
            .map_err(|e| anyhow!(e))?
            .enabled)
    }
    #[cfg(not(feature = "crypt"))]
    {
        Ok(false)
    }
}

/// 自動マウントの設定。
async fn cmd_autostart(action: AutostartAction, json: bool) -> Result<()> {
    match action {
        AutostartAction::Status => {
            let state = autostart::status();
            if json {
                let (enabled, command) = match &state {
                    autostart::State::Enabled { command } => (true, Some(command.clone())),
                    autostart::State::Disabled => (false, None),
                    autostart::State::Unknown(reason) => {
                        println!(
                            "{}",
                            serde_json::json!({"enabled": null, "reason": reason})
                        );
                        return Ok(());
                    }
                };
                println!(
                    "{}",
                    serde_json::json!({"enabled": enabled, "command": command})
                );
            } else {
                match state {
                    autostart::State::Enabled { command } => {
                        println!("自動マウント: 有効");
                        println!("登録内容: {command}");
                    }
                    autostart::State::Disabled => {
                        println!("自動マウント: 無効（既定）");
                        println!("有効にするには `tbx autostart enable` を実行してください。");
                    }
                    autostart::State::Unknown(reason) => {
                        println!("自動マウント: 確認できません（{reason}）");
                    }
                }
            }
            Ok(())
        }
        AutostartAction::Enable { target, crypt } => {
            let target = target.unwrap_or_else(autostart::default_target);
            // 暗号化が有効な環境では、指定が無くても暗号化で登録する。
            // 平文で生えると利用者の意図と違う。
            let use_crypt = crypt || crypt_enabled_for_autostart();
            let what = if use_crypt {
                format!("`tbx mount {target} --crypt`")
            } else {
                format!("`tbx mount {target}`")
            };
            eprintln!("次の内容で登録します: {what}");
            eprintln!("（PC 起動時に自動でマウントします。ログイン済みである必要があります）");
            let command = autostart::enable(&target, use_crypt).map_err(|e| anyhow!(e))?;
            if json {
                println!(
                    "{}",
                    serde_json::json!({"enabled": true, "target": target, "command": command})
                );
            } else {
                println!("自動マウントを有効にしました。");
                println!("登録内容: {command}");
            }
            Ok(())
        }
        AutostartAction::Disable => {
            let removed = autostart::disable().map_err(|e| anyhow!(e))?;
            if json {
                println!("{}", serde_json::json!({"enabled": false, "removed": removed}));
            } else if removed {
                println!("自動マウントを無効にしました。");
            } else {
                println!("自動マウントは登録されていませんでした。");
            }
            Ok(())
        }
    }
}

/// 暗号化してマウントする。
#[cfg(feature = "crypt")]
async fn mount_encrypted(
    session: Session,
    target: &str,
    mode: MountMode,
    port: u16,
    tuning: terabox_mount::MountTuning,
) -> Result<()> {
    use terabox_core::crypt::{Cipher, CryptConfig};

    let config = CryptConfig::load().map_err(|e| anyhow!(e))?;
    if !config.enabled {
        bail!(
            "暗号化が有効になっていません。`tbx crypt setup` を実行するか、`tbx mount {target} --crypt` を使ってください。"
        );
    }
    let password = load_crypt_password(&config.secret_key)?;
    let cipher = Cipher::new(&password, &config.salt, &config).map_err(|e| anyhow!(e))?;

    eprintln!(
        "暗号化モードでマウントします（{}）。保存先には暗号文だけが置かれます。",
        mode_label(mode)
    );

    match mode {
        MountMode::Auto => {
            if cfg!(windows) {
                if terabox_mount::winfsp_available() {
                    return terabox_mount::mount_winfsp_encrypted(session, target, cipher, tuning).await;
                }
                eprintln!("WinFsp が見つからないため WebDAV モードで起動します。");
                terabox_mount::mount_webdav_encrypted(session, target, port, cipher).await
            } else {
                if terabox_mount::fuse_available() {
                    return terabox_mount::mount_fuse_encrypted(session, target, cipher).await;
                }
                eprintln!("FUSE が使えないため WebDAV モードで起動します。");
                terabox_mount::mount_webdav_encrypted(session, target, port, cipher).await
            }
        }
        MountMode::Winfsp => {
            terabox_mount::mount_winfsp_encrypted(session, target, cipher, tuning).await
        }
        MountMode::Fuse => terabox_mount::mount_fuse_encrypted(session, target, cipher).await,
        MountMode::Webdav => {
            terabox_mount::mount_webdav_encrypted(session, target, port, cipher).await
        }
    }
}

#[cfg(feature = "crypt")]
fn mode_label(mode: MountMode) -> &'static str {
    match mode {
        MountMode::Auto => "自動",
        MountMode::Winfsp => "WinFsp",
        MountMode::Fuse => "FUSE",
        MountMode::Webdav => "WebDAV",
    }
}

// ---- 暗号化（rclone crypt 互換）。既定は無効 ----

/// 暗号化のパスワードを資格情報ストアから読む。
// ---- 暗号化のパスワード保管 ----
//
// 資格情報ストア（keyring）は musl ビルドで使えない（D-Bus が無い）。musl でも
// 暗号化を使える必要があるため、keyring が無いビルドでは `secrets/crypt-<key>.json`
// へ 0600 で保存する。保存先の種類は `secret.rs` の判定と同じ条件にする。

/// このビルドが資格情報ストアを持ち、暗号化のパスワードを OS の金庫へ置けるか。
#[cfg(all(
    feature = "crypt",
    any(target_os = "windows", target_os = "macos", target_env = "gnu")
))]
const CRYPT_USE_KEYRING: bool = true;
#[cfg(all(
    feature = "crypt",
    not(any(target_os = "windows", target_os = "macos", target_env = "gnu"))
))]
const CRYPT_USE_KEYRING: bool = false;

#[cfg(all(
    feature = "crypt",
    any(target_os = "windows", target_os = "macos", target_env = "gnu")
))]
fn crypt_password_entry(secret_key: &str) -> Result<keyring::Entry> {
    keyring::Entry::new("tbx.terabox", &format!("crypt:{secret_key}"))
        .map_err(|e| anyhow!("資格情報ストアを開けません: {e}"))
}

/// ファイル退避のパス（`secrets/crypt-<key>.json`）。
#[cfg(feature = "crypt")]
fn crypt_password_file(secret_key: &str) -> std::path::PathBuf {
    secret::secret_dir(&terabox_core::config::tbx_home())
        .join(format!("crypt-{secret_key}.json"))
}

#[cfg(feature = "crypt")]
fn load_crypt_password(secret_key: &str) -> Result<String> {
    if CRYPT_USE_KEYRING {
        #[cfg(any(target_os = "windows", target_os = "macos", target_env = "gnu"))]
        {
            let entry = crypt_password_entry(secret_key)?;
            return match entry.get_password() {
                Ok(v) => Ok(v),
                Err(keyring::Error::NoEntry) => bail!(
                    "暗号化のパスワードが保存されていません。`tbx crypt setup` を実行してください。"
                ),
                Err(e) => bail!("暗号化のパスワードを読めません: {e}"),
            };
        }
    }

    let path = crypt_password_file(secret_key);
    if !path.exists() {
        bail!("暗号化のパスワードが保存されていません。`tbx crypt setup` を実行してください。");
    }
    let text = std::fs::read_to_string(&path)
        .map_err(|e| anyhow!("{} を読めません: {e}", path.display()))?;
    let value: serde_json::Value =
        serde_json::from_str(&text).map_err(|e| anyhow!("保存形式を読めません: {e}"))?;
    value
        .get("password")
        .and_then(|v| v.as_str())
        .map(|v| v.to_string())
        .ok_or_else(|| anyhow!("保存されたパスワードの形式が違います"))
}

#[cfg(feature = "crypt")]
fn store_crypt_password(secret_key: &str, password: &str) -> Result<()> {
    if CRYPT_USE_KEYRING {
        #[cfg(any(target_os = "windows", target_os = "macos", target_env = "gnu"))]
        {
            let entry = crypt_password_entry(secret_key)?;
            return entry
                .set_password(password)
                .map_err(|e| anyhow!("暗号化のパスワードを保存できません: {e}"));
        }
    }

    let path = crypt_password_file(secret_key);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| anyhow!("{} を作れません: {e}", parent.display()))?;
    }
    let payload = serde_json::json!({ "password": password });
    std::fs::write(&path, serde_json::to_vec(&payload)?)
        .map_err(|e| anyhow!("{} へ書けません: {e}", path.display()))?;
    // 0600 にする（Unix）。Windows では ACL を変えない（利用者のプロファイル配下）。
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600));
    }
    Ok(())
}

/// 暗号化のパスワードを削除する（`crypt disable --purge` 用）。
#[cfg(feature = "crypt")]
fn delete_crypt_password(secret_key: &str) -> Result<bool> {
    let mut removed = false;
    if CRYPT_USE_KEYRING {
        #[cfg(any(target_os = "windows", target_os = "macos", target_env = "gnu"))]
        {
            if let Ok(entry) = crypt_password_entry(secret_key) {
                match entry.delete_credential() {
                    Ok(()) => removed = true,
                    Err(keyring::Error::NoEntry) => {}
                    Err(_) => {}
                }
            }
        }
    }
    let path = crypt_password_file(secret_key);
    if path.exists() {
        std::fs::remove_file(&path)
            .map_err(|e| anyhow!("{} を消せません: {e}", path.display()))?;
        removed = true;
    }
    Ok(removed)
}

async fn cmd_crypt(action: CryptAction, json: bool) -> Result<()> {
    #[cfg(not(feature = "crypt"))]
    {
        let _ = (action, json);
        bail!("このビルドは暗号化を含んでいません。`--features crypt` を付けてビルドしてください。");
    }

    #[cfg(feature = "crypt")]
    {
        use terabox_core::crypt::CryptConfig;

        match action {
            CryptAction::Status => {
                let cfg = CryptConfig::load().map_err(|e| anyhow!(e))?;
                let saved = load_crypt_password(&cfg.secret_key).is_ok();
                if json {
                    println!(
                        "{}",
                        serde_json::json!({
                            "enabled": cfg.enabled,
                            "filename_encryption": cfg.filename_encryption,
                            "directory_name_encryption": cfg.directory_name_encryption,
                            "salt": if cfg.salt.is_empty() { "既定（rclone と同じ）" } else { "設定済み" },
                            "password_saved": saved,
                            "compat": "rclone crypt (standard)",
                        })
                    );
                } else {
                    println!("暗号化: {}", if cfg.enabled { "有効" } else { "無効（既定）" });
                    println!("方式: rclone crypt 互換（{}）", cfg.filename_encryption);
                    println!(
                        "ディレクトリ名: {}",
                        if cfg.directory_name_encryption {
                            "暗号化する"
                        } else {
                            "そのまま"
                        }
                    );
                    println!(
                        "salt: {}",
                        if cfg.salt.is_empty() {
                            "既定（rclone と同じ固定値）"
                        } else {
                            "設定済み"
                        }
                    );
                    println!(
                        "パスワード: {}",
                        if saved {
                            "保存済み（資格情報ストア）"
                        } else {
                            "未保存（`tbx crypt setup` を実行してください）"
                        }
                    );
                    if !cfg.enabled {
                        println!();
                        println!(
                            "有効にするには `tbx crypt setup`、無効にするには `tbx crypt disable`。"
                        );
                    }
                }
                Ok(())
            }
            CryptAction::Setup {
                interactive,
                password,
                salt,
                yes,
            } => {
                let mut cfg = CryptConfig::load().map_err(|e| anyhow!(e))?;

                let new_password = match password {
                    Some(p) => p,
                    None => {
                        if !interactive {
                            bail!(
                                "パスワードを指定してください（`--interactive` で対話入力、または `--password`）。"
                            );
                        }
                        rpassword::prompt_password("暗号化のパスワード: ")
                            .context("パスワードを受け取れませんでした")?
                    }
                };
                if new_password.trim().is_empty() {
                    bail!("パスワードが空です。");
                }

                // 既存の暗号文と食い違うパスワードを設定すると、復号できなくなる。
                // 既に有効な場合は、警告して確認を求める。
                if cfg.enabled && !yes {
                    eprintln!("警告: 既に暗号化が有効です。パスワードを変えると、");
                    eprintln!("既存の暗号化ファイルは復号できなくなります（元に戻せません）。");
                    eprintln!("続けるには `--yes` を付けてください。");
                    bail!("中止しました。");
                }

                if let Some(s) = salt {
                    cfg.salt = s;
                }
                cfg.enabled = true;
                cfg.save().map_err(|e| anyhow!(e))?;
                store_crypt_password(&cfg.secret_key, &new_password)?;

                if json {
                    println!(
                        "{}",
                        serde_json::json!({
                            "enabled": true,
                            "salt_set": !cfg.salt.is_empty(),
                            "password_saved": true,
                        })
                    );
                } else {
                    println!("暗号化を有効にしました（rclone crypt 互換）。");
                    if CRYPT_USE_KEYRING {
                        println!("パスワードは資格情報ストアにのみ保存しています。");
                    } else {
                        println!(
                            "パスワードは {} に保存しています（このビルドは資格情報ストアを含まないため、0600 のファイルです）。",
                            crypt_password_file(&cfg.secret_key).display()
                        );
                    }
                    println!();
                    println!("注意:");
                    println!("  - パスワードを失うと、保存したデータは復号できません。");
                    println!("  - マウント中だけ平文で見えます。公式アプリや Web では暗号文だけが見えます。");
                    println!("  - 既存の平文ファイルはそのまま残ります。`tbx crypt-migrate` で移せます。");
                }
                Ok(())
            }
            CryptAction::Disable { yes, purge } => {
                let mut cfg = CryptConfig::load().map_err(|e| anyhow!(e))?;
                if !cfg.enabled {
                    println!("既に無効です。");
                    return Ok(());
                }
                if !yes {
                    eprintln!("暗号化を無効にすると、以後のマウントは平文で行われます。");
                    eprintln!("既に保存された暗号文はそのまま残り、復号にはパスワードが要ります。");
                    bail!("中止しました。`--yes` を付けると実行します。");
                }
                cfg.enabled = false;
                cfg.save().map_err(|e| anyhow!(e))?;
                let purged = if purge {
                    delete_crypt_password(&cfg.secret_key)?
                } else {
                    false
                };
                if json {
                    println!(
                        "{}",
                        serde_json::json!({"enabled": false, "password_deleted": purged})
                    );
                } else {
                    println!("暗号化を無効にしました。保存済みの暗号文は残っています。");
                    if purged {
                        println!("保存済みのパスワードも削除しました。");
                    }
                }
                Ok(())
            }
        }
    }
}


/// 既存の平文ファイルを暗号化形式へ移す。
///
/// 既定は下見（`--apply` を付けるまで何もしない）。暗号化して送り、
/// サーバ側のサイズが期待値と一致したことを確認してから、元を消す。
/// 一致しなければ元を残す（移行の途中でデータを失わない）。
async fn cmd_crypt_migrate(
    path: &str,
    apply: bool,
    delete_source: bool,
    json: bool,
) -> Result<()> {
    #[cfg(not(feature = "crypt"))]
    {
        let _ = (path, apply, delete_source, json);
        bail!("このビルドは暗号化を含んでいません。`--features crypt` を付けてビルドしてください。");
    }

    #[cfg(feature = "crypt")]
    {
        use terabox_core::crypt::{Cipher, CryptConfig};

        let config = CryptConfig::load().map_err(|e| anyhow!(e))?;
        if !config.enabled {
            bail!("暗号化が有効になっていません。`tbx crypt setup` を実行してください。");
        }
        let password = load_crypt_password(&config.secret_key)?;
        let cipher = Cipher::new(&password, &config.salt, &config).map_err(|e| anyhow!(e))?;

        let client = api().await?;
        let root = terabox_core::vfs::normalize_path(path);

        // 対象を集める（再帰）。
        let mut queue = vec![root.clone()];
        let mut files: Vec<(String, String, u64)> = Vec::new();
        let mut dirs: Vec<String> = Vec::new();
        while let Some(dir) = queue.pop() {
            let mut page = 1;
            loop {
                let resp = client
                    .list(&dir, page, 1000, "name", false)
                    .await
                    .map_err(map_session_error)?;
                if resp.list.is_empty() {
                    break;
                }
                for entry in &resp.list {
                    let rel = entry
                        .path
                        .strip_prefix(&root)
                        .unwrap_or(&entry.path)
                        .trim_start_matches('/')
                        .to_string();
                    if entry.isdir {
                        dirs.push(rel);
                        queue.push(entry.path.clone());
                    } else {
                        files.push((entry.path.clone(), rel, entry.size));
                    }
                }
                if resp.has_more.unwrap_or(0) == 0 {
                    break;
                }
                page += 1;
            }
        }

        if files.is_empty() {
            println!("{root} に移行するファイルはありません。");
            return Ok(());
        }

        println!(
            "{} のファイル {} 件、フォルダ {} 件を暗号化形式へ移します{}。",
            root,
            files.len(),
            dirs.len(),
            if apply { "" } else { "（下見のみ）" }
        );

        // 暗号化の名前は、下見でも表示する（何がどうなるかを見せる）。
        if !apply {
            for (remote, rel, size) in files.iter().take(10) {
                let encrypted = cipher.encrypt_path(rel, false);
                println!(
                    "  {remote}（{}）→ /{encrypted}（{} バイト）",
                    format::bytes(*size),
                    cipher.encrypted_size(*size)
                );
            }
            if files.len() > 10 {
                println!("  ... ほか {} 件", files.len() - 10);
            }
            println!();
            println!("実行するには `--apply` を付けてください。");
            return Ok(());
        }

        let mut moved = 0usize;
        let mut failed: Vec<String> = Vec::new();
        let mut removed = 0usize;

        for (remote, rel, size) in &files {
            // 1) 平文を読む。
            let plain = match client.dlink(remote).await {
                Ok(dlink) => {
                    let session = client.session().await;
                    let downloader = Downloader::new(&session).map_err(|e| anyhow!(e))?;
                    let mut ledger =
                        Ledger::open(&terabox_core::config::ledger_path()).map_err(|e| anyhow!(e))?;
                    let req = DownloadRequest {
                        remote_path: remote.clone(),
                        fs_id: String::new(),
                        local_path: std::env::temp_dir().join("tbx-crypt-migrate.bin"),
                        expected_size: Some(*size),
                        connections: 1,
                        part_size: 4 * 1024 * 1024,
                    };
                    match downloader.download(&dlink, &req, &mut ledger, |_| {}).await {
                        Ok(done) => match std::fs::read(&done.local_path) {
                            Ok(b) => {
                                let _ = std::fs::remove_file(&done.local_path);
                                b
                            }
                            Err(e) => {
                                failed.push(format!("{remote}: 読み出しに失敗: {e}"));
                                continue;
                            }
                        },
                        Err(e) => {
                            failed.push(format!("{remote}: 取得に失敗: {e}"));
                            continue;
                        }
                    }
                }
                Err(e) => {
                    failed.push(format!("{remote}: dlink を取得できません: {e}"));
                    continue;
                }
            };

            // 2) 暗号化して、暗号名で送る。
            let sealed = cipher.encrypt_data(&plain);
            let encrypted_rel = cipher.encrypt_path(rel, false);
            let parent_rel = match rel.rfind('/') {
                Some(i) => &rel[..i],
                None => "",
            };

            // 親フォルダを（必要なら）暗号名で作る。
            if !parent_rel.is_empty() {
                let enc_parent = cipher.encrypt_path(parent_rel, true);
                let enc_parent_abs = format!("/{enc_parent}");
                if let Err(e) = client.create_dir(&enc_parent_abs).await {
                    // 既にある場合は失敗してよい（作成済み）。
                    let _ = e;
                }
            }

            let enc_dir = if parent_rel.is_empty() {
                root.clone()
            } else {
                let enc_parent = cipher.encrypt_path(parent_rel, true);
                if root == "/" {
                    format!("/{enc_parent}")
                } else {
                    format!("{root}/{enc_parent}")
                }
            };

            let mut uploader = Uploader::new(client.clone());
            let mut ledger =
                Ledger::open(&terabox_core::config::ledger_path()).map_err(|e| anyhow!(e))?;
            let tmp = std::env::temp_dir().join(format!("tbx-crypt-{}.bin", encrypted_rel.len()));
            std::fs::write(&tmp, &sealed).map_err(|e| anyhow!("一時ファイルを書けません: {e}"))?;
            let req = UploadRequest {
                local_path: tmp.clone(),
                remote_dir: enc_dir.clone(),
                remote_name: Some(encrypted_rel.rsplit('/').next().unwrap_or("file").to_string()),
                part_size: 4 * 1024 * 1024,
            };
            let uploaded = uploader.upload(&req, &mut ledger, |_| {}).await;
            let _ = std::fs::remove_file(&tmp);
            if let Err(e) = uploaded {
                failed.push(format!("{remote}: 暗号文を送れません: {e}"));
                continue;
            }

            // 3) 送った暗号文のサイズを確認する（黙って成功を装わない）。
            let expected = cipher.encrypted_size(plain.len() as u64);
            let enc_full = format!(
                "{}/{}",
                enc_dir.trim_end_matches('/'),
                req.remote_name.as_deref().unwrap_or("file")
            );
            match client
                .list(&enc_dir, 1, 1000, "name", false)
                .await
                .ok()
                .and_then(|r| {
                    r.list
                        .into_iter()
                        .find(|e| e.path == enc_full)
                        .map(|e| e.size)
                }) {
                Some(actual) if actual == expected => {}
                Some(actual) => {
                    failed.push(format!(
                        "{remote}: 送信後のサイズが想定と違います（{actual} != {expected}）。元は残します"
                    ));
                    continue;
                }
                None => {
                    failed.push(format!("{remote}: 送信後に暗号文が見つかりません。元は残します"));
                    continue;
                }
            }
            moved += 1;

            // 4) 元を消す（明示時のみ）。
            if delete_source {
                match client.delete(&[remote.clone()]).await {
                    Ok(_) => removed += 1,
                    Err(e) => failed.push(format!("{remote}: 元を消せません: {e}")),
                }
            }
        }

        if json {
            println!(
                "{}",
                serde_json::json!({
                    "root": root,
                    "files": files.len(),
                    "moved": moved,
                    "removed_source": removed,
                    "failed": failed,
                })
            );
        } else {
            println!(
                "{} 件中 {} 件を暗号化して送りました。{}",
                files.len(),
                moved,
                if delete_source {
                    format!("元を {} 件削除しました。", removed)
                } else {
                    "元はそのまま残しています（`--delete-source` で削除できます）。".to_string()
                }
            );
            if !failed.is_empty() {
                println!("失敗 {} 件:", failed.len());
                for f in &failed {
                    println!("  {f}");
                }
            }
        }
        if !failed.is_empty() {
            bail!("{} 件が失敗しました", failed.len());
        }
        Ok(())
    }
}

async fn cmd_tui() -> Result<()> {
    let client = api().await?;
    terabox_tui::run(client).await.map_err(|e| anyhow!(e))
}

fn cmd_transfers(json: bool) -> Result<()> {
    let ledger = Ledger::open(&terabox_core::config::ledger_path()).map_err(|e| anyhow!(e))?;
    let unfinished = ledger.unfinished().map_err(|e| anyhow!(e))?;
    if json {
        let rows: Vec<serde_json::Value> = unfinished
            .iter()
            .map(|r| {
                serde_json::json!({
                    "id": r.id,
                    "kind": r.kind.as_str(),
                    "remote_path": r.remote_path,
                    "local_path": r.local_path,
                    "size": r.size,
                    "bytes_done": r.bytes_done,
                    "state": r.state.as_str(),
                })
            })
            .collect();
        println!("{}", serde_json::to_string_pretty(&rows)?);
        return Ok(());
    }
    if unfinished.is_empty() {
        println!("未完了の転送はありません。");
        return Ok(());
    }
    for r in &unfinished {
        println!(
            "{}  {}  {}/{}  {}",
            r.kind.as_str(),
            r.state.as_str(),
            format::bytes(r.bytes_done),
            format::bytes(r.size),
            r.remote_path
        );
    }
    Ok(())
}

async fn cmd_doctor(json: bool) -> Result<()> {
    let mut checks: Vec<(String, bool, String)> = Vec::new();

    match Config::load() {
        Ok(cfg) => checks.push((
            "設定".into(),
            true,
            format!(
                "接続 {} 本 / 分割 {} MiB / 保存先 {}",
                cfg.connections,
                cfg.part_size_mib,
                cfg.download_dir.display()
            ),
        )),
        Err(e) => checks.push(("設定".into(), false, format!("{e}"))),
    }

    let home = terabox_core::config::tbx_home();
    let account = account_name();
    let kind = secret::detect_kind(&home, &account);
    match secret::load(&home, &account, kind) {
        Ok(Some(s)) => checks.push((
            "セッション".into(),
            true,
            format!(
                "アカウント {account}（保存先 {}）/ {} / 書き込み {}",
                kind.label(),
                s.account_label(),
                s.can_write()
            ),
        )),
        Ok(None) => checks.push((
            "セッション".into(),
            false,
            format!(
                "アカウント {account} は未ログイン（`tbx login --name {account}` を実行してください）"
            ),
        )),
        Err(e) => checks.push(("セッション".into(), false, e)),
    }

    match accounts::load(&home) {
        Ok(index) => checks.push((
            "アカウント".into(),
            true,
            format!(
                "{} 件（既定 {}）",
                index.accounts.len(),
                if index.current.is_empty() {
                    "-".to_string()
                } else {
                    index.current.clone()
                }
            ),
        )),
        Err(e) => checks.push(("アカウント".into(), false, e)),
    }

    if let Ok(client) = api().await {
        match client.list("/", 1, 1, "name", false).await {
            Ok(_) => checks.push(("API /api/list".into(), true, "応答あり".into())),
            Err(TbxError::Api(err)) if err.is_not_logged_in() => checks.push((
                "API /api/list".into(),
                false,
                format!("未ログイン応答（errno {}）", err.errno),
            )),
            Err(e) => checks.push(("API /api/list".into(), false, format!("{e}"))),
        }
        match client.quota().await {
            Ok(q) => {
                let missing = if q.unavailable.is_empty() {
                    "全項目を取得".to_string()
                } else {
                    format!("未取得 {} 項目", q.unavailable.len())
                };
                checks.push(("API /api/quota".into(), true, missing));
            }
            Err(TbxError::Api(err)) if err.is_not_logged_in() => checks.push((
                "API /api/quota".into(),
                false,
                format!("未ログイン応答（errno {}）", err.errno),
            )),
            Err(e) => checks.push(("API /api/quota".into(), false, format!("{e}"))),
        }
    }

    let cfg = config().unwrap_or_default();
    match pick_player(&cfg) {
        Some(p) => checks.push(("プレーヤ".into(), true, p)),
        None => checks.push((
            "プレーヤ".into(),
            false,
            "mpv / VLC / ffplay が見つかりません".into(),
        )),
    }

    match login::find_browser() {
        Some(b) => checks.push((
            "ブラウザ".into(),
            true,
            format!("{} ({})", b.name, b.path.display()),
        )),
        None => checks.push((
            "ブラウザ".into(),
            false,
            "Chrome / Edge が見つかりません（貼り付けログインは可能）".into(),
        )),
    }

    let winfsp = terabox_mount::winfsp_available();
    checks.push((
        "WinFsp".into(),
        winfsp,
        if winfsp {
            "利用可能".into()
        } else {
            "未導入（WebDAV モードで代替できます）".into()
        },
    ));

    // 初心者が最初に見るべき状態。有効/無効と、有効時の注意を 1 行で出す。
    #[cfg(feature = "crypt")]
    match terabox_core::crypt::CryptConfig::load() {
        Ok(cfg) => {
            let saved = cfg.enabled && load_crypt_password(&cfg.secret_key).is_ok();
            checks.push((
                "暗号化".into(),
                !cfg.enabled || saved,
                if !cfg.enabled {
                    "無効（既定）。`tbx crypt setup` で有効にできます".to_string()
                } else if saved {
                    "有効（rclone crypt 互換）。パスワードは資格情報ストアに保存済み".to_string()
                } else {
                    "有効ですがパスワードが未保存。`tbx crypt setup` を実行してください".to_string()
                },
            ));
        }
        Err(e) => checks.push(("暗号化".into(), false, format!("設定を読めません: {e}"))),
    }
    #[cfg(not(feature = "crypt"))]
    checks.push((
        "暗号化".into(),
        true,
        "このビルドは暗号化を含みません（`--features crypt` で有効）".to_string(),
    ));

    match autostart::status() {
        autostart::State::Enabled { command } => {
            checks.push(("自動マウント".into(), true, format!("有効: {command}")))
        }
        autostart::State::Disabled => checks.push((
            "自動マウント".into(),
            true,
            "無効（既定）。`tbx autostart enable` で有効にできます".to_string(),
        )),
        autostart::State::Unknown(reason) => {
            checks.push(("自動マウント".into(), false, reason))
        }
    }

    if json {
        let out: Vec<serde_json::Value> = checks
            .iter()
            .map(|(name, ok, detail)| serde_json::json!({"check": name, "ok": ok, "detail": detail}))
            .collect();
        println!("{}", serde_json::to_string_pretty(&out)?);
        return Ok(());
    }

    for (name, ok, detail) in &checks {
        println!("{} {name}: {detail}", if *ok { "[ok]  " } else { "[注意]" });
    }
    Ok(())
}
