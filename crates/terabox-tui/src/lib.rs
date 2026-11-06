//! `tbx tui` の対話画面。
//!
//! 画面は 4 つ: 一覧 / 転送 / 共有 / 容量。常に左に一覧、右に情報を置く 2 ペイン
//! 構成にして、キー操作だけで完結させる。
//!
//! ## 表示方針
//! - ダーク配色（背景 #101317 / パネル #171b21 / 罫線 #262c35 /
//!   本文 #d8dee7 / アクセント #4f9cf9）を 256 色へ割り当てる。
//! - 1 行 1 エントリの高密度表示。絵文字は使わない。
//! - 長いパスやハッシュは省略記号で切らない。横スクロールで全文を見せる。

use std::time::Duration;

use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyModifiers};
use crossterm::execute;
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, List, ListItem, ListState, Paragraph, Wrap};
use ratatui::{Frame, Terminal};
use terabox_core::api::ApiClient;
use terabox_core::models::{FsEntry, QuotaInfo};
use terabox_core::vfs::{base_name, normalize_path, parent_path};

/// 配色。端末の 256 色に寄せた近似値。
mod palette {
    use ratatui::style::Color;

    pub const BG: Color = Color::Rgb(16, 19, 23);
    pub const PANEL: Color = Color::Rgb(23, 27, 33);
    pub const BORDER: Color = Color::Rgb(38, 44, 53);
    pub const TEXT: Color = Color::Rgb(216, 222, 231);
    pub const ACCENT: Color = Color::Rgb(79, 156, 249);
    pub const MUTED: Color = Color::Rgb(140, 150, 165);
    /// 成功・正常を表す色。
    pub const OK: Color = Color::Rgb(90, 200, 130);
    /// 注意を表す色。
    pub const WARN: Color = Color::Rgb(230, 190, 90);
    /// 失敗を表す色。
    pub const ERROR: Color = Color::Rgb(230, 110, 110);
}

/// 右ペインの表示内容。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Pane {
    Transfers,
    Share,
    Quota,
    Help,
}

/// 画面の状態。
struct App {
    api: ApiClient,
    cwd: String,
    entries: Vec<FsEntry>,
    selected: usize,
    offset: usize,
    pane: Pane,
    status: String,
    quota: Option<QuotaInfo>,
    logs: Vec<String>,
}

/// 描画と入力のループを回す。
pub async fn run(api: ApiClient) -> anyhow::Result<()> {
    enable_raw_mode()?;
    let mut stdout = std::io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;
    terminal.clear()?;

    let mut app = App {
        api,
        cwd: "/".to_string(),
        entries: Vec::new(),
        selected: 0,
        offset: 0,
        pane: Pane::Transfers,
        status: String::from("読み込み中..."),
        quota: None,
        logs: Vec::new(),
    };

    app.reload().await;
    app.load_quota().await;

    let result = event_loop(&mut terminal, &mut app).await;

    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;
    result
}

async fn event_loop(
    terminal: &mut Terminal<CrosstermBackend<std::io::Stdout>>,
    app: &mut App,
) -> anyhow::Result<()> {
    loop {
        terminal.draw(|f| draw(f, app))?;

        // 250ms ごとに入力を確認し、無ければ再描画する（時計・進捗の更新用）。
        if event::poll(Duration::from_millis(250))? {
            if let Event::Key(key) = event::read()? {
                if !handle_key(app, key).await? {
                    return Ok(());
                }
            }
        }
    }
}

/// キー入力を処理する。`false` を返すと終了。
async fn handle_key(app: &mut App, key: KeyEvent) -> anyhow::Result<bool> {
    // Ctrl+C / q で終了。
    if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
        return Ok(false);
    }

    match key.code {
        KeyCode::Char('q') => return Ok(false),
        KeyCode::Char('j') | KeyCode::Down => {
            if app.selected + 1 < app.entries.len() {
                app.selected += 1;
            }
        }
        KeyCode::Char('k') | KeyCode::Up => {
            if app.selected > 0 {
                app.selected -= 1;
            }
        }
        KeyCode::Char('g') => app.selected = 0,
        KeyCode::Char('G') => app.selected = app.entries.len().saturating_sub(1),
        KeyCode::Enter | KeyCode::Char('l') | KeyCode::Right => {
            if let Some(entry) = app.entries.get(app.selected) {
                if entry.isdir {
                    app.cwd = normalize_path(&entry.path);
                    app.selected = 0;
                    app.reload().await;
                }
            }
        }
        KeyCode::Char('h') | KeyCode::Left | KeyCode::Backspace => {
            if app.cwd != "/" {
                app.cwd = parent_path(&app.cwd);
                app.selected = 0;
                app.reload().await;
            }
        }
        KeyCode::Char('r') => {
            app.reload().await;
            app.load_quota().await;
        }
        KeyCode::Char('1') => app.pane = Pane::Transfers,
        KeyCode::Char('2') => app.pane = Pane::Share,
        KeyCode::Char('3') => app.pane = Pane::Quota,
        KeyCode::Char('?') => app.pane = Pane::Help,
        KeyCode::Char('d') => {
            if let Some(entry) = app.entries.get(app.selected).cloned() {
                if !entry.isdir {
                    app.status = format!("{} をダウンロードします", entry.display_name());
                    download_entry(app, &entry).await;
                }
            }
        }
        KeyCode::Char('p') => {
            if let Some(entry) = app.entries.get(app.selected).cloned() {
                if !entry.isdir {
                    play_entry(app, &entry).await;
                }
            }
        }
        _ => {}
    }
    Ok(true)
}

impl App {
    async fn reload(&mut self) {
        match self.api.list(&self.cwd, 1, 2000, "name", false).await {
            Ok(resp) => {
                self.entries = resp.list;
                if self.selected >= self.entries.len() {
                    self.selected = self.entries.len().saturating_sub(1);
                }
                self.status = format!("{} 件 / {}", self.entries.len(), self.cwd);
            }
            Err(e) => {
                self.status = format!("読み込み失敗: {e}");
                self.entries.clear();
            }
        }
    }

    async fn load_quota(&mut self) {
        match self.api.quota().await {
            Ok(q) => self.quota = Some(q),
            Err(e) => self.logs.push(format!("容量を取得できません: {e}")),
        }
    }

    fn push_log(&mut self, line: String) {
        self.logs.push(line);
        if self.logs.len() > 500 {
            self.logs.remove(0);
        }
    }
}

async fn download_entry(app: &mut App, entry: &FsEntry) {
    let cfg = terabox_core::config::Config::load().unwrap_or_default();
    let dir = cfg.download_dir.clone();
    if let Err(e) = std::fs::create_dir_all(&dir) {
        app.push_log(format!("保存先を作れません: {e}"));
        return;
    }
    match app.api.dlink(&entry.path).await {
        Ok(dlink) => {
            let session = app.api.session().await;
            let downloader = match terabox_core::transfer::Downloader::new(&session) {
                Ok(d) => d,
                Err(e) => {
                    app.push_log(format!("{e}"));
                    return;
                }
            };
            let mut ledger =
                match terabox_core::store::Ledger::open(&terabox_core::config::ledger_path()) {
                    Ok(l) => l,
                    Err(e) => {
                        app.push_log(format!("台帳を開けません: {e}"));
                        return;
                    }
                };
            let req = terabox_core::transfer::DownloadRequest {
                remote_path: entry.path.clone(),
                fs_id: entry.fs_id.clone(),
                local_path: dir.join(entry.display_name()),
                expected_size: Some(entry.size),
                connections: cfg.connections,
                part_size: cfg.part_size_bytes(),
            };
            let log = &mut app.logs;
            let outcome = downloader
                .download(&dlink, &req, &mut ledger, |p| {
                    if let terabox_core::transfer::Progress::Overall { done, total, .. } = p {
                        if done == total {
                            log.push(format!("完了: {}", entry.display_name()));
                        }
                    }
                })
                .await;
            match outcome {
                Ok(done) => {
                    app.status = format!(
                        "保存しました: {}（{} バイト）",
                        done.local_path.display(),
                        done.bytes
                    );
                }
                Err(e) => app.push_log(format!("ダウンロード失敗: {e}")),
            }
        }
        Err(e) => app.push_log(format!("dlink を取得できません: {e}")),
    }
}

async fn play_entry(app: &mut App, entry: &FsEntry) {
    match app.api.dlink(&entry.path).await {
        Ok(dlink) => {
            let cfg = terabox_core::config::Config::load().unwrap_or_default();
            let mut started = false;
            for name in &cfg.players {
                let candidate = if name.eq_ignore_ascii_case("vlc") {
                    std::env::var("PROGRAMFILES")
                        .ok()
                        .map(|pf| std::path::PathBuf::from(pf).join(r"VideoLAN\VLC\vlc.exe"))
                        .filter(|p| p.exists())
                        .map(|p| p.display().to_string())
                } else {
                    which_on_path(name)
                };
                if let Some(path) = candidate {
                    if std::process::Command::new(&path).arg(&dlink).spawn().is_ok() {
                        app.status = format!("{name} で再生しています");
                        started = true;
                        break;
                    }
                }
            }
            if !started {
                app.push_log(
                    "プレーヤが見つかりません（mpv / VLC / ffplay）。設定の players を確認してください。"
                        .into(),
                );
            }
        }
        Err(e) => app.push_log(format!("dlink を取得できません: {e}")),
    }
}

/// PATH 上の実行ファイルを探す。
fn which_on_path(name: &str) -> Option<String> {
    let path = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path) {
        for ext in ["", ".exe", ".bat", ".cmd"] {
            let candidate = dir.join(format!("{name}{ext}"));
            if candidate.is_file() {
                return Some(candidate.display().to_string());
            }
        }
    }
    None
}

fn draw(f: &mut Frame, app: &mut App) {
    let area = f.area();
    f.render_widget(
        Block::default().style(Style::default().bg(palette::BG)),
        area,
    );

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(2),
            Constraint::Min(5),
            Constraint::Length(1),
        ])
        .split(area);

    draw_header(f, chunks[0], app);

    let body = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(58), Constraint::Percentage(42)])
        .split(chunks[1]);

    draw_list(f, body[0], app);
    draw_pane(f, body[1], app);

    draw_footer(f, chunks[2], app);
}

fn draw_header(f: &mut Frame, area: Rect, app: &App) {
    let line = Line::from(vec![
        Span::styled(" tbx ", Style::default().fg(palette::BG).bg(palette::ACCENT).add_modifier(Modifier::BOLD)),
        Span::styled(format!(" {} ", app.cwd), Style::default().fg(palette::TEXT)),
        Span::styled(
            format!(" {} 件 ", app.entries.len()),
            Style::default().fg(palette::MUTED),
        ),
        Span::styled(
            match app.pane {
                Pane::Transfers => " [1] 転送 ",
                Pane::Share => " [2] 共有 ",
                Pane::Quota => " [3] 容量 ",
                Pane::Help => " [?] ヘルプ ",
            },
            Style::default().fg(palette::ACCENT),
        ),
    ]);
    f.render_widget(
        Paragraph::new(line).style(Style::default().bg(palette::PANEL)),
        area,
    );
}

fn draw_list(f: &mut Frame, area: Rect, app: &mut App) {
    let items: Vec<ListItem> = app
        .entries
        .iter()
        .map(|e| {
            let icon = if e.isdir { "DIR " } else { "FILE" };
            let size = if e.isdir {
                String::new()
            } else {
                format!("{:>10}", human(e.size))
            };
            ListItem::new(Line::from(vec![
                Span::styled(
                    format!("{icon} "),
                    Style::default().fg(if e.isdir {
                        palette::ACCENT
                    } else {
                        palette::MUTED
                    }),
                ),
                Span::styled(size, Style::default().fg(palette::MUTED)),
                Span::raw("  "),
                Span::styled(
                    e.display_name().to_string(),
                    Style::default().fg(palette::TEXT),
                ),
            ]))
        })
        .collect();

    let list = List::new(items)
        .style(Style::default().bg(palette::PANEL))
        .highlight_style(
            Style::default()
                .bg(Color::Rgb(30, 36, 44))
                .fg(palette::TEXT)
                .add_modifier(Modifier::BOLD),
        )
        .highlight_symbol("");

    let mut state = ListState::default();
    state.select(Some(app.selected));
    app.offset = state.offset();
    f.render_stateful_widget(list, area, &mut state);
}

fn draw_pane(f: &mut Frame, area: Rect, app: &App) {
    let (title, body) = match app.pane {
        Pane::Transfers => (
            " 転送 ",
            app.logs
                .iter()
                .rev()
                .take(area.height as usize)
                .rev()
                .cloned()
                .collect::<Vec<_>>()
                .join("\n"),
        ),
        Pane::Share => (
            " 共有 ",
            "共有リンクは CLI から: tbx share <url> [--save]\n\nこの画面からはまだ実行できません。".to_string(),
        ),
        Pane::Quota => {
            let text = match &app.quota {
                Some(q) => quota_text(q),
                None => "容量を取得していません（r で再取得）".to_string(),
            };
            (" 容量 ", text)
        }
        Pane::Help => (
            " ヘルプ ",
            [
                "j / k      移動",
                "Enter / l  開く",
                "h / ←      戻る",
                "g / G      先頭 / 末尾",
                "d          ダウンロード",
                "p          外部プレーヤで再生",
                "r          再読み込み",
                "1 2 3 ?    転送 / 共有 / 容量 / ヘルプ",
                "q          終了",
            ]
            .join("\n"),
        ),
    };

    let paragraph = Paragraph::new(body)
        .style(Style::default().fg(palette::TEXT).bg(palette::PANEL))
        .wrap(Wrap { trim: false })
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(palette::BORDER))
                .title(Span::styled(title, Style::default().fg(palette::ACCENT))),
        );
    f.render_widget(paragraph, area);
}

fn quota_text(q: &QuotaInfo) -> String {
    let mut lines = Vec::new();
    lines.push(format!(
        "総容量      {}",
        q.total_bytes.map(human).unwrap_or_else(|| "未取得".into())
    ));
    lines.push(format!(
        "使用量      {}",
        q.used_bytes.map(human).unwrap_or_else(|| "未取得".into())
    ));
    lines.push(format!(
        "残量        {}",
        q.free_bytes().map(human).unwrap_or_else(|| "未取得".into())
    ));
    lines.push(format!(
        "VIP         {}",
        match q.is_vip {
            Some(true) => "あり",
            Some(false) => "なし",
            None => "未取得",
        }
    ));
    lines.push(format!(
        "単一上限    {}",
        q.max_file_size_bytes
            .map(human)
            .unwrap_or_else(|| "未取得".into())
    ));
    if !q.unavailable.is_empty() {
        lines.push(String::new());
        lines.push("未取得の理由:".to_string());
        for item in &q.unavailable {
            lines.push(format!("  {item}"));
        }
    }
    lines.join("\n")
}

fn draw_footer(f: &mut Frame, area: Rect, app: &App) {
    // 状態の種類を色で示す。文言に依存した判定はここ 1 箇所だけに閉じる。
    let color = if app.status.contains("失敗") || app.status.contains("できません") {
        palette::ERROR
    } else if app.status.contains("未取得") || app.status.contains("未ログイン") {
        palette::WARN
    } else if app.status.contains("保存しました") || app.status.contains("完了") {
        palette::OK
    } else {
        palette::MUTED
    };
    let text = format!(" {} ", app.status);
    f.render_widget(
        Paragraph::new(text).style(Style::default().fg(color).bg(palette::PANEL)),
        area,
    );
}

fn human(n: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
    if n < 1024 {
        return format!("{n} B");
    }
    let mut v = n as f64;
    let mut u = 0;
    while v >= 1024.0 && u < UNITS.len() - 1 {
        v /= 1024.0;
        u += 1;
    }
    format!("{v:.1} {}", UNITS[u])
}

/// 使っていない補助。`base_name` を将来のパンくず表示で使う。
#[allow(dead_code)]
fn crumb(path: &str) -> String {
    base_name(path)
}
