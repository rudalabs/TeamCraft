//! Long operations the user can stop half-way with *Cancel* in their
//! progress dialog: the upload of a check-in and the Pack and Go download.
//!
//! * **Check-in.** Large files go first as chunked uploads; Cancel stops them
//!   before the next chunk and discards the upload sessions on the server
//!   (`DELETE /uploads/{id}`, not resumable later: a cancel is a decision,
//!   and the local files may change before the next attempt – network
//!   errors are still resumed automatically within one check-in). The small
//!   files travel inside one multipart request whose body is streamed from
//!   the files ([`FileStream`]): Cancel makes the stream fail, so the request
//!   breaks off before the server has the whole body. The server reads the
//!   whole body before it stores anything in one transaction, so nothing is
//!   stored and the revision stays checked out by the user. Once the last
//!   byte is on its way the check-in can no longer be stopped; the dialog
//!   says "finishing" and reports the result.
//! * **Pack and Go.** The ZIP is written next to the target
//!   (`<name>.teamcraft-download`) and renamed only when it is complete.
//!   Cancel – also while the server is still packing – drops the request
//!   and removes the partial file ([`until_cancelled`]).

use std::{
    future::Future,
    pin::Pin,
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Arc,
    },
    task::{Context, Poll},
    time::Duration,
};

use tokio::io::{AsyncRead, ReadBuf};

use crate::api::ApiError;

/// HTTP-like status of an operation the user cancelled (nginx's "client
/// closed request"): the user interface shows it as a note, not as an error.
pub const CANCELLED_STATUS: u16 = 499;

/// How often a running operation looks at its cancel switch.
const POLL: Duration = Duration::from_millis(100);

/// Bytes read per piece of a streamed request body.
const PIECE: usize = 256 * 1024;

pub fn cancelled(message: impl Into<String>) -> ApiError {
    ApiError {
        status: CANCELLED_STATUS,
        message: message.into(),
    }
}

#[cfg(test)]
fn is_cancelled(e: &ApiError) -> bool {
    e.status == CANCELLED_STATUS
}

/// Shared between the streamed body of one request and its watcher.
#[derive(Debug, Default)]
pub struct Progress {
    /// Set by the watcher: the body fails at its next piece.
    pub stop: AtomicBool,
    /// Bytes the body has handed to the HTTP client so far.
    pub sent: AtomicU64,
}

/// A file as a request body that can be stopped between two pieces.
pub struct FileStream<R> {
    source: R,
    buf: Box<[u8]>,
    progress: Arc<Progress>,
}

impl<R> FileStream<R> {
    pub fn new(source: R, progress: Arc<Progress>) -> FileStream<R> {
        FileStream {
            source,
            buf: vec![0u8; PIECE].into_boxed_slice(),
            progress,
        }
    }
}

impl<R: AsyncRead + Unpin> futures_core::Stream for FileStream<R> {
    type Item = std::io::Result<Vec<u8>>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let this = self.get_mut();
        if this.progress.stop.load(Ordering::Relaxed) {
            return Poll::Ready(Some(Err(std::io::Error::new(
                std::io::ErrorKind::Interrupted,
                "cancelled",
            ))));
        }
        let mut read = ReadBuf::new(&mut this.buf);
        match Pin::new(&mut this.source).poll_read(cx, &mut read) {
            Poll::Pending => Poll::Pending,
            Poll::Ready(Err(e)) => Poll::Ready(Some(Err(e))),
            Poll::Ready(Ok(())) => {
                let piece = read.filled();
                if piece.is_empty() {
                    Poll::Ready(None)
                } else {
                    this.progress
                        .sent
                        .fetch_add(piece.len() as u64, Ordering::Relaxed);
                    Poll::Ready(Some(Ok(piece.to_vec())))
                }
            }
        }
    }
}

/// Runs a request whose body streams `total` bytes through `progress`:
/// reports the bytes sent every [`POLL`] and passes a cancel on to the body
/// while bytes are still to be sent. Afterwards (the server has the whole
/// body) a cancel no longer has any effect.
pub async fn watch_upload<F: Future>(
    request: F,
    progress: &Progress,
    total: u64,
    cancel: &AtomicBool,
    report: &(dyn Fn(u64, u64) + Send + Sync),
) -> F::Output {
    tokio::pin!(request);
    let mut tick = tokio::time::interval(POLL);
    loop {
        tokio::select! {
            out = &mut request => {
                report(progress.sent.load(Ordering::Relaxed).min(total), total);
                return out;
            }
            _ = tick.tick() => {
                let sent = progress.sent.load(Ordering::Relaxed).min(total);
                if sent < total && cancel.load(Ordering::Relaxed) {
                    progress.stop.store(true, Ordering::Relaxed);
                }
                report(sent, total);
            }
        }
    }
}

/// Runs `work` until it ends or `cancel` is set; a cancel drops it at
/// whatever step it is in (waiting for the server, receiving) and answers
/// `on_cancel`.
pub async fn until_cancelled<T, F>(
    work: F,
    cancel: &AtomicBool,
    on_cancel: &str,
) -> Result<T, ApiError>
where
    F: Future<Output = Result<T, ApiError>>,
{
    tokio::pin!(work);
    let mut tick = tokio::time::interval(POLL);
    loop {
        tokio::select! {
            out = &mut work => return out,
            _ = tick.tick() => {
                if cancel.load(Ordering::Relaxed) {
                    return Err(cancelled(on_cancel));
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Collects a stream the way the HTTP client does: piece by piece.
    async fn drain<R: AsyncRead + Unpin>(
        stream: &mut FileStream<R>,
        stop_after: Option<(usize, &Progress)>,
    ) -> (Vec<u8>, Option<std::io::Error>) {
        let mut out = Vec::new();
        let mut pieces = 0;
        loop {
            let next = std::future::poll_fn(|cx| Pin::new(&mut *stream).poll_next(cx)).await;
            match next {
                None => return (out, None),
                Some(Err(e)) => return (out, Some(e)),
                Some(Ok(piece)) => {
                    out.extend_from_slice(&piece);
                    pieces += 1;
                    if let Some((n, p)) = stop_after {
                        if pieces == n {
                            p.stop.store(true, Ordering::Relaxed);
                        }
                    }
                }
            }
        }
    }

    use futures_core::Stream;

    #[tokio::test]
    async fn a_file_streams_completely_and_counts_its_bytes() {
        let data: Vec<u8> = (0..(PIECE * 2 + 17)).map(|i| (i % 251) as u8).collect();
        let progress = Arc::new(Progress::default());
        let mut stream = FileStream::new(std::io::Cursor::new(data.clone()), progress.clone());
        let (out, err) = drain(&mut stream, None).await;
        assert!(err.is_none());
        assert_eq!(out, data);
        assert_eq!(progress.sent.load(Ordering::Relaxed), data.len() as u64);
    }

    #[tokio::test]
    async fn a_stopped_stream_fails_before_the_rest_is_read() {
        let data = vec![7u8; PIECE * 4];
        let progress = Arc::new(Progress::default());
        let mut stream = FileStream::new(std::io::Cursor::new(data), progress.clone());
        let (out, err) = drain(&mut stream, Some((1, &progress))).await;
        let err = err.expect("the body breaks off");
        assert_eq!(err.kind(), std::io::ErrorKind::Interrupted);
        assert_eq!(out.len(), PIECE, "only the first piece left");
        assert_eq!(progress.sent.load(Ordering::Relaxed), PIECE as u64);
    }

    #[tokio::test]
    async fn the_watcher_stops_the_body_only_while_bytes_are_left() {
        let report = |_: u64, _: u64| {};
        // bytes left: the cancel reaches the body
        let progress = Progress::default();
        progress.sent.store(10, Ordering::Relaxed);
        let cancel = AtomicBool::new(true);
        let out = watch_upload(
            async {
                tokio::time::sleep(Duration::from_millis(350)).await;
                "answer"
            },
            &progress,
            100,
            &cancel,
            &report,
        )
        .await;
        assert_eq!(out, "answer", "the request itself decides how it ends");
        assert!(progress.stop.load(Ordering::Relaxed));
        // everything sent: too late, the server finishes the check-in
        let progress = Progress::default();
        progress.sent.store(100, Ordering::Relaxed);
        watch_upload(
            tokio::time::sleep(Duration::from_millis(350)),
            &progress,
            100,
            &cancel,
            &report,
        )
        .await;
        assert!(!progress.stop.load(Ordering::Relaxed));
    }

    // ------------------------------------------------------------------
    // Against a local HTTP server
    // ------------------------------------------------------------------

    use crate::api::{Api, Transfer};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    fn api_at(addr: std::net::SocketAddr) -> Api {
        Api {
            client: crate::api::client(),
            base: format!("http://{addr}"),
            token: Some("test".into()),
        }
    }

    /// Reads the request head; returns the bytes of the body read with it.
    async fn read_head(sock: &mut tokio::net::TcpStream) -> Vec<u8> {
        let mut data = Vec::new();
        let mut buf = [0u8; 4096];
        loop {
            let n = sock.read(&mut buf).await.unwrap();
            assert!(n > 0, "request head");
            data.extend_from_slice(&buf[..n]);
            if let Some(end) = data.windows(4).position(|w| w == b"\r\n\r\n") {
                return data[end + 4..].to_vec();
            }
        }
    }

    #[tokio::test]
    async fn pack_and_go_leaves_no_file_when_cancelled() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        // a server that answers with part of a large ZIP and then keeps packing
        tokio::spawn(async move {
            while let Ok((mut sock, _)) = listener.accept().await {
                tokio::spawn(async move {
                    read_head(&mut sock).await;
                    let _ = sock
                        .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 10000000\r\n\r\n")
                        .await;
                    let _ = sock.write_all(&[1u8; 64 * 1024]).await;
                    tokio::time::sleep(Duration::from_secs(30)).await;
                });
            }
        });
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("ASM-000001_A.zip");
        let cancel = Arc::new(AtomicBool::new(false));
        let seen = Arc::new(AtomicU64::new(0));
        let (c, s) = (cancel.clone(), seen.clone());
        let on_bytes = move |received: u64, total: Option<u64>| {
            assert_eq!(total, Some(10_000_000));
            s.store(received, Ordering::Relaxed);
            if received > 0 {
                c.store(true, Ordering::Relaxed);
            }
        };
        let err = api_at(addr)
            .download_to(
                "/items/ASM-000001/revisions/A/package",
                &dest,
                &cancel,
                &on_bytes,
            )
            .await
            .unwrap_err();
        assert!(is_cancelled(&err), "{err:?}");
        assert!(seen.load(Ordering::Relaxed) > 0, "progress was reported");
        let left: Vec<_> = std::fs::read_dir(dir.path()).unwrap().collect();
        assert!(left.is_empty(), "no ZIP and no partial file: {left:?}");
    }

    #[tokio::test]
    async fn pack_and_go_writes_the_complete_file() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let (mut sock, _) = listener.accept().await.unwrap();
            read_head(&mut sock).await;
            sock.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 5\r\n\r\nhello")
                .await
                .unwrap();
        });
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("x.zip");
        let cancel = AtomicBool::new(false);
        let n = api_at(addr)
            .download_to("/package", &dest, &cancel, &|_, _| {})
            .await
            .unwrap();
        assert_eq!(n, 5);
        assert_eq!(std::fs::read(&dest).unwrap(), b"hello");
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
    }

    #[tokio::test]
    async fn a_cancelled_check_in_never_reaches_the_server_complete() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        // below the chunked limit: inside the request
        let size = 24 * 1024 * 1024;
        // a slow server: reads the body in small steps and reports how much
        // arrived before the connection closed (or answers once it has all)
        let (tx, rx) = tokio::sync::oneshot::channel();
        tokio::spawn(async move {
            let (mut sock, _) = listener.accept().await.unwrap();
            let mut got = read_head(&mut sock).await.len();
            let mut buf = [0u8; 16 * 1024];
            loop {
                tokio::time::sleep(Duration::from_millis(20)).await;
                match sock.read(&mut buf).await {
                    Ok(0) | Err(_) => break,
                    Ok(n) => got += n,
                }
                if got > size {
                    let _ = sock
                        .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\n{}")
                        .await;
                    break;
                }
            }
            let _ = tx.send(got);
        });
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("frame.iam");
        std::fs::write(&file, vec![3u8; size]).unwrap();
        let cancel = Arc::new(AtomicBool::new(false));
        let c = cancel.clone();
        let progress = move |name: &str, sent: u64, total: u64| {
            assert_eq!(name, "frame.iam");
            assert_eq!(total, size as u64);
            if sent > 0 {
                c.store(true, Ordering::Relaxed);
            }
        };
        let transfer = Transfer {
            cancel: &cancel,
            progress: &progress,
        };
        let err = api_at(addr)
            .send_files(
                "/items/PRT-1/revisions/A/checkin",
                "PRT-1",
                "A",
                &[("frame.iam".to_string(), file.clone())],
                &[("comment", "x".to_string())],
                &transfer,
            )
            .await
            .unwrap_err();
        assert!(is_cancelled(&err), "{err:?}");
        assert!(err.message.contains("still checked out"));
        let got = tokio::time::timeout(Duration::from_secs(10), rx)
            .await
            .expect("the server saw the connection end")
            .unwrap();
        assert!(got < size, "the body broke off: {got} of {size} bytes");
        assert_eq!(
            std::fs::read(&file).unwrap().len(),
            size,
            "local file untouched"
        );
    }

    #[tokio::test]
    async fn work_is_dropped_on_cancel() {
        let cancel = AtomicBool::new(false);
        let done = until_cancelled(async { Ok::<_, ApiError>(5) }, &cancel, "stopped").await;
        assert_eq!(done.unwrap(), 5);
        cancel.store(true, Ordering::Relaxed);
        let dropped = Arc::new(AtomicBool::new(false));
        struct Flag(Arc<AtomicBool>);
        impl Drop for Flag {
            fn drop(&mut self) {
                self.0.store(true, Ordering::Relaxed);
            }
        }
        let flag = Flag(dropped.clone());
        let err = until_cancelled(
            async move {
                let _flag = flag;
                tokio::time::sleep(Duration::from_secs(30)).await;
                Ok::<_, ApiError>(())
            },
            &cancel,
            "stopped",
        )
        .await
        .unwrap_err();
        assert!(is_cancelled(&err));
        assert_eq!(err.message, "stopped");
        assert!(dropped.load(Ordering::Relaxed), "the work was dropped");
    }
}
