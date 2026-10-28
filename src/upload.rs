//! Receiving the document: the request body is read as a stream of `multipart/form-data`,
//! the first file part named `document` is spooled to a per-conversion temporary directory, and the size
//! limit is enforced while the body arrives — a declared `Content-Length` over it is refused before a byte
//! is read (so `Expect: 100-continue` gets no `100`), and a body that crosses it, chunked or not, is refused
//! as soon as it does, without reading the rest.
//!
//! What is a client error: a body that is not multipart, a malformed one, or one without a `document` file part (a
//! plain form field named `document` is not a document; moalhaddar/docx-to-pdf answers it 400 too) is 400; a document
//! part over the limit, or a request over the limit plus [`MULTIPART_ALLOWANCE`], is 413.
//!
//! The size limit bounds the bytes received, not what a DOCX or ODT inflates to while LibreOffice loads it
//! (a 7.4 MiB DOCX wrote 1.5 GiB to `/tmp`). So once the body is read, each received DOCX or ODT
//! is counted ([`crate::allowlist::inflation`]), and one whose parts inflate past [`Upload::max_inflated_size`]
//! is 413 too, before any instance sees it.

use std::future::Future as _;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::task::{Context, Poll};
use std::time::Duration;

use axum::http::HeaderMap;
use axum::http::header::{CONTENT_DISPOSITION, CONTENT_LENGTH, CONTENT_TYPE};
use bytes::Bytes;
use futures_util::Stream;
use tempfile::TempDir;
use tokio::io::AsyncWriteExt;

use crate::problem::Problem;

/// What a request may carry beyond the document itself: the part headers, the boundaries and small extra
/// fields. The request limit is the document limit plus this, so a document exactly at the limit fits.
pub const MULTIPART_ALLOWANCE: u64 = 64 * 1024;

/// The detail of the 413 for a document whose parts inflate past the limit.
pub const INFLATED: &str = "the document's compressed parts inflate past the size limit";

/// The name of the part that carries the document, as upstream's clients send it.
pub const DOCUMENT_FIELD: &str = "document";

/// The file name of the spooled document inside its directory. The client's file name is ignored
/// (the format is decided by the bytes) and never reaches the file system.
const SPOOLED_NAME: &str = "document";

/// How documents are received: the size limit and where they are spooled.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Upload {
    /// The largest document accepted, in bytes (`DOCX_TO_PDF_MAX_DOCUMENT_SIZE`).
    pub max_document_size: u64,
    /// The directory under which each conversion gets its own temporary directory.
    pub spool_root: PathBuf,
    /// The longest the body may go without a byte (`DOCX_TO_PDF_IDLE_TIMEOUT`); past it, 408.
    pub idle_timeout: Duration,
    /// The most a received DOCX or ODT may inflate to, in bytes (`DOCX_TO_PDF_MAX_INFLATED_SIZE`); past it,
    /// 413. 0 = no limit.
    pub max_inflated_size: u64,
}

/// A document received whole: its file in a temporary directory of its own, removed when this is dropped,
/// on every path.
#[derive(Debug)]
pub struct ReceivedDocument {
    directory: TempDir,
    size: u64,
}

impl ReceivedDocument {
    /// The conversion's temporary directory; the document is in it.
    pub fn directory(&self) -> &Path {
        self.directory.path()
    }

    /// The spooled document.
    pub fn path(&self) -> PathBuf {
        self.directory.path().join(SPOOLED_NAME)
    }

    /// The document's size in bytes.
    pub fn size(&self) -> u64 {
        self.size
    }
}

/// Why no document was received.
#[derive(Debug)]
pub enum UploadError {
    /// The body is not `multipart/form-data` (no or another `Content-Type`, no boundary): 400.
    NotMultipart,
    /// The multipart body is malformed or ended early: 400.
    Malformed,
    /// No file part named `document`: 400.
    NoDocument,
    /// The document or the request is over the limit: 413.
    TooLarge,
    /// The document's parts inflate past [`Upload::max_inflated_size`]: 413.
    Inflated,
    /// No byte of the body arrived for [`Upload::idle_timeout`]: 408.
    IdleTimeout,
    /// The document could not be written to its temporary directory: 500.
    Spool(std::io::Error),
}

#[allow(
    clippy::use_self,
    reason = "tests/api_reference.rs reads the details from `Problem::<variant>(` in the source"
)]
impl From<UploadError> for Problem {
    fn from(error: UploadError) -> Self {
        match error {
            UploadError::NotMultipart => {
                Problem::BadRequest("the body is not multipart/form-data with a boundary")
            }
            UploadError::Malformed => Problem::BadRequest("the multipart body is malformed"),
            UploadError::NoDocument => {
                Problem::BadRequest("the body has no file part named \"document\"")
            }
            UploadError::TooLarge => {
                Problem::TooLarge("the document or the request is over the size limit")
            }
            UploadError::Inflated => Problem::TooLarge(INFLATED),
            UploadError::IdleTimeout => {
                Problem::RequestTimeout(crate::problem::IDLE_TIMEOUT_DETAIL)
            }
            UploadError::Spool(_) => Problem::Internal,
        }
    }
}

impl Upload {
    /// Documents up to `max_document_size` bytes, spooled under the system's temporary directory, inflating
    /// to at most the default for that size ([`crate::config::default_max_inflated_size`]).
    pub fn new(max_document_size: u64) -> Self {
        Self {
            max_document_size,
            spool_root: std::env::temp_dir(),
            idle_timeout: crate::config::DEFAULT_IDLE_TIMEOUT,
            max_inflated_size: crate::config::default_max_inflated_size(max_document_size),
        }
    }

    /// The largest request body accepted: the document limit plus [`MULTIPART_ALLOWANCE`].
    pub fn request_limit(&self) -> u64 {
        self.max_document_size.saturating_add(MULTIPART_ALLOWANCE)
    }

    /// Reads `body`, the request's, under `headers`, and spools its first `document` file part.
    pub async fn receive<S, E>(
        &self,
        headers: &HeaderMap,
        body: S,
    ) -> Result<ReceivedDocument, UploadError>
    where
        S: Stream<Item = Result<Bytes, E>> + Send,
        E: Into<Box<dyn std::error::Error + Send + Sync>>,
    {
        let mut received = self.receive_up_to(headers, body, 1).await?;
        Ok(received.remove(0).0)
    }

    /// Reads `body` and spools each of its `document` file parts, up to `max_documents` of them (the platform
    /// probe's), each in its own directory, with the part's file name if it has one (a label for
    /// the answer; it never reaches the file system). The request may be `max_documents` documents plus
    /// [`MULTIPART_ALLOWANCE`] long; a `document` part past `max_documents` is 413.
    pub async fn receive_all<S, E>(
        &self,
        headers: &HeaderMap,
        body: S,
        max_documents: usize,
    ) -> Result<Vec<(ReceivedDocument, Option<String>)>, UploadError>
    where
        S: Stream<Item = Result<Bytes, E>> + Send,
        E: Into<Box<dyn std::error::Error + Send + Sync>>,
    {
        self.receive_up_to(headers, body, max_documents).await
    }

    async fn receive_up_to<S, E>(
        &self,
        headers: &HeaderMap,
        body: S,
        max_documents: usize,
    ) -> Result<Vec<(ReceivedDocument, Option<String>)>, UploadError>
    where
        S: Stream<Item = Result<Bytes, E>> + Send,
        E: Into<Box<dyn std::error::Error + Send + Sync>>,
    {
        let content_type = headers
            .get(CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .ok_or(UploadError::NotMultipart)?;
        let boundary =
            multer::parse_boundary(content_type).map_err(|_| UploadError::NotMultipart)?;

        // A declared length over the limit is refused before the body is polled, so hyper never sends
        // `100 Continue` and the client never sends the body.
        let request_limit = self
            .max_document_size
            .saturating_mul(max_documents as u64)
            .saturating_add(MULTIPART_ALLOWANCE);
        let declared = headers
            .get(CONTENT_LENGTH)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.trim().parse::<u64>().ok());
        if declared.is_some_and(|length| length > request_limit) {
            return Err(UploadError::TooLarge);
        }

        let constraints = multer::Constraints::new().size_limit(
            multer::SizeLimit::new()
                .whole_stream(request_limit)
                .per_field(self.max_document_size),
        );
        let body = IdleLimited::new(body, self.idle_timeout);
        let mut multipart = multer::Multipart::with_constraints(body, boundary, constraints);

        let mut received = Vec::new();
        while let Some(mut field) = multipart.next_field().await.map_err(classify)? {
            if field.name() == Some(DOCUMENT_FIELD) && is_file_part(field.headers()) {
                if received.len() < max_documents {
                    let name = field.file_name().map(str::to_owned);
                    received.push((self.spool(&mut field).await?, name));
                } else if max_documents > 1 {
                    return Err(UploadError::TooLarge);
                }
            }
            // Any other part is skipped: the next `next_field` reads past it, under the same limits.
        }
        if received.is_empty() {
            return Err(UploadError::NoDocument);
        }
        for (document, _) in &received {
            self.check_inflation(document).await?;
        }
        Ok(received)
    }

    /// Refuses `document` when it is a DOCX or ODT whose parts inflate past [`Upload::max_inflated_size`].
    /// Any other document is the allowlist's to judge.
    async fn check_inflation(&self, document: &ReceivedDocument) -> Result<(), UploadError> {
        use crate::allowlist::Inflation;
        if self.max_inflated_size == 0 {
            return Ok(());
        }
        let inflation = crate::allowlist::inflation_file(&document.path(), self.max_inflated_size)
            .await
            .map_err(UploadError::Spool)?;
        let (declared, inflated) = match inflation {
            Inflation::DeclaredPast(bytes) => (bytes, 0),
            Inflation::InflatedPast(bytes) => (0, bytes),
            Inflation::NotCounted | Inflation::Within(_) => return Ok(()),
        };
        tracing::warn!(
            reason = "inflated",
            document_size_bytes = document.size(),
            declared_bytes = declared,
            inflated_bytes = inflated,
            max_inflated_size_bytes = self.max_inflated_size,
            "the document's parts inflate past the limit; refused before conversion"
        );
        Err(UploadError::Inflated)
    }

    /// Writes `field` chunk by chunk into a new temporary directory. On any error the directory is
    /// dropped, and with it the partial file.
    async fn spool(&self, field: &mut multer::Field<'_>) -> Result<ReceivedDocument, UploadError> {
        let directory = tempfile::Builder::new()
            .prefix("docx-to-pdf-")
            .tempdir_in(&self.spool_root)
            .map_err(UploadError::Spool)?;
        let mut file = tokio::fs::File::create(directory.path().join(SPOOLED_NAME))
            .await
            .map_err(UploadError::Spool)?;
        let mut size = 0u64;
        while let Some(chunk) = field.chunk().await.map_err(classify)? {
            size += chunk.len() as u64;
            file.write_all(&chunk).await.map_err(UploadError::Spool)?;
        }
        file.flush().await.map_err(UploadError::Spool)?;
        Ok(ReceivedDocument { directory, size })
    }
}

/// The client error a multipart failure is.
fn classify(error: multer::Error) -> UploadError {
    match error {
        multer::Error::StreamReadFailed(e) if e.is::<IdleTimedOut>() => UploadError::IdleTimeout,
        multer::Error::FieldSizeExceeded { .. } | multer::Error::StreamSizeExceeded { .. } => {
            UploadError::TooLarge
        }
        multer::Error::NoMultipart
        | multer::Error::NoBoundary
        | multer::Error::DecodeContentType(_) => UploadError::NotMultipart,
        _ => UploadError::Malformed,
    }
}

/// The body stream, failing with [`IdleTimedOut`] once no chunk has arrived for `limit`. The clock restarts
/// at each chunk and counts only while the stream is waited on and has nothing: a pause shorter than the
/// limit is waited for, however long the whole body takes, since upstream's 30 ms read timeout failed about
/// 5 % of real uploads.
struct IdleLimited<S> {
    inner: Pin<Box<S>>,
    limit: Duration,
    idle: Pin<Box<tokio::time::Sleep>>,
}

impl<S> IdleLimited<S> {
    fn new(inner: S, limit: Duration) -> Self {
        Self {
            inner: Box::pin(inner),
            limit,
            idle: Box::pin(tokio::time::sleep(limit)),
        }
    }
}

impl<S, E> Stream for IdleLimited<S>
where
    S: Stream<Item = Result<Bytes, E>>,
    E: Into<Box<dyn std::error::Error + Send + Sync>>,
{
    type Item = Result<Bytes, Box<dyn std::error::Error + Send + Sync>>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let this = &mut *self;
        match this.inner.as_mut().poll_next(cx) {
            Poll::Ready(item) => {
                let deadline = tokio::time::Instant::now() + this.limit;
                this.idle.as_mut().reset(deadline);
                Poll::Ready(item.map(|chunk| chunk.map_err(Into::into)))
            }
            Poll::Pending => match this.idle.as_mut().poll(cx) {
                Poll::Ready(()) => Poll::Ready(Some(Err(Box::new(IdleTimedOut)))),
                Poll::Pending => Poll::Pending,
            },
        }
    }
}

/// The body went without a byte for the idle limit.
#[derive(Debug)]
struct IdleTimedOut;

impl std::fmt::Display for IdleTimedOut {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("no byte of the body arrived within the idle limit")
    }
}

impl std::error::Error for IdleTimedOut {}

/// Whether a part is a file part: its `Content-Disposition` has a `filename` or `filename*` parameter
/// (RFC 7578 §4.2), whatever its value or encoding. A part without one is a plain form field.
fn is_file_part(headers: &HeaderMap) -> bool {
    let Some(disposition) = headers.get(CONTENT_DISPOSITION) else {
        return false;
    };
    disposition
        .as_bytes()
        .split(|&b| b == b';')
        .skip(1)
        .any(|parameter| {
            let name = parameter.split(|&b| b == b'=').next().unwrap_or_default();
            let name = name.trim_ascii();
            name.eq_ignore_ascii_case(b"filename") || name.eq_ignore_ascii_case(b"filename*")
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures_util::stream;
    use std::convert::Infallible;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU64, Ordering};

    const BOUNDARY: &str = "X-BOUNDARY-0011";

    /// A part of a multipart body: a file part when `filename` is given, else a plain field.
    struct Part<'a> {
        name: &'a str,
        filename: Option<&'a str>,
        content: &'a [u8],
    }

    fn file<'a>(name: &'a str, filename: &'a str, content: &'a [u8]) -> Part<'a> {
        Part {
            name,
            filename: Some(filename),
            content,
        }
    }

    fn field<'a>(name: &'a str, content: &'a [u8]) -> Part<'a> {
        Part {
            name,
            filename: None,
            content,
        }
    }

    fn multipart(parts: &[Part<'_>]) -> Vec<u8> {
        let mut body = Vec::new();
        for part in parts {
            body.extend_from_slice(format!("--{BOUNDARY}\r\n").as_bytes());
            match part.filename {
                Some(filename) => {
                    body.extend_from_slice(
                        format!(
                            "Content-Disposition: form-data; name=\"{}\"; filename=\"{filename}\"\r\n\
                             Content-Type: application/octet-stream\r\n\r\n",
                            part.name
                        )
                        .as_bytes(),
                    );
                }
                None => body.extend_from_slice(
                    format!(
                        "Content-Disposition: form-data; name=\"{}\"\r\n\r\n",
                        part.name
                    )
                    .as_bytes(),
                ),
            }
            body.extend_from_slice(part.content);
            body.extend_from_slice(b"\r\n");
        }
        body.extend_from_slice(format!("--{BOUNDARY}--\r\n").as_bytes());
        body
    }

    fn headers(content_type: Option<&str>, content_length: Option<u64>) -> HeaderMap {
        let mut headers = HeaderMap::new();
        if let Some(t) = content_type {
            headers.insert(CONTENT_TYPE, t.parse().unwrap());
        }
        if let Some(n) = content_length {
            headers.insert(CONTENT_LENGTH, n.into());
        }
        headers
    }

    fn form() -> String {
        format!("multipart/form-data; boundary={BOUNDARY}")
    }

    /// The body in chunks of `chunk` bytes, as a network would deliver it.
    #[allow(
        clippy::needless_pass_by_value,
        reason = "a test helper that takes what its callers build in place"
    )]
    fn chunks(body: Vec<u8>, chunk: usize) -> impl Stream<Item = Result<Bytes, Infallible>> + Send {
        let pieces: Vec<Result<Bytes, Infallible>> = body
            .chunks(chunk.max(1))
            .map(|c| Ok(Bytes::copy_from_slice(c)))
            .collect();
        stream::iter(pieces)
    }

    struct Fixture {
        root: TempDir,
        upload: Upload,
    }

    fn fixture(max_document_size: u64) -> Fixture {
        let root = tempfile::tempdir().unwrap();
        let upload = Upload {
            max_document_size,
            spool_root: root.path().to_owned(),
            idle_timeout: crate::config::DEFAULT_IDLE_TIMEOUT,
            max_inflated_size: crate::config::default_max_inflated_size(max_document_size),
        };
        Fixture { root, upload }
    }

    impl Fixture {
        async fn receive(&self, body: Vec<u8>) -> Result<ReceivedDocument, UploadError> {
            let length = body.len() as u64;
            self.upload
                .receive(&headers(Some(&form()), Some(length)), chunks(body, 7))
                .await
        }

        fn spooled_directories(&self) -> usize {
            std::fs::read_dir(self.root.path()).unwrap().count()
        }
    }

    #[tokio::test]
    async fn the_document_part_is_spooled_to_a_directory_of_its_own() {
        let f = fixture(1024);
        let document = f
            .receive(multipart(&[file(
                "document",
                "input.docx",
                b"PK\x03\x04 bytes",
            )]))
            .await
            .unwrap();
        assert_eq!(std::fs::read(document.path()).unwrap(), b"PK\x03\x04 bytes");
        assert_eq!(document.size(), 10);
        assert!(document.path().starts_with(document.directory()));
        assert!(document.directory().starts_with(f.root.path()));
        assert_eq!(f.spooled_directories(), 1);
        drop(document);
        assert_eq!(
            f.spooled_directories(),
            0,
            "the directory goes with the document"
        );
    }

    #[tokio::test]
    async fn the_first_document_part_wins_and_other_fields_are_ignored() {
        let f = fixture(1024);
        let document = f
            .receive(multipart(&[
                field("other", b"x"),
                file("attachment", "a.docx", b"not this"),
                file("document", "ex1.docx", b"first"),
                file("document", "ex2.docx", b"second"),
            ]))
            .await
            .unwrap();
        assert_eq!(std::fs::read(document.path()).unwrap(), b"first");
    }

    #[tokio::test]
    async fn the_file_name_plays_no_part_non_ascii_included() {
        let f = fixture(1024);
        for name in ["Отчёт.docx", "../../etc/passwd", "a;b.docx", ""] {
            let document = f
                .receive(multipart(&[file("document", name, b"bytes")]))
                .await
                .unwrap_or_else(|e| panic!("{name:?}: {e:?}"));
            assert_eq!(document.path().file_name().unwrap(), "document");
            assert_eq!(std::fs::read(document.path()).unwrap(), b"bytes");
        }
    }

    #[tokio::test]
    async fn an_rfc_5987_file_name_makes_a_file_part_too() {
        let f = fixture(1024);
        let body = format!(
            "--{BOUNDARY}\r\nContent-Disposition: form-data; name=\"document\"; \
             filename*=UTF-8''%D0%9E%D1%82%D1%87%D1%91%D1%82.docx\r\n\r\nbytes\r\n--{BOUNDARY}--\r\n"
        );
        let document = f.receive(body.into_bytes()).await.unwrap();
        assert_eq!(std::fs::read(document.path()).unwrap(), b"bytes");
    }

    #[tokio::test]
    async fn an_empty_document_is_received_the_allowlist_judges_it_later() {
        let f = fixture(1024);
        let document = f
            .receive(multipart(&[file("document", "empty.docx", b"")]))
            .await
            .unwrap();
        assert_eq!(document.size(), 0);
    }

    #[tokio::test]
    async fn a_body_that_is_not_multipart_is_refused() {
        let f = fixture(1024);
        for content_type in [
            None,
            Some("application/json"),
            Some("multipart/form-data"),
            Some("multipart/mixed; boundary=x"),
            Some("not a type"),
        ] {
            let result = f
                .upload
                .receive(&headers(content_type, None), chunks(b"{}".to_vec(), 7))
                .await;
            assert!(
                matches!(result, Err(UploadError::NotMultipart)),
                "{content_type:?}: {result:?}"
            );
        }
    }

    #[tokio::test]
    async fn a_malformed_body_is_refused() {
        let f = fixture(1024);
        let whole = multipart(&[file("document", "a.docx", b"bytes")]);
        for body in [
            Vec::new(),
            b"garbage without a boundary".to_vec(),
            whole[..whole.len() - 20].to_vec(),
        ] {
            let result = f.receive(body.clone()).await;
            assert!(
                matches!(result, Err(UploadError::Malformed)),
                "{:?}: {result:?}",
                String::from_utf8_lossy(&body)
            );
        }
        assert_eq!(f.spooled_directories(), 0);
    }

    #[tokio::test]
    async fn a_malformed_part_after_the_document_is_still_malformed() {
        let f = fixture(1024);
        let mut body = multipart(&[file("document", "a.docx", b"bytes")]);
        body.truncate(body.len() - format!("--{BOUNDARY}--\r\n").len());
        body.extend_from_slice(format!("--{BOUNDARY}\r\nno colon here\r\n\r\nx").as_bytes());
        let result = f.receive(body).await;
        assert!(matches!(result, Err(UploadError::Malformed)), "{result:?}");
        assert_eq!(f.spooled_directories(), 0);
    }

    #[tokio::test]
    async fn no_document_part_is_refused() {
        let f = fixture(1024);
        for body in [
            multipart(&[file("file", "input.docx", b"bytes")]),
            multipart(&[field("other", b"x")]),
            multipart(&[]),
        ] {
            let result = f.receive(body).await;
            assert!(matches!(result, Err(UploadError::NoDocument)), "{result:?}");
        }
    }

    #[tokio::test]
    async fn document_as_a_plain_field_is_not_a_document() {
        let f = fixture(1024);
        let result = f.receive(multipart(&[field("document", b"hello")])).await;
        assert!(matches!(result, Err(UploadError::NoDocument)), "{result:?}");

        // A file part after it is the document, as Spring takes the file part.
        let document = f
            .receive(multipart(&[
                field("document", b"hello"),
                file("document", "a.docx", b"bytes"),
            ]))
            .await
            .unwrap();
        assert_eq!(std::fs::read(document.path()).unwrap(), b"bytes");
    }

    #[tokio::test]
    async fn a_document_at_the_limit_is_received() {
        let f = fixture(1000);
        let content = vec![b'a'; 1000];
        let document = f
            .receive(multipart(&[
                field("other", b"x"),
                file("document", "a.docx", &content),
            ]))
            .await
            .unwrap();
        assert_eq!(document.size(), 1000);
    }

    #[tokio::test]
    async fn a_document_one_byte_over_the_limit_is_too_large() {
        let f = fixture(1000);
        let content = vec![b'a'; 1001];
        let result = f
            .receive(multipart(&[file("document", "a.docx", &content)]))
            .await;
        assert!(matches!(result, Err(UploadError::TooLarge)), "{result:?}");
        assert_eq!(f.spooled_directories(), 0, "the partial spool is removed");
    }

    #[tokio::test]
    async fn another_part_over_the_limit_makes_the_request_too_large() {
        let f = fixture(1000);
        let big = vec![b'a'; 1001];
        let result = f
            .receive(multipart(&[
                file("document", "a.docx", b"ok"),
                file("other", "b", &big),
            ]))
            .await;
        assert!(matches!(result, Err(UploadError::TooLarge)), "{result:?}");
    }

    #[tokio::test]
    async fn a_request_over_the_limit_plus_the_allowance_is_too_large_from_small_parts() {
        let f = fixture(1000);
        let small = vec![b'a'; 900];
        let mut parts = vec![file("document", "a.docx", b"ok")];
        let count = crate::num::usize_from_u64(MULTIPART_ALLOWANCE / 900 + 2);
        parts.extend((0..count).map(|_| field("other", &small)));
        let result = f.receive(multipart(&parts)).await;
        assert!(matches!(result, Err(UploadError::TooLarge)), "{result:?}");
    }

    /// A body that would be `total` bytes, made on demand, counting what was taken from it.
    fn endless_document(
        total: u64,
        taken: Arc<AtomicU64>,
    ) -> impl Stream<Item = Result<Bytes, Infallible>> + Send {
        const CHUNK: usize = 64 * 1024;
        let head = Bytes::from(format!(
            "--{BOUNDARY}\r\nContent-Disposition: form-data; name=\"document\"; filename=\"big.docx\"\r\n\r\n"
        ));
        let zeros = Bytes::from(vec![0u8; CHUNK]);
        let mut first = Some(head);
        stream::iter(std::iter::from_fn(move || {
            let next = first.take().unwrap_or_else(|| zeros.clone());
            if taken.load(Ordering::SeqCst) >= total {
                return None;
            }
            taken.fetch_add(next.len() as u64, Ordering::SeqCst);
            Some(Ok(next))
        }))
    }

    #[tokio::test]
    async fn a_declared_length_over_the_limit_is_refused_before_a_byte_is_read() {
        let f = fixture(1000);
        let taken = Arc::new(AtomicU64::new(0));
        let result = f
            .upload
            .receive(
                &headers(Some(&form()), Some(f.upload.request_limit() + 1)),
                endless_document(1 << 30, taken.clone()),
            )
            .await;
        assert!(matches!(result, Err(UploadError::TooLarge)), "{result:?}");
        assert_eq!(taken.load(Ordering::SeqCst), 0);
        assert_eq!(f.spooled_directories(), 0);
    }

    #[tokio::test]
    async fn a_body_far_over_the_limit_is_read_only_up_to_the_limit() {
        // A 4 GiB body (chunked: no declared length) against a 1 MiB limit: refused once the limit is
        // crossed, having taken at most the request limit plus one network chunk — memory and disk stay
        // bounded by the limit, not by the body.
        let f = fixture(1 << 20);
        let taken = Arc::new(AtomicU64::new(0));
        let result = f
            .upload
            .receive(
                &headers(Some(&form()), None),
                endless_document(4 << 30, taken.clone()),
            )
            .await;
        assert!(matches!(result, Err(UploadError::TooLarge)), "{result:?}");
        let taken = taken.load(Ordering::SeqCst);
        assert!(
            taken <= f.upload.request_limit() + 64 * 1024,
            "took {taken} bytes against a request limit of {}",
            f.upload.request_limit()
        );
        assert_eq!(f.spooled_directories(), 0);
    }

    #[tokio::test]
    async fn a_body_that_fails_while_streaming_is_malformed() {
        let f = fixture(1024);
        let broken = stream::iter(vec![
            Ok(Bytes::from(format!(
                "--{BOUNDARY}\r\nContent-Disposition: form-data; name=\"document\"; filename=\"a\"\r\n\r\nab"
            ))),
            Err(std::io::Error::other("connection reset")),
        ]);
        let result = f
            .upload
            .receive(&headers(Some(&form()), None), broken)
            .await;
        assert!(matches!(result, Err(UploadError::Malformed)), "{result:?}");
        assert_eq!(f.spooled_directories(), 0);
    }

    #[test]
    fn each_upload_error_maps_to_its_http_status() {
        use axum::http::StatusCode;
        for (error, status) in [
            (UploadError::NotMultipart, StatusCode::BAD_REQUEST),
            (UploadError::Malformed, StatusCode::BAD_REQUEST),
            (UploadError::NoDocument, StatusCode::BAD_REQUEST),
            (UploadError::TooLarge, StatusCode::PAYLOAD_TOO_LARGE),
            (UploadError::Inflated, StatusCode::PAYLOAD_TOO_LARGE),
            (
                UploadError::Spool(std::io::Error::other("disk full")),
                StatusCode::INTERNAL_SERVER_ERROR,
            ),
        ] {
            assert_eq!(Problem::from(error).status(), status);
        }
    }

    /// A DOCX whose `word/document.xml` is `size` bytes of `<w:p/>`, deflated: a deflate bomb in small.
    fn bomb(size: usize) -> Vec<u8> {
        const CT: &str = r#"<Types><Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/></Types>"#;
        let mut out = Vec::new();
        let mut central = Vec::new();
        for (name, data) in [
            ("[Content_Types].xml", CT.as_bytes().to_vec()),
            (
                "word/document.xml",
                b"<w:p/>".repeat(size / 6 + 1)[..size].to_vec(),
            ),
        ] {
            let packed = miniz_oxide::deflate::compress_to_vec(&data, 6);
            let offset = u32::try_from(out.len()).unwrap();
            let sizes = [
                u32::try_from(packed.len()).unwrap(),
                u32::try_from(data.len()).unwrap(),
            ];
            out.extend_from_slice(b"PK\x03\x04\x14\0\0\0\x08\0\0\0\0\0\0\0\0\0");
            for v in &sizes {
                out.extend_from_slice(&v.to_le_bytes());
            }
            out.extend_from_slice(&u16::try_from(name.len()).unwrap().to_le_bytes());
            out.extend_from_slice(&[0, 0]);
            out.extend_from_slice(name.as_bytes());
            out.extend_from_slice(&packed);
            central.extend_from_slice(b"PK\x01\x02\x14\0\x14\0\0\0\x08\0\0\0\0\0\0\0\0\0");
            for v in &sizes {
                central.extend_from_slice(&v.to_le_bytes());
            }
            central.extend_from_slice(&u16::try_from(name.len()).unwrap().to_le_bytes());
            central.extend_from_slice(&[0; 12]);
            central.extend_from_slice(&offset.to_le_bytes());
            central.extend_from_slice(name.as_bytes());
        }
        let directory = u32::try_from(out.len()).unwrap();
        out.extend_from_slice(&central);
        out.extend_from_slice(b"PK\x05\x06\0\0\0\0\x02\0\x02\0");
        out.extend_from_slice(&u32::try_from(central.len()).unwrap().to_le_bytes());
        out.extend_from_slice(&directory.to_le_bytes());
        out.extend_from_slice(&[0, 0]);
        out
    }

    #[tokio::test]
    async fn a_docx_that_inflates_past_the_limit_is_too_large_and_not_kept() {
        let mut f = fixture(1024 * 1024);
        f.upload.max_inflated_size = 1024 * 1024;
        let bomb = bomb(2 * 1024 * 1024);
        assert!(bomb.len() < 16 * 1024);
        let body = multipart(&[file("document", "a.docx", &bomb)]);
        let result = f
            .upload
            .receive(&headers(Some(&form()), None), chunks(body, 4096))
            .await;
        assert!(matches!(result, Err(UploadError::Inflated)), "{result:?}");
        assert_eq!(
            f.spooled_directories(),
            0,
            "the refused document is removed"
        );
        assert_eq!(
            Problem::from(UploadError::Inflated),
            Problem::TooLarge(INFLATED)
        );

        // Within the limit, or with no limit, it is received.
        for limit in [3 * 1024 * 1024, 0] {
            f.upload.max_inflated_size = limit;
            let body = multipart(&[file("document", "a.docx", &bomb)]);
            let result = f
                .upload
                .receive(&headers(Some(&form()), None), chunks(body, 4096))
                .await;
            assert!(result.is_ok(), "limit {limit}: {result:?}");
        }
    }

    #[tokio::test]
    async fn any_received_document_past_the_limit_refuses_the_probes_request() {
        let mut f = fixture(1024 * 1024);
        f.upload.max_inflated_size = 1024 * 1024;
        let (small, large) = (bomb(1000), bomb(2 * 1024 * 1024));
        let body = multipart(&[
            file("document", "small.docx", &small),
            file("document", "large.docx", &large),
        ]);
        let result = f
            .upload
            .receive_all(&headers(Some(&form()), None), chunks(body, 4096), 4)
            .await;
        assert!(matches!(result, Err(UploadError::Inflated)), "{result:?}");
        assert_eq!(f.spooled_directories(), 0);
    }

    #[test]
    fn the_default_inflated_limit_follows_the_document_limit() {
        assert_eq!(Upload::new(10 << 20).max_inflated_size, 256 << 20);
        assert_eq!(Upload::new(1200 << 20).max_inflated_size, 3600 << 20);
    }

    #[test]
    fn the_request_limit_leaves_room_for_the_multipart_framing() {
        assert_eq!(
            Upload::new(1000).request_limit(),
            1000 + MULTIPART_ALLOWANCE
        );
        assert_eq!(Upload::new(u64::MAX).request_limit(), u64::MAX);
    }
}
