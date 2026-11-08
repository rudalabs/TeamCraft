//! Get resumes a large file half-way instead of starting it over.
//!
//! A file is downloaded into a partial file next to its place in the working
//! folder, `<name>.<first 16 hex digits of its SHA-256>.teamcraft-download`
//! (ignored by the workspace scan, see `layout::is_ignored`). A cancelled Get,
//! a dropped connection or a crash leaves it there; the next Get asks the
//! server only for the rest:
//!
//! ```text
//! Range: bytes=<length of the partial file>-
//! If-Range: "<sha256>"            the content the partial file belongs to
//! ```
//!
//! The server answers `206` with the rest, `200` with the whole file when
//! the content changed meanwhile (or it cannot resume – the partial file is
//! then started over) or `416` when the partial file is already complete.
//! The SHA-256 of the whole file (the bytes already there plus the new ones)
//! is checked before the file is renamed into its place; a resumed file that
//! does not match is started over once from the beginning. Partial files of
//! other contents of the same file (an older version) are removed first.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

use reqwest::{header, Method, StatusCode};
use sha2::{Digest, Sha256};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use crate::api::{ensure_ok, Api, ApiError};

pub const PART_SUFFIX: &str = ".teamcraft-download";

/// The partial file of `dest` for the content `sha256`.
pub fn part_path(dest: &Path, sha256: &str) -> PathBuf {
    let mut name = dest.as_os_str().to_owned();
    let tag: String = sha256.chars().take(16).collect();
    if !tag.is_empty() {
        name.push(".");
        name.push(tag.to_ascii_lowercase());
    }
    name.push(PART_SUFFIX);
    PathBuf::from(name)
}

/// Partial files of `dest` other than `keep` (older contents, the naming
/// without a hash).
fn stale_parts(dest: &Path, keep: &Path) -> Vec<PathBuf> {
    let (Some(dir), Some(name)) = (dest.parent(), dest.file_name()) else {
        return Vec::new();
    };
    let prefix = format!("{}.", name.to_string_lossy());
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    entries
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| p.as_path() != keep)
        .filter(|p| {
            p.file_name().is_some_and(|n| {
                let n = n.to_string_lossy();
                n.starts_with(&prefix) && n.ends_with(PART_SUFFIX)
            })
        })
        .collect()
}

/// The first byte of a `Content-Range: bytes <first>-<last>/<size>`.
fn range_start(value: &str) -> Option<u64> {
    value
        .trim()
        .strip_prefix("bytes ")?
        .split_once('-')?
        .0
        .trim()
        .parse()
        .ok()
}

/// SHA-256 state over the bytes of `path` (the part already downloaded).
async fn hash_file(path: &Path) -> Result<Sha256, ApiError> {
    let mut file = tokio::fs::File::open(path).await?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 1 << 20];
    loop {
        let n = file.read(&mut buf).await?;
        if n == 0 {
            return Ok(hasher);
        }
        hasher.update(&buf[..n]);
    }
}

enum Failure {
    /// A resumed file did not match its SHA-256: start over once.
    Restart,
    Api(ApiError),
}

impl From<ApiError> for Failure {
    fn from(e: ApiError) -> Failure {
        Failure::Api(e)
    }
}

impl From<std::io::Error> for Failure {
    fn from(e: std::io::Error) -> Failure {
        Failure::Api(e.into())
    }
}

impl From<reqwest::Error> for Failure {
    fn from(e: reqwest::Error) -> Failure {
        Failure::Api(e.into())
    }
}

fn damaged(dest: &Path) -> ApiError {
    ApiError::local(format!(
        "{} arrived damaged (SHA-256 does not match) – get it again.",
        dest.file_name().unwrap_or_default().to_string_lossy()
    ))
}

/// Downloads `path` into `dest` through its partial file, resuming what an
/// earlier attempt left (see the module documentation). `on_bytes` is told
/// every chunk, and once the bytes already there. Returns the bytes received
/// in this call.
pub async fn download(
    api: &Api,
    path: &str,
    dest: &Path,
    expected_sha256: &str,
    cancel: &AtomicBool,
    on_bytes: &mut (dyn FnMut(u64) + Send),
) -> Result<u64, ApiError> {
    let url = api.url(path)?;
    let part = part_path(dest, expected_sha256);
    for stale in stale_parts(dest, &part) {
        let _ = tokio::fs::remove_file(stale).await;
    }
    // Without a hash a partial file cannot be told apart from another
    // content: never resumed.
    let mut may_resume = !expected_sha256.is_empty();
    loop {
        match fetch(
            api,
            &url,
            &part,
            dest,
            expected_sha256,
            cancel,
            on_bytes,
            may_resume,
        )
        .await
        {
            Ok(received) => {
                if let Err(e) = tokio::fs::rename(&part, dest).await {
                    // complete and checked: the next Get only renames it
                    return Err(ApiError::local(format!(
                        "{} could not be replaced (open in a CAD program?): {e}",
                        dest.display()
                    )));
                }
                return Ok(received);
            }
            Err(Failure::Restart) if may_resume => {
                let _ = tokio::fs::remove_file(&part).await;
                may_resume = false;
            }
            Err(Failure::Restart) => return Err(damaged(dest)),
            Err(Failure::Api(e)) => return Err(e),
        }
    }
}

/// One attempt; leaves the partial file in place unless its content is wrong.
#[allow(clippy::too_many_arguments)]
async fn fetch(
    api: &Api,
    url: &str,
    part: &Path,
    dest: &Path,
    expected_sha256: &str,
    cancel: &AtomicBool,
    on_bytes: &mut (dyn FnMut(u64) + Send),
    may_resume: bool,
) -> Result<u64, Failure> {
    let have = if may_resume {
        tokio::fs::metadata(part)
            .await
            .map(|m| m.len())
            .unwrap_or(0)
    } else {
        0
    };
    let mut request = api.request(Method::GET, url);
    if have > 0 {
        request = request
            .header(header::RANGE, format!("bytes={have}-"))
            .header(header::IF_RANGE, format!("\"{expected_sha256}\""));
    }
    let resp = request.send().await?;
    let status = resp.status();
    if have > 0 && status == StatusCode::RANGE_NOT_SATISFIABLE {
        // the partial file holds every byte already
        on_bytes(have);
        let sha = hex::encode(hash_file(part).await?.finalize());
        return if sha.eq_ignore_ascii_case(expected_sha256) {
            Ok(0)
        } else {
            Err(Failure::Restart)
        };
    }
    let mut resp = ensure_ok(resp).await?;
    let resumed = have > 0
        && status == StatusCode::PARTIAL_CONTENT
        && resp
            .headers()
            .get(header::CONTENT_RANGE)
            .and_then(|v| v.to_str().ok())
            .and_then(range_start)
            == Some(have);
    if status == StatusCode::PARTIAL_CONTENT && !resumed {
        // a range we did not ask for: never mix it into the file
        return Err(Failure::Api(ApiError::local(
            "The server answered with an unexpected part of the file.",
        )));
    }
    let (mut file, mut hasher) = if resumed {
        let hasher = hash_file(part).await?;
        let file = tokio::fs::OpenOptions::new()
            .append(true)
            .open(part)
            .await?;
        on_bytes(have);
        (file, hasher)
    } else {
        // the whole file (fresh, or the content changed): start over
        (tokio::fs::File::create(part).await?, Sha256::new())
    };
    let mut received = 0u64;
    let streamed: Result<(), Failure> = async {
        while let Some(chunk) = resp.chunk().await? {
            if cancel.load(Ordering::Relaxed) {
                return Err(Failure::Api(ApiError::local("Cancelled.")));
            }
            file.write_all(&chunk).await?;
            hasher.update(&chunk);
            received += chunk.len() as u64;
            on_bytes(chunk.len() as u64);
        }
        Ok(())
    }
    .await;
    // what arrived stays on disk for the next attempt
    file.flush().await?;
    drop(file);
    streamed?;
    let sha = hex::encode(hasher.finalize());
    if !expected_sha256.is_empty() && !sha.eq_ignore_ascii_case(expected_sha256) {
        let _ = tokio::fs::remove_file(part).await;
        return if resumed {
            Err(Failure::Restart)
        } else {
            Err(Failure::Api(damaged(dest)))
        };
    }
    Ok(received)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};
    use tokio::io::AsyncWriteExt;

    #[test]
    fn partial_file_names() {
        let dest = Path::new("/ws/ROCKET/frame.iam");
        assert_eq!(
            part_path(dest, "ABCDEF0123456789ffff"),
            PathBuf::from("/ws/ROCKET/frame.iam.abcdef0123456789.teamcraft-download")
        );
        assert_eq!(
            part_path(dest, ""),
            PathBuf::from("/ws/ROCKET/frame.iam.teamcraft-download")
        );
        assert_eq!(range_start("bytes 400-999/1000"), Some(400));
        assert_eq!(range_start("bytes */1000"), None);
        assert_eq!(range_start("items 1-2/3"), None);
    }

    #[test]
    fn stale_partial_files_of_the_same_file_only() {
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("frame.iam");
        let keep = part_path(&dest, "aaaa");
        for name in [
            "frame.iam.bbbb.teamcraft-download",
            "frame.iam.teamcraft-download",
            "frame.iam.aaaa.teamcraft-download",
            "frame.iam.bak",
            "frame.ipt.cccc.teamcraft-download",
        ] {
            std::fs::write(dir.path().join(name), b"x").unwrap();
        }
        let mut stale: Vec<String> = stale_parts(&dest, &keep)
            .iter()
            .map(|p| p.file_name().unwrap().to_string_lossy().to_string())
            .collect();
        stale.sort();
        assert_eq!(
            stale,
            vec![
                "frame.iam.bbbb.teamcraft-download",
                "frame.iam.teamcraft-download"
            ]
        );
    }

    // ------------------------------------------------------------------
    // Against a local server that answers ranges like TeamCraft
    // ------------------------------------------------------------------

    #[derive(Default)]
    struct Seen {
        ranges: Vec<Option<String>>,
    }

    /// Serves `content` (ETag = its SHA-256) with Range/If-Range; after
    /// `cut` bytes of a whole-file answer it drops the connection once.
    async fn serve(content: Vec<u8>, cut: Option<usize>, seen: Arc<Mutex<Seen>>) -> String {
        use tokio::io::AsyncReadExt as _;
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let etag = format!("\"{}\"", hex::encode(Sha256::digest(&content)));
        let cut = Arc::new(Mutex::new(cut));
        tokio::spawn(async move {
            while let Ok((mut sock, _)) = listener.accept().await {
                let (content, etag, seen, cut) =
                    (content.clone(), etag.clone(), seen.clone(), cut.clone());
                tokio::spawn(async move {
                    let mut head = Vec::new();
                    let mut buf = [0u8; 4096];
                    while !head.windows(4).any(|w| w == b"\r\n\r\n") {
                        let n = sock.read(&mut buf).await.unwrap_or(0);
                        if n == 0 {
                            return;
                        }
                        head.extend_from_slice(&buf[..n]);
                    }
                    let head = String::from_utf8_lossy(&head).to_ascii_lowercase();
                    let value = |name: &str| {
                        head.lines()
                            .find_map(|l| l.strip_prefix(&format!("{name}:")))
                            .map(|v| v.trim().to_string())
                    };
                    let range = value("range");
                    seen.lock().unwrap().ranges.push(range.clone());
                    let start = match (&range, value("if-range")) {
                        (Some(r), Some(v)) if v == etag => r
                            .strip_prefix("bytes=")
                            .and_then(|r| r.trim_end_matches('-').parse::<usize>().ok()),
                        _ => None,
                    };
                    let total = content.len();
                    let reply = match start {
                        Some(s) if s >= total => {
                            format!("HTTP/1.1 416 Range Not Satisfiable\r\nContent-Range: bytes */{total}\r\nContent-Length: 0\r\n\r\n").into_bytes()
                        }
                        Some(s) => {
                            let mut r = format!("HTTP/1.1 206 Partial Content\r\nContent-Range: bytes {s}-{}/{total}\r\nContent-Length: {}\r\nETag: {etag}\r\n\r\n", total - 1, total - s).into_bytes();
                            r.extend_from_slice(&content[s..]);
                            r
                        }
                        None => {
                            let mut r = format!("HTTP/1.1 200 OK\r\nContent-Length: {total}\r\nETag: {etag}\r\n\r\n").into_bytes();
                            match cut.lock().unwrap().take() {
                                Some(c) => r.extend_from_slice(&content[..c]),
                                None => r.extend_from_slice(&content),
                            }
                            r
                        }
                    };
                    let _ = sock.write_all(&reply).await;
                    let _ = sock.shutdown().await;
                });
            }
        });
        format!("http://{addr}")
    }

    fn api_at(base: String) -> Api {
        Api {
            client: crate::api::client(),
            base,
            token: Some("test".into()),
        }
    }

    fn content(n: usize) -> Vec<u8> {
        (0..n).map(|i| (i * 7 % 251) as u8).collect()
    }

    #[tokio::test]
    async fn an_interrupted_file_continues_where_it_stopped() {
        let data = content(300_000);
        let sha = hex::encode(Sha256::digest(&data));
        let seen = Arc::new(Mutex::new(Seen::default()));
        let base = serve(data.clone(), Some(120_000), seen.clone()).await;
        let api = api_at(base);
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("frame.iam");
        let cancel = AtomicBool::new(false);
        let mut total = 0u64;
        // first attempt: the connection drops after 120 000 bytes
        let first = download(&api, "/files/1/content", &dest, &sha, &cancel, &mut |n| {
            total += n
        })
        .await;
        assert!(first.is_err(), "the cut is an error");
        let part = part_path(&dest, &sha);
        assert_eq!(std::fs::metadata(&part).unwrap().len(), 120_000, "kept");
        assert!(!dest.exists());
        // second attempt: only the rest, then checked and renamed
        let mut shown = 0u64;
        let received = download(&api, "/files/1/content", &dest, &sha, &cancel, &mut |n| {
            shown += n
        })
        .await
        .unwrap();
        assert_eq!(received, 180_000, "only the rest was transferred");
        assert_eq!(shown, 300_000, "progress counts the bytes already there");
        assert_eq!(std::fs::read(&dest).unwrap(), data);
        assert!(!part.exists());
        let ranges = seen.lock().unwrap().ranges.clone();
        assert_eq!(ranges, vec![None, Some("bytes=120000-".to_string())]);
    }

    #[tokio::test]
    async fn a_cancelled_file_keeps_its_part() {
        let data = content(200_000);
        let sha = hex::encode(Sha256::digest(&data));
        let seen = Arc::new(Mutex::new(Seen::default()));
        let api = api_at(serve(data.clone(), None, seen).await);
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("hub.step");
        let cancel = AtomicBool::new(false);
        let mut seen_bytes = 0u64;
        let err = download(&api, "/f", &dest, &sha, &cancel, &mut |n| {
            seen_bytes += n;
            if seen_bytes > 0 {
                cancel.store(true, Ordering::Relaxed);
            }
        })
        .await
        .unwrap_err();
        assert_eq!(err.message, "Cancelled.");
        assert!(
            part_path(&dest, &sha).exists(),
            "the part stays for the next Get"
        );
        cancel.store(false, Ordering::Relaxed);
        download(&api, "/f", &dest, &sha, &cancel, &mut |_| {})
            .await
            .unwrap();
        assert_eq!(std::fs::read(&dest).unwrap(), data);
    }

    #[tokio::test]
    async fn a_wrong_part_is_started_over_and_a_complete_one_only_renamed() {
        let data = content(50_000);
        let sha = hex::encode(Sha256::digest(&data));
        let seen = Arc::new(Mutex::new(Seen::default()));
        let api = api_at(serve(data.clone(), None, seen.clone()).await);
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("plate.ipt");
        let part = part_path(&dest, &sha);
        let cancel = AtomicBool::new(false);
        // a part with wrong bytes (e.g. written by something else): the
        // resumed file does not match, so it is fetched once more completely
        let mut f = tokio::fs::File::create(&part).await.unwrap();
        f.write_all(&vec![0u8; 10_000]).await.unwrap();
        drop(f);
        download(&api, "/f", &dest, &sha, &cancel, &mut |_| {})
            .await
            .unwrap();
        assert_eq!(std::fs::read(&dest).unwrap(), data);
        assert_eq!(
            seen.lock().unwrap().ranges,
            vec![Some("bytes=10000-".to_string()), None]
        );
        // a complete part (the rename failed last time): 416, checked, renamed
        std::fs::remove_file(&dest).unwrap();
        std::fs::write(&part, &data).unwrap();
        let received = download(&api, "/f", &dest, &sha, &cancel, &mut |_| {})
            .await
            .unwrap();
        assert_eq!(received, 0, "nothing transferred");
        assert_eq!(std::fs::read(&dest).unwrap(), data);
        // a part of an older content of the same file is removed first
        let old = part_path(&dest, "0123456789abcdef0000");
        std::fs::write(&old, b"old").unwrap();
        download(&api, "/f", &dest, &sha, &cancel, &mut |_| {})
            .await
            .unwrap();
        assert!(!old.exists());
    }

    #[tokio::test]
    async fn a_changed_file_is_sent_whole() {
        // the server's content is not what the part belongs to: If-Range
        // fails, the server sends everything, the part is started over
        let data = content(40_000);
        let sha = hex::encode(Sha256::digest(&data));
        let seen = Arc::new(Mutex::new(Seen::default()));
        let api = api_at(serve(data.clone(), None, seen.clone()).await);
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("bolt.ipt");
        let other = hex::encode(Sha256::digest(b"another version"));
        std::fs::write(part_path(&dest, &other), b"another ver").unwrap();
        // asked for the other content: the server ignores the range
        let cancel = AtomicBool::new(false);
        let err = download(&api, "/f", &dest, &other, &cancel, &mut |_| {})
            .await
            .unwrap_err();
        assert!(err.message.contains("damaged"), "{}", err.message);
        assert!(!part_path(&dest, &other).exists());
        assert_eq!(seen.lock().unwrap().ranges[0].as_deref(), Some("bytes=11-"));
        // the expected content resumes nothing and downloads fine
        download(&api, "/f", &dest, &sha, &cancel, &mut |_| {})
            .await
            .unwrap();
        assert_eq!(std::fs::read(&dest).unwrap(), data);
    }
}
