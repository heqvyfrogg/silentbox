//! `tbx` のコマンドライン定義と実行。
//!
//! 標準出力はデータ、標準エラーは進捗とメッセージ。`--json` を付けた場合は
//! 標準出力に JSON だけを出す（パイプで使えるようにする）。

mod cdp;
mod credentials;
mod format;
mod login;
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
    },
    /// 保存済みセッションを削除する。
    Logout,
    /// 現在のアカウント・容量・VIP 状態を表示する。
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
        /// リモートの絶対パス。
        path: String,
        /// 保存先ディレクトリ（既定は設定の download_dir）。
        #[arg(short, long)]
        output: Option<PathBuf>,
        /// 保存ファイル名の上書き。
        #[arg(long)]
        name: Option<String>,
        /// 分割接続数。
        #[arg(long)]
        conn: Option<u8>,
        /// 既存ファイルがあっても最初から取り直す。
        #[arg(long)]
        restart: bool,
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
        /// ドライブレター（例: `T:`）。
        drive: String,
        /// バックエンド。
        #[arg(long, value_enum, default_value_t = MountMode::Auto)]
        mode: MountMode,
        /// WebDAV の待受ポート。
        #[arg(long, default_value_t = 8765)]
        port: u16,
    },
    /// 対話画面（TUI）を開く。
    Tui,
    /// 環境と接続を診断する。
    Doctor,
    /// 未完了の転送を表示する。
    Transfers,
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
    /// WinFsp があれば使い、無ければ WebDAV に切り替える。
    Auto,
    /// WinFsp を使う。
    Winfsp,
    /// WebDAV を使う。
    Webdav,
}

fn main() -> Result<()> {
    let cli = Cli::parse();

    if let Some(home) = &cli.home {
        std::env::set_var("TBX_HOME", home);
    }

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
        } => cmd_login(paste, port, profile, json).await,
        Command::Logout => cmd_logout(json),
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
        } => cmd_get(&path, output, name, conn, restart, json).await,
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
        Command::Mount { drive, mode, port } => cmd_mount(&drive, mode, port).await,
        Command::Tui => cmd_tui().await,
        Command::Doctor => cmd_doctor(json).await,
        Command::Transfers => cmd_transfers(json),
    }
}

fn config() -> Result<Config> {
    Config::load().map_err(|e| anyhow!(e))
}

fn load_session() -> Result<Session> {
    match credentials::load() {
        Ok(Some(s)) => Ok(s),
        Ok(None) => bail!("保存済みセッションがありません。`tbx login` を実行してください。"),
        Err(e) => bail!("{e}"),
    }
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

async fn cmd_login(paste: bool, port: u16, profile: Option<PathBuf>, json: bool) -> Result<()> {
    let mut from_browser = None;
    if !paste {
        let profile = profile.unwrap_or_else(default_profile_dir);
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

    match credentials::save(&session) {
        credentials::SaveOutcome::Saved => {}
        credentials::SaveOutcome::Unavailable(reason) => {
            eprintln!("警告: {reason}");
            eprintln!("このセッションは保存されていません。次の実行では再ログインが必要です。");
        }
    }

    if json {
        println!(
            "{}",
            serde_json::json!({
                "logged_in": true,
                "account": session.account_label(),
                "can_write": session.can_write(),
            })
        );
    } else {
        println!("ログインしました（{}）", session.account_label());
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

fn default_profile_dir() -> PathBuf {
    terabox_core::config::tbx_home().join("browser-profile")
}

fn cmd_logout(json: bool) -> Result<()> {
    match credentials::delete() {
        Ok(true) => {
            if json {
                println!("{}", serde_json::json!({"logged_out": true}));
            } else {
                println!("保存済みセッションを削除しました。");
            }
            Ok(())
        }
        Ok(false) => {
            if json {
                println!(
                    "{}",
                    serde_json::json!({"logged_out": false, "reason": "no session"})
                );
            } else {
                println!("保存済みセッションはありませんでした。");
            }
            Ok(())
        }
        Err(e) => bail!("{e}"),
    }
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
    json: bool,
) -> Result<()> {
    let client = api().await?;
    let cfg = config()?;
    let normalized = terabox_core::vfs::normalize_path(path);
    let file_name = name.unwrap_or_else(|| terabox_core::vfs::base_name(&normalized));
    if file_name.is_empty() {
        bail!("ダウンロードするファイル名を決められません。`--name` で指定してください。");
    }

    // メタ情報からサイズと fs_id を得る。取得できなくても dlink で進める。
    let meta = client
        .list(
            &terabox_core::vfs::parent_path(&normalized),
            1,
            1000,
            "name",
            false,
        )
        .await
        .ok()
        .and_then(|resp| resp.list.into_iter().find(|e| e.display_name() == file_name));

    if let Some(entry) = &meta {
        if entry.isdir {
            bail!("{normalized} はフォルダです。フォルダ一括取得は未対応です。");
        }
    }

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

async fn cmd_mount(drive: &str, mode: MountMode, port: u16) -> Result<()> {
    let client = api().await?;
    let session = client.session().await;
    match mode {
        MountMode::Auto => {
            if terabox_mount::winfsp_available() {
                terabox_mount::mount_winfsp(session, drive).await
            } else {
                eprintln!("WinFsp が見つからないため WebDAV モードで起動します。");
                eprintln!("（WinFsp を入れるとエクスプローラの仮想ファイルとして扱えます）");
                terabox_mount::mount_webdav(session, drive, port).await
            }
        }
        MountMode::Winfsp => terabox_mount::mount_winfsp(session, drive).await,
        MountMode::Webdav => terabox_mount::mount_webdav(session, drive, port).await,
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

    match credentials::load() {
        Ok(Some(s)) => checks.push((
            "セッション".into(),
            true,
            format!("{} / 書き込み {}", s.account_label(), s.can_write()),
        )),
        Ok(None) => checks.push((
            "セッション".into(),
            false,
            "未ログイン（`tbx login` を実行してください）".into(),
        )),
        Err(e) => checks.push(("セッション".into(), false, e)),
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
