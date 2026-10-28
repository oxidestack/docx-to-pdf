//! A minimal HTTP/1.1 client over a raw socket, the same for moalhaddar/docx-to-pdf and for oxidestack/docx-to-pdf:
//! it sends exactly the framing a step asks for (`Content-Length` or chunked, `Expect: 100-continue`) and keeps interim
//! answers, which a general-purpose client hides.

use std::fmt::Write as _;
use std::io::{self, Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::time::Duration;

/// A request as it goes on the wire.
pub struct Request {
    pub method: String,
    pub path: String,
    /// Headers in order, beyond `Host`, the body's framing and `Connection: close`.
    pub headers: Vec<(String, String)>,
    pub body: Option<Vec<u8>>,
    /// Send the body chunked instead of with `Content-Length`.
    pub chunked: bool,
    /// Send `Expect: 100-continue` and wait for the interim answer before the body.
    pub expect_continue: bool,
}

/// An answer: the interim statuses, then the final status, headers (names lowercased, in order) and body.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Response {
    pub interim: Vec<u16>,
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

impl Response {
    /// Every value of the header `name`, joined by `, ` (RFC 9110 §5.3); `None` when absent.
    pub fn header(&self, name: &str) -> Option<String> {
        let values: Vec<&str> = self
            .headers
            .iter()
            .filter(|(n, _)| n.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
            .collect();
        (!values.is_empty()).then(|| values.join(", "))
    }
}

/// How long to wait for `100 Continue` before sending the body anyway, as curl does.
const CONTINUE_WAIT: Duration = Duration::from_secs(10);

/// Where a scenario is sent: `host:port`.
#[derive(Debug, Clone)]
pub struct Target {
    pub authority: String,
    /// The longest a step may take; the emulated moalhaddar/docx-to-pdf converts slowly.
    pub timeout: Duration,
}

impl Target {
    pub fn new(authority: impl Into<String>, timeout: Duration) -> Self {
        Self {
            authority: authority.into(),
            timeout,
        }
    }

    /// From a base URL such as `http://localhost:48080`.
    pub fn from_url(url: &str, timeout: Duration) -> Self {
        let authority = url
            .trim_start_matches("http://")
            .trim_end_matches('/')
            .to_owned();
        Self::new(authority, timeout)
    }

    /// Connects to the first address that answers: `localhost` may resolve to `::1` first while a Docker
    /// port is published on IPv4 only.
    fn connect(&self) -> io::Result<TcpStream> {
        let mut last = io::Error::other(format!("{}: no address", self.authority));
        for addr in self.authority.to_socket_addrs()? {
            match TcpStream::connect_timeout(&addr, Duration::from_secs(10)) {
                Ok(stream) => return Ok(stream),
                Err(e) => last = e,
            }
        }
        Err(last)
    }
}

/// Sends `request` on a fresh connection and reads the whole answer.
pub fn send(target: &Target, request: &Request) -> io::Result<Response> {
    let mut stream = target.connect()?;
    stream.set_read_timeout(Some(target.timeout))?;
    stream.set_write_timeout(Some(target.timeout))?;

    let mut head = format!(
        "{} {} HTTP/1.1\r\nHost: {}\r\n",
        request.method, request.path, target.authority
    );
    for (name, value) in &request.headers {
        write!(head, "{name}: {value}\r\n").unwrap();
    }
    if let Some(body) = &request.body {
        if request.chunked {
            head.push_str("Transfer-Encoding: chunked\r\n");
        } else {
            write!(head, "Content-Length: {}\r\n", body.len()).unwrap();
        }
    }
    if request.expect_continue {
        head.push_str("Expect: 100-continue\r\n");
    }
    head.push_str("Connection: close\r\n\r\n");
    stream.write_all(head.as_bytes())?;

    let mut reader = Reader {
        stream,
        buf: Vec::new(),
    };
    let mut interim = Vec::new();
    if request.expect_continue {
        reader.stream.set_read_timeout(Some(CONTINUE_WAIT))?;
        match reader.head() {
            Ok((100, _)) => interim.push(100),
            Ok((status, headers)) => {
                // A final answer before the body: the server refused it unread.
                reader.stream.set_read_timeout(Some(target.timeout))?;
                let body = reader.body(status, &request.method, &headers)?;
                return Ok(Response {
                    interim,
                    status,
                    headers,
                    body,
                });
            }
            Err(e)
                if matches!(
                    e.kind(),
                    io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
                ) => {}
            Err(e) => return Err(e),
        }
        reader.stream.set_read_timeout(Some(target.timeout))?;
    }
    // A server may answer and close without reading the body (a 404 before the route reads it); the
    // answer is still the answer, so a failed write is reported only when no answer can be read either.
    let written = match &request.body {
        Some(body) => write_body(&mut reader.stream, body, request.chunked),
        None => Ok(()),
    };
    loop {
        let (status, headers) = match (reader.head(), &written) {
            (Ok(head), _) => head,
            (Err(_), Err(w)) => {
                return Err(io::Error::new(w.kind(), format!("sending the body: {w}")));
            }
            (Err(e), Ok(())) => return Err(e),
        };
        if (100..200).contains(&status) {
            interim.push(status);
            continue;
        }
        let body = reader.body(status, &request.method, &headers)?;
        return Ok(Response {
            interim,
            status,
            headers,
            body,
        });
    }
}

fn write_body(stream: &mut TcpStream, body: &[u8], chunked: bool) -> io::Result<()> {
    if !chunked {
        return stream.write_all(body);
    }
    for chunk in body.chunks(4096) {
        stream.write_all(format!("{:x}\r\n", chunk.len()).as_bytes())?;
        stream.write_all(chunk)?;
        stream.write_all(b"\r\n")?;
    }
    stream.write_all(b"0\r\n\r\n")
}

struct Reader {
    stream: TcpStream,
    buf: Vec<u8>,
}

impl Reader {
    fn fill(&mut self) -> io::Result<usize> {
        let mut chunk = [0u8; 16384];
        let n = self.stream.read(&mut chunk)?;
        self.buf.extend_from_slice(&chunk[..n]);
        Ok(n)
    }

    /// Reads up to and including `\r\n\r\n`: the status and the headers.
    fn head(&mut self) -> io::Result<(u16, Vec<(String, String)>)> {
        let end = loop {
            if let Some(i) = find(&self.buf, b"\r\n\r\n") {
                break i;
            }
            if self.fill()? == 0 {
                return Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    format!(
                        "connection closed before an answer: {:?}",
                        String::from_utf8_lossy(&self.buf)
                    ),
                ));
            }
        };
        let head = String::from_utf8_lossy(&self.buf[..end]).into_owned();
        self.buf.drain(..end + 4);
        let mut lines = head.split("\r\n");
        let status_line = lines.next().unwrap_or_default();
        let status = status_line
            .split(' ')
            .nth(1)
            .and_then(|s| s.parse().ok())
            .ok_or_else(|| io::Error::other(format!("bad status line {status_line:?}")))?;
        let headers = lines
            .filter_map(|l| l.split_once(':'))
            .map(|(n, v)| (n.trim().to_ascii_lowercase(), v.trim().to_owned()))
            .collect();
        Ok((status, headers))
    }

    fn body(
        &mut self,
        status: u16,
        method: &str,
        headers: &[(String, String)],
    ) -> io::Result<Vec<u8>> {
        if method == "HEAD" || status == 204 || status == 304 {
            return Ok(Vec::new());
        }
        let header = |name: &str| {
            headers
                .iter()
                .find(|(n, _)| n == name)
                .map(|(_, v)| v.as_str())
        };
        if header("transfer-encoding").is_some_and(|v| v.eq_ignore_ascii_case("chunked")) {
            return self.chunked();
        }
        if let Some(length) = header("content-length") {
            let length: usize = length
                .parse()
                .map_err(|_| io::Error::other(format!("bad content-length {length:?}")))?;
            while self.buf.len() < length {
                if self.fill()? == 0 {
                    return Err(io::Error::new(
                        io::ErrorKind::UnexpectedEof,
                        "body shorter than its content-length",
                    ));
                }
            }
            return Ok(self.buf.drain(..length).collect());
        }
        while self.fill()? > 0 {}
        Ok(std::mem::take(&mut self.buf))
    }

    fn chunked(&mut self) -> io::Result<Vec<u8>> {
        let mut body = Vec::new();
        loop {
            let line_end = loop {
                if let Some(i) = find(&self.buf, b"\r\n") {
                    break i;
                }
                if self.fill()? == 0 {
                    return Err(io::Error::new(
                        io::ErrorKind::UnexpectedEof,
                        "chunked body cut short",
                    ));
                }
            };
            let line = String::from_utf8_lossy(&self.buf[..line_end]).into_owned();
            self.buf.drain(..line_end + 2);
            let size_text = line.split(';').next().unwrap_or_default().trim();
            let size = usize::from_str_radix(size_text, 16)
                .map_err(|_| io::Error::other(format!("bad chunk size {line:?}")))?;
            if size == 0 {
                // Trailers, if any, end with an empty line; the connection closes after them.
                while find(&self.buf, b"\r\n").is_none() && self.fill()? > 0 {}
                return Ok(body);
            }
            while self.buf.len() < size + 2 {
                if self.fill()? == 0 {
                    return Err(io::Error::new(
                        io::ErrorKind::UnexpectedEof,
                        "chunk cut short",
                    ));
                }
            }
            body.extend(self.buf.drain(..size));
            self.buf.drain(..2);
        }
    }
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|w| w == needle)
}

/// The boundary of every multipart body the harness builds; fixed so a request is the same every run.
pub const BOUNDARY: &str = "------------------------docxtopdfscenario";

/// One part of a `multipart/form-data` body.
pub enum Part {
    File {
        name: String,
        filename: String,
        content_type: String,
        bytes: Vec<u8>,
    },
    Field {
        name: String,
        value: String,
    },
}

/// A `multipart/form-data` body of `parts` in order, as curl's `-F` writes it.
pub fn multipart(parts: &[Part]) -> Vec<u8> {
    let mut body = Vec::new();
    for part in parts {
        body.extend_from_slice(format!("--{BOUNDARY}\r\n").as_bytes());
        match part {
            Part::File {
                name,
                filename,
                content_type,
                bytes,
            } => {
                body.extend_from_slice(
                    format!(
                        "Content-Disposition: form-data; name=\"{name}\"; filename=\"{filename}\"\r\nContent-Type: {content_type}\r\n\r\n"
                    )
                    .as_bytes(),
                );
                body.extend_from_slice(bytes);
            }
            Part::Field { name, value } => {
                body.extend_from_slice(
                    format!("Content-Disposition: form-data; name=\"{name}\"\r\n\r\n{value}")
                        .as_bytes(),
                );
            }
        }
        body.extend_from_slice(b"\r\n");
    }
    body.extend_from_slice(format!("--{BOUNDARY}--\r\n").as_bytes());
    body
}
