//! WebDAV バックエンド。
//!
//! インストーラ不要でドライブレターを割り当てたい場合の経路。ローカルに
//! WebDAV サーバを立て、`net use <drive> http://127.0.0.1:<port>/` で繋ぐ。
//!
//! Windows の WebDAV クライアント（WebClient）は Basic 認証を平文で送るため、
//! 待受は **127.0.0.1 のみ** に限定する。外部公開はしない。
//!
//! 対応メソッド: `OPTIONS` / `PROPFIND` / `GET` / `HEAD` / `PUT` / `DELETE` /
//! `MKCOL` / `MOVE`。`Depth` は 0 と 1 のみ扱う（infinity は 403）。
//! `GET` は `Range` に対応し、動画プレーヤのシークに耐える。本文は
//! 1MiB ずつ読み出して流すため、巨大ファイルでもメモリを圧迫しない。

use std::convert::Infallible;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::Arc;

use bytes::Bytes;
use futures::stream;
use http_body_util::{combinators::UnsyncBoxBody, BodyExt, Full, StreamBody};
use hyper::body::{Body as HttpBody, Frame};
use hyper::header::{
    HeaderValue, CONTENT_LENGTH, CONTENT_RANGE, CONTENT_TYPE,
};
use hyper::{Request, Response, StatusCode};
use hyper_util::rt::TokioIo;
use terabox_core::error::TbxError;
use terabox_core::models::FsEntry;
use terabox_core::session::Session;
use terabox_core::vfs::{normalize_path, RemoteFs};
use tokio::net::TcpListener;

use crate::remote::TeraBoxFs;

/// 応答本文の型。
type Body = UnsyncBoxBody<Bytes, std::io::Error>;

/// 1 回の読み出しで流す単位。
const STREAM_CHUNK: u64 = 1024 * 1024;

/// ローカル限定の WebDAV サーバを立て、`net use` でドライブを割り当てる。
pub async fn serve_and_connect(session: Session, drive: &str, port: u16) -> anyhow::Result<()> {
    let api = terabox_core::api::ApiClient::new(session.clone())?;
    let fs: Arc<dyn RemoteFs + Send + Sync> = Arc::new(TeraBoxFs::new(api, session));

    let addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), port);
    let listener = TcpListener::bind(addr).await.map_err(|e| {
        anyhow::anyhow!(
            "ポート {port} を待ち受けできませんでした（既に使用中の可能性があります）: {e}"
        )
    })?;
    let actual_port = listener.local_addr()?.port();

    println!("WebDAV サーバを http://127.0.0.1:{actual_port}/ で起動しました。");
    println!("ドライブ {drive} を割り当てます...");

    connect_drive(drive, actual_port)?;
    println!("{drive} を割り当てました。Ctrl+C で終了します（終了時に切断します）。");

    let server = tokio::spawn(async move {
        loop {
            let (stream, _) = match listener.accept().await {
                Ok(v) => v,
                Err(_) => break,
            };
            let fs = Arc::clone(&fs);
            tokio::spawn(async move {
                let io = TokioIo::new(stream);
                let _ = hyper::server::conn::http1::Builder::new()
                    .serve_connection(
                        io,
                        hyper::service::service_fn(move |req| {
                            let fs = Arc::clone(&fs);
                            async move { handle(fs, req).await }
                        }),
                    )
                    .await;
            });
        }
    });

    tokio::select! {
        _ = tokio::signal::ctrl_c() => {}
        _ = server => {}
    }

    disconnect_drive(drive);
    println!("{drive} を切断しました。");
    Ok(())
}

/// ドライブを割り当てる。
///
/// Windows は `net use` でドライブレターを割り当てる。Unix では接続コマンドを
/// **実行しない**（`mount.davfs` は root 権限と `/etc/fstab` を要求するため、
/// 勝手に走らせない）。代わりに実際の URL と、その環境で使える手段を出す。
fn connect_drive(drive: &str, port: u16) -> anyhow::Result<()> {
    #[cfg(windows)]
    {
        let letter = drive.trim_end_matches(':');
        if letter.len() != 1 || !letter.chars().next().unwrap().is_ascii_alphabetic() {
            anyhow::bail!("ドライブレターの指定が不正です（例: T:）: {drive}");
        }

        // 既存の割り当てがあれば先に外す（残骸で失敗するのを避ける）。
        let _ = std::process::Command::new("net")
            .args(["use", &format!("{letter}:"), "/delete", "/y"])
            .output();

        let output = std::process::Command::new("net")
            .args([
                "use",
                &format!("{letter}:"),
                &format!("http://127.0.0.1:{port}/"),
            ])
            .output()?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            let stdout = String::from_utf8_lossy(&output.stdout);
            anyhow::bail!(
                "ドライブの割り当てに失敗しました（net use {letter}:）: {}{}",
                stdout.trim(),
                stderr.trim()
            );
        }
        let _ = drive;
        Ok(())
    }
    #[cfg(not(windows))]
    {
        let _ = drive;
        let url = format!("http://127.0.0.1:{port}/");
        println!("WebDAV の URL: {url}");
        println!("この環境では自動でマウントしません。次のいずれかで繋いでください:");
        println!("  gvfs-mount dav://127.0.0.1:{port}/        （GNOME）");
        println!("  mount -t davfs {url} <マウント先>          （davfs2・要 root）");
        println!("または `tbx mount <ディレクトリ> --mode fuse`（FUSE ビルド時）を使います。");
        Ok(())
    }
}

fn disconnect_drive(drive: &str) {
    #[cfg(windows)]
    {
        let letter = drive.trim_end_matches(':');
        let _ = std::process::Command::new("net")
            .args(["use", &format!("{letter}:"), "/delete", "/y"])
            .output();
    }
    #[cfg(not(windows))]
    {
        let _ = drive;
    }
}

/// 1 リクエストを処理する。テストからも直接呼べるようにしておく。
///
/// 本文の型を総称にしておくことで、実サーバ（`Incoming`）とテスト
/// （`Full<Bytes>`）の両方から同じ経路を通せる。
pub async fn handle<B>(fs: Arc<dyn RemoteFs + Send + Sync>, req: Request<B>) -> Result<Response<Body>, Infallible>
where
    B: HttpBody<Data = Bytes> + Send + 'static,
    B::Error: std::fmt::Display,
{
    let method = req.method().clone();
    let path = percent_decode(req.uri().path());

    let response = match method.as_str() {
        "OPTIONS" => options_response(),
        "PROPFIND" => propfind(fs.as_ref(), &path, &req).await,
        "GET" => get_file(&fs, &path, &req, false).await,
        "HEAD" => get_file(&fs, &path, &req, true).await,
        "PUT" => put_file(fs.as_ref(), &path, req).await,
        "DELETE" => delete_path(fs.as_ref(), &path).await,
        "MKCOL" => mkcol(fs.as_ref(), &path).await,
        "MOVE" => move_path(fs.as_ref(), &path, &req).await,
        _ => text_response(StatusCode::METHOD_NOT_ALLOWED, "未対応のメソッドです"),
    };

    Ok(response)
}

fn percent_decode(input: &str) -> String {
    terabox_core::percent::decode(input)
}

fn boxed(body: Full<Bytes>) -> Body {
    BodyExt::boxed_unsync(body.map_err(|never| match never {}))
}

fn empty_body() -> Body {
    boxed(Full::new(Bytes::new()))
}

fn options_response() -> Response<Body> {
    let mut resp = Response::new(empty_body());
    *resp.status_mut() = StatusCode::OK;
    let headers = resp.headers_mut();
    headers.insert("DAV", HeaderValue::from_static("1, 2"));
    headers.insert(
        "Allow",
        HeaderValue::from_static("OPTIONS, GET, HEAD, PUT, DELETE, PROPFIND, MKCOL, MOVE"),
    );
    headers.insert("MS-Author-Via", HeaderValue::from_static("DAV"));
    resp
}

fn text_response(status: StatusCode, message: &str) -> Response<Body> {
    let mut resp = Response::new(boxed(Full::new(Bytes::from(message.to_string()))));
    *resp.status_mut() = status;
    resp.headers_mut().insert(
        CONTENT_TYPE,
        HeaderValue::from_static("text/plain; charset=utf-8"),
    );
    resp
}

async fn propfind<B>(
    fs: &(dyn RemoteFs + Send + Sync),
    path: &str,
    req: &Request<B>,
) -> Response<Body>
where
    B: HttpBody<Data = Bytes>,
{
    let depth = req
        .headers()
        .get("Depth")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("1");

    if depth == "infinity" {
        return text_response(
            StatusCode::FORBIDDEN,
            "Depth: infinity は未対応です。Depth: 0 か 1 を指定してください。",
        );
    }

    let p = normalize_path(path);

    let mut items: Vec<(String, Option<FsEntry>)> = Vec::new();
    match fs.metadata(&p).await {
        Ok(Some(_)) => items.push((p.clone(), None)),
        Ok(None) => return text_response(StatusCode::NOT_FOUND, "見つかりません"),
        Err(e) => return text_response(StatusCode::BAD_GATEWAY, &format!("{e}")),
    }

    if depth == "1" {
        if let Ok(list) = fs.read_dir(&p).await {
            for entry in list {
                items.push((entry.path.clone(), Some(entry)));
            }
        }
    }

    let xml = build_multistatus(&items);
    let mut resp = Response::new(boxed(Full::new(Bytes::from(xml))));
    *resp.status_mut() = StatusCode::MULTI_STATUS;
    resp.headers_mut().insert(
        CONTENT_TYPE,
        HeaderValue::from_static("application/xml; charset=utf-8"),
    );
    resp
}

fn build_multistatus(items: &[(String, Option<FsEntry>)]) -> String {
    let mut xml = String::from(
        r#"<?xml version="1.0" encoding="utf-8"?><D:multistatus xmlns:D="DAV:">"#,
    );
    for (path, entry) in items {
        let is_dir = entry.as_ref().map(|e| e.isdir).unwrap_or(true);
        let size = entry.as_ref().map(|e| e.size).unwrap_or(0);
        let mtime = entry.as_ref().map(|e| e.server_mtime).unwrap_or(0);
        let href = encode_href(path, is_dir);
        let display = if path == "/" {
            "/".to_string()
        } else {
            path.rsplit('/').next().unwrap_or(path).to_string()
        };
        xml.push_str("<D:response><D:href>");
        xml.push_str(&href);
        xml.push_str("</D:href><D:propstat><D:prop>");
        xml.push_str("<D:displayname>");
        xml.push_str(&xml_escape(&display));
        xml.push_str("</D:displayname>");
        if is_dir {
            xml.push_str("<D:resourcetype><D:collection/></D:resourcetype>");
            xml.push_str("<D:getcontentlength>0</D:getcontentlength>");
            xml.push_str("<D:getcontenttype>httpd/unix-directory</D:getcontenttype>");
        } else {
            xml.push_str("<D:resourcetype/>");
            xml.push_str(&format!("<D:getcontentlength>{size}</D:getcontentlength>"));
            xml.push_str("<D:getcontenttype>application/octet-stream</D:getcontenttype>");
        }
        if mtime > 0 {
            if let Some(dt) = chrono::DateTime::from_timestamp(mtime, 0) {
                xml.push_str(&format!(
                    "<D:getlastmodified>{}</D:getlastmodified>",
                    dt.format("%a, %d %b %Y %H:%M:%S GMT")
                ));
            }
        }
        xml.push_str("</D:prop><D:status>HTTP/1.1 200 OK</D:status></D:propstat></D:response>");
    }
    xml.push_str("</D:multistatus>");
    xml
}

fn encode_href(path: &str, is_dir: bool) -> String {
    let encoded: String = path
        .split('/')
        .map(|seg| urlencoding::encode(seg).into_owned())
        .collect::<Vec<_>>()
        .join("/");
    if is_dir && !encoded.ends_with('/') {
        format!("{encoded}/")
    } else if encoded.is_empty() {
        "/".to_string()
    } else {
        encoded
    }
}

fn xml_escape(input: &str) -> String {
    input
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

/// `Range` ヘッダの解釈結果。
#[derive(Debug, PartialEq, Eq)]
enum RangeRequest {
    /// 指定なし。全体を返す。
    Full,
    /// 解釈できない、または multi-range。仕様どおり全体を返す。
    Unsatisfiable(u64),
    /// 部分的に返す（開始・終了とも含む）。
    Partial(u64, u64),
}

/// `Range: bytes=start-end` を解釈する。
///
/// - ヘッダ無し → `Full`
/// - `bytes=90-`  → 90 から末尾まで
/// - `bytes=-10`  → 末尾 10 バイト（HTTP の suffix range）
/// - 範囲外      → `Unsatisfiable`
/// - `bytes=0-1,5-6` のような複数範囲 → 全体を返す（`Full` 相当）
fn parse_range(header: Option<&str>, total: u64) -> RangeRequest {
    let Some(header) = header else {
        return RangeRequest::Full;
    };
    let Some(spec) = header.trim().strip_prefix("bytes=") else {
        return RangeRequest::Full;
    };
    if spec.contains(',') {
        return RangeRequest::Full;
    }
    let Some((a, b)) = spec.split_once('-') else {
        return RangeRequest::Full;
    };

    let a = a.trim();
    let b = b.trim();

    if a.is_empty() {
        // suffix range: 末尾から b バイト。
        let Ok(n) = b.parse::<u64>() else {
            return RangeRequest::Full;
        };
        if n == 0 || total == 0 {
            return RangeRequest::Unsatisfiable(total);
        }
        let n = n.min(total);
        return RangeRequest::Partial(total - n, total - 1);
    }

    let Ok(start) = a.parse::<u64>() else {
        return RangeRequest::Full;
    };
    if start >= total {
        return RangeRequest::Unsatisfiable(total);
    }
    let end = if b.is_empty() {
        total - 1
    } else {
        match b.parse::<u64>() {
            Ok(v) => v.min(total - 1),
            Err(_) => return RangeRequest::Full,
        }
    };
    if end < start {
        return RangeRequest::Unsatisfiable(total);
    }
    RangeRequest::Partial(start, end)
}

async fn get_file<B>(
    fs: &Arc<dyn RemoteFs + Send + Sync>,
    path: &str,
    req: &Request<B>,
    head_only: bool,
) -> Response<Body>
where
    B: HttpBody<Data = Bytes>,
{
    let p = normalize_path(path);
    let meta = match fs.metadata(&p).await {
        Ok(Some(m)) => m,
        Ok(None) => return text_response(StatusCode::NOT_FOUND, "見つかりません"),
        Err(e) => return text_response(StatusCode::BAD_GATEWAY, &format!("{e}")),
    };
    if meta.is_dir {
        return text_response(StatusCode::METHOD_NOT_ALLOWED, "フォルダは取得できません");
    }

    let range_header = req
        .headers()
        .get(hyper::header::RANGE)
        .and_then(|v| v.to_str().ok());

    let (status, start, end) = match parse_range(range_header, meta.size) {
        RangeRequest::Unsatisfiable(total) => {
            let mut resp = text_response(
                StatusCode::RANGE_NOT_SATISFIABLE,
                "要求された範囲がファイルサイズを超えています",
            );
            if let Ok(v) = HeaderValue::from_str(&format!("bytes */{total}")) {
                resp.headers_mut().insert(CONTENT_RANGE, v);
            }
            return resp;
        }
        RangeRequest::Full => (StatusCode::OK, 0, meta.size.saturating_sub(1)),
        RangeRequest::Partial(s, e) => (StatusCode::PARTIAL_CONTENT, s, e),
    };

    let length = if meta.size == 0 {
        0
    } else {
        end.saturating_sub(start) + 1
    };

    let body: Body = if head_only || length == 0 {
        empty_body()
    } else {
        stream_file(Arc::clone(fs), &p, start, length)
    };

    let mut resp = Response::new(body);
    *resp.status_mut() = status;
    let headers = resp.headers_mut();
    if let Ok(v) = HeaderValue::from_str(&length.to_string()) {
        headers.insert(CONTENT_LENGTH, v);
    }
    headers.insert(
        CONTENT_TYPE,
        HeaderValue::from_static("application/octet-stream"),
    );
    headers.insert(
        hyper::header::ACCEPT_RANGES,
        HeaderValue::from_static("bytes"),
    );
    if status == StatusCode::PARTIAL_CONTENT {
        if let Ok(v) = HeaderValue::from_str(&format!("bytes {start}-{end}/{}", meta.size)) {
            headers.insert(CONTENT_RANGE, v);
        }
    }
    resp
}

/// 範囲を分割して読むストリームを作る。1 チャンクずつ `read_range` を呼ぶ。
///
/// `Arc` を所有したままストリームへ移すので、`unsafe` も寿命のごまかしも要らない。
fn stream_file(fs: Arc<dyn RemoteFs + Send + Sync>, path: &str, start: u64, length: u64) -> Body {
    let path = path.to_string();

    let state = (start, length, path);
    let chunks = stream::unfold(state, move |(offset, remaining, path)| {
        // `fs` をクロージャの外で clone し、各ステップへ所有権を移す。
        let fs = Arc::clone(&fs);
        async move {
            if remaining == 0 {
                return None;
            }
            let want = remaining.min(STREAM_CHUNK);
            match fs.read_range(&path, offset, want).await {
                Ok(data) if !data.is_empty() => {
                    let n = data.len() as u64;
                    Some((
                        Ok::<_, std::io::Error>(Frame::data(Bytes::from(data))),
                        (offset + n, remaining.saturating_sub(n), path),
                    ))
                }
                Ok(_) => Some((
                    Err(std::io::Error::new(
                        std::io::ErrorKind::UnexpectedEof,
                        "読み出しが途中で終わりました",
                    )),
                    (offset, 0, path),
                )),
                Err(e) => Some((
                    Err(std::io::Error::new(std::io::ErrorKind::Other, e.to_string())),
                    (offset, 0, path),
                )),
            }
        }
    });

    BodyExt::boxed_unsync(StreamBody::new(chunks))
}

async fn put_file<B>(
    fs: &(dyn RemoteFs + Send + Sync),
    path: &str,
    req: Request<B>,
) -> Response<Body>
where
    B: HttpBody<Data = Bytes> + Send + 'static,
    B::Error: std::fmt::Display,
{
    let p = normalize_path(path);
    let body = match req.into_body().collect().await {
        Ok(b) => b.to_bytes(),
        Err(e) => {
            return text_response(StatusCode::BAD_REQUEST, &format!("本文を読めません: {e}"))
        }
    };
    match fs.write(&p, &body).await {
        Ok(()) => {
            let mut resp = Response::new(empty_body());
            *resp.status_mut() = StatusCode::CREATED;
            resp
        }
        Err(e) => text_response(StatusCode::BAD_GATEWAY, &format!("{e}")),
    }
}

async fn delete_path(fs: &(dyn RemoteFs + Send + Sync), path: &str) -> Response<Body> {
    let p = normalize_path(path);
    match fs.delete(&p).await {
        Ok(()) => {
            let mut resp = Response::new(empty_body());
            *resp.status_mut() = StatusCode::NO_CONTENT;
            resp
        }
        Err(e) => text_response(StatusCode::BAD_GATEWAY, &format!("{e}")),
    }
}

async fn mkcol(fs: &(dyn RemoteFs + Send + Sync), path: &str) -> Response<Body> {
    let p = normalize_path(path);
    match fs.create_dir(&p).await {
        Ok(()) => {
            let mut resp = Response::new(empty_body());
            *resp.status_mut() = StatusCode::CREATED;
            resp
        }
        Err(e) => text_response(StatusCode::BAD_GATEWAY, &format!("{e}")),
    }
}

async fn move_path<B>(
    fs: &(dyn RemoteFs + Send + Sync),
    path: &str,
    req: &Request<B>,
) -> Response<Body>
where
    B: HttpBody<Data = Bytes>,
{
    let dest_header = match req.headers().get("Destination").and_then(|v| v.to_str().ok()) {
        Some(v) => v.to_string(),
        None => return text_response(StatusCode::BAD_REQUEST, "Destination がありません"),
    };
    let dest_path = match dest_header.find("://") {
        Some(i) => match dest_header[i + 3..].find('/') {
            Some(j) => percent_decode(&dest_header[i + 3 + j..]),
            None => return text_response(StatusCode::BAD_REQUEST, "Destination の形式が不正です"),
        },
        None => percent_decode(&dest_header),
    };
    match fs.rename(path, &dest_path).await {
        Ok(()) => {
            let mut resp = Response::new(empty_body());
            *resp.status_mut() = StatusCode::CREATED;
            resp
        }
        Err(e) => text_response(StatusCode::BAD_GATEWAY, &format!("{e}")),
    }
}

/// テスト用にトレイトの戻り値を文字列化する。
#[allow(dead_code)]
fn describe(err: &TbxError) -> String {
    err.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn range_without_header_is_full() {
        assert_eq!(parse_range(None, 100), RangeRequest::Full);
    }

    #[test]
    fn range_open_ended() {
        assert_eq!(parse_range(Some("bytes=0-9"), 100), RangeRequest::Partial(0, 9));
        assert_eq!(
            parse_range(Some("bytes=90-"), 100),
            RangeRequest::Partial(90, 99)
        );
    }

    #[test]
    fn range_suffix_means_last_n_bytes() {
        assert_eq!(
            parse_range(Some("bytes=-10"), 100),
            RangeRequest::Partial(90, 99)
        );
        // ファイルより大きい suffix は全体。
        assert_eq!(
            parse_range(Some("bytes=-500"), 100),
            RangeRequest::Partial(0, 99)
        );
    }

    #[test]
    fn range_clamps_beyond_end() {
        assert_eq!(
            parse_range(Some("bytes=50-500"), 100),
            RangeRequest::Partial(50, 99)
        );
    }

    #[test]
    fn range_outside_file_is_unsatisfiable() {
        assert_eq!(
            parse_range(Some("bytes=200-300"), 100),
            RangeRequest::Unsatisfiable(100)
        );
    }

    #[test]
    fn multi_range_falls_back_to_full() {
        assert_eq!(parse_range(Some("bytes=0-1,5-6"), 100), RangeRequest::Full);
    }

    #[test]
    fn garbage_range_falls_back_to_full() {
        assert_eq!(parse_range(Some("items=0-9"), 100), RangeRequest::Full);
        assert_eq!(parse_range(Some("bytes=abc-def"), 100), RangeRequest::Full);
    }

    #[test]
    fn href_encodes_and_trails_dir() {
        assert_eq!(encode_href("/a b/c.txt", false), "/a%20b/c.txt");
        assert_eq!(encode_href("/dir", true), "/dir/");
        assert_eq!(encode_href("/", true), "/");
    }

    #[test]
    fn multistatus_escapes_names() {
        let xml = build_multistatus(&[(
            "/a&b.txt".to_string(),
            Some(FsEntry {
                fs_id: "1".into(),
                server_filename: "a&b.txt".into(),
                path: "/a&b.txt".into(),
                isdir: false,
                size: 3,
                server_mtime: 0,
                server_ctime: 0,
                md5: String::new(),
                thumbs: None,
                category: None,
            }),
        )]);
        assert!(xml.contains("&amp;"));
        assert!(xml.contains("<D:getcontentlength>3</D:getcontentlength>"));
    }

    /// インメモリの `RemoteFs` 実装。ネットワークを使わずにハンドラを検証する。
    struct MockFs {
        files: std::collections::HashMap<String, Vec<u8>>,
    }

    #[async_trait::async_trait]
    impl RemoteFs for MockFs {
        async fn read_dir(&self, _path: &str) -> Result<Vec<FsEntry>, TbxError> {
            Ok(vec![FsEntry {
                fs_id: "1".into(),
                server_filename: "a.bin".into(),
                path: "/a.bin".into(),
                isdir: false,
                size: self.files.get("/a.bin").map(|d| d.len() as u64).unwrap_or(0),
                server_mtime: 1_700_000_000,
                server_ctime: 0,
                md5: String::new(),
                thumbs: None,
                category: None,
            }])
        }

        async fn metadata(&self, path: &str) -> Result<Option<terabox_core::vfs::RemoteMeta>, TbxError> {
            if path == "/" {
                return Ok(Some(terabox_core::vfs::RemoteMeta {
                    path: "/".into(),
                    size: 0,
                    is_dir: true,
                    mtime: 0,
                    fs_id: "0".into(),
                }));
            }
            Ok(self.files.get(path).map(|d| terabox_core::vfs::RemoteMeta {
                path: path.to_string(),
                size: d.len() as u64,
                is_dir: false,
                mtime: 1_700_000_000,
                fs_id: "1".into(),
            }))
        }

        async fn read(&self, path: &str) -> Result<Vec<u8>, TbxError> {
            Ok(self.files.get(path).cloned().unwrap_or_default())
        }

        async fn read_range(
            &self,
            path: &str,
            offset: u64,
            length: u64,
        ) -> Result<Vec<u8>, TbxError> {
            let data = self.files.get(path).cloned().unwrap_or_default();
            let start = (offset as usize).min(data.len());
            let end = (start + length as usize).min(data.len());
            Ok(data[start..end].to_vec())
        }

        async fn write(&self, _path: &str, _data: &[u8]) -> Result<(), TbxError> {
            Ok(())
        }
        async fn delete(&self, _path: &str) -> Result<(), TbxError> {
            Ok(())
        }
        async fn rename(&self, _from: &str, _to: &str) -> Result<(), TbxError> {
            Ok(())
        }
        async fn create_dir(&self, _path: &str) -> Result<(), TbxError> {
            Ok(())
        }
    }

    fn request(method: &str, uri: &str, headers: &[(&str, &str)]) -> Request<Full<Bytes>> {
        let mut builder = Request::builder().method(method).uri(uri);
        for (k, v) in headers {
            builder = builder.header(*k, *v);
        }
        builder.body(Full::new(Bytes::new())).expect("request")
    }

    #[tokio::test]
    async fn options_advertises_dav() {
        let fs: Arc<dyn RemoteFs + Send + Sync> = Arc::new(MockFs {
            files: Default::default(),
        });
        let resp = handle(fs, request("OPTIONS", "/", &[])).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        assert_eq!(resp.headers()["DAV"], "1, 2");
    }

    #[tokio::test]
    async fn propfind_root_returns_multistatus() {
        let mut files = std::collections::HashMap::new();
        files.insert("/a.bin".to_string(), vec![0u8; 10]);
        let fs: Arc<dyn RemoteFs + Send + Sync> = Arc::new(MockFs { files });
        let resp = handle(fs, request("PROPFIND", "/", &[("Depth", "1")]))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::MULTI_STATUS);
        let body = resp.into_body().collect().await.unwrap().to_bytes();
        let xml = String::from_utf8_lossy(&body);
        assert!(xml.contains("<D:multistatus"));
        assert!(xml.contains("a.bin"));
    }

    #[tokio::test]
    async fn propfind_infinity_is_rejected() {
        let fs: Arc<dyn RemoteFs + Send + Sync> = Arc::new(MockFs {
            files: Default::default(),
        });
        let resp = handle(fs, request("PROPFIND", "/", &[("Depth", "infinity")]))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn get_returns_partial_content_for_range() {
        let mut files = std::collections::HashMap::new();
        files.insert("/a.bin".to_string(), (0..100u8).collect::<Vec<_>>());
        let fs: Arc<dyn RemoteFs + Send + Sync> = Arc::new(MockFs { files });
        let resp = handle(fs, request("GET", "/a.bin", &[("Range", "bytes=10-19")]))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::PARTIAL_CONTENT);
        assert_eq!(resp.headers()[CONTENT_RANGE], "bytes 10-19/100");
        let body = resp.into_body().collect().await.unwrap().to_bytes();
        assert_eq!(body.len(), 10);
        assert_eq!(body[0], 10);
    }

    #[tokio::test]
    async fn get_full_file_without_range() {
        let mut files = std::collections::HashMap::new();
        files.insert("/a.bin".to_string(), vec![7u8; 32]);
        let fs: Arc<dyn RemoteFs + Send + Sync> = Arc::new(MockFs { files });
        let resp = handle(fs, request("GET", "/a.bin", &[])).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = resp.into_body().collect().await.unwrap().to_bytes();
        assert_eq!(body.len(), 32);
        assert!(body.iter().all(|b| *b == 7));
    }

    #[tokio::test]
    async fn head_returns_no_body() {
        let mut files = std::collections::HashMap::new();
        files.insert("/a.bin".to_string(), vec![1u8; 16]);
        let fs: Arc<dyn RemoteFs + Send + Sync> = Arc::new(MockFs { files });
        let resp = handle(fs, request("HEAD", "/a.bin", &[])).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        assert_eq!(resp.headers()[CONTENT_LENGTH], "16");
    }

    #[tokio::test]
    async fn get_missing_file_is_404() {
        let fs: Arc<dyn RemoteFs + Send + Sync> = Arc::new(MockFs {
            files: Default::default(),
        });
        let resp = handle(fs, request("GET", "/nope.bin", &[])).await.unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }
}
