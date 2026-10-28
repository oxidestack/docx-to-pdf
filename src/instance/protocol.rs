//! The pipe protocol between the server and an instance.
//!
//! One line per message, fields separated by a tab, `\n` at the end:
//! - the instance's first line on stdout: `ready\t<LOK init ms>\t<pid>`;
//! - a request on its stdin: `convert\t<input path>\t<output path>`;
//! - its answer: `ok\t<load ms>\t<export ms>`, `not_loaded\t<LOK's error text>`,
//!   `export_failed\t<LOK's error text>` or `bad_request\t<what was wrong>`.
//!
//! Paths are absolute and the server makes them (the conversion's temporary directory), so a path holding a
//! tab or a line break is refused when the request is encoded rather than escaped. An error text from
//! LibreOffice has its tabs and line breaks turned into spaces.

use std::fmt;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// The instance's ready line: LOK is initialised and requests may come.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Ready {
    /// Milliseconds from the instance's start to the end of LOK's initialisation, as the instance measured it.
    pub init_ms: u64,
    /// The instance's own process id.
    pub pid: u32,
}

/// A request to an instance.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Request {
    /// Load the document at `input` and store it as a PDF at `output`.
    Convert {
        /// The document to load.
        input: PathBuf,
        /// Where the PDF goes.
        output: PathBuf,
    },
}

/// An instance's answer to one request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Answer {
    /// The PDF is at the output path. The two durations are LOK's load and its export (`saveAs`).
    Converted {
        /// LOK's `documentLoad`.
        load: Duration,
        /// LOK's `saveAs`.
        export: Duration,
    },
    /// LOK's `documentLoad` returned null; the text is LOK's `getError` (a 422: an allowed format the backend cannot
    /// load).
    NotLoaded(String),
    /// LOK's `saveAs` failed; the text is LOK's `getError`.
    ExportFailed(String),
    /// The instance could not read the request line.
    BadRequest(String),
}

/// A line that is not a message of this protocol, or a path that cannot travel in one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProtocolError(pub String);

impl fmt::Display for ProtocolError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for ProtocolError {}

fn bad(what: impl Into<String>) -> ProtocolError {
    ProtocolError(what.into())
}

/// The fields of one line, its trailing `\n` (or `\r\n`) removed.
fn fields(line: &str) -> Vec<&str> {
    line.trim_end_matches(['\n', '\r']).split('\t').collect()
}

fn millis(field: &str) -> Result<u64, ProtocolError> {
    field
        .parse()
        .map_err(|_| bad(format!("not a number of milliseconds: {field:?}")))
}

#[allow(
    clippy::unnecessary_debug_formatting,
    reason = "a path that is not UTF-8 is shown escaped"
)]
fn path_field(path: &Path) -> Result<&str, ProtocolError> {
    let s = path
        .to_str()
        .ok_or_else(|| bad(format!("not UTF-8: {path:?}")))?;
    if s.is_empty() || s.contains(['\t', '\n', '\r']) {
        return Err(bad(format!("cannot travel in a request line: {s:?}")));
    }
    Ok(s)
}

/// LibreOffice's text, made to fit in one field.
fn text_field(text: &str) -> String {
    text.replace(['\t', '\n', '\r'], " ")
}

impl Ready {
    /// The line the instance writes.
    pub fn encode(&self) -> String {
        format!("ready\t{}\t{}", self.init_ms, self.pid)
    }

    /// The ready line the instance wrote.
    pub fn parse(line: &str) -> Result<Self, ProtocolError> {
        match fields(line).as_slice() {
            ["ready", init_ms, pid] => Ok(Self {
                init_ms: millis(init_ms)?,
                pid: pid
                    .parse()
                    .map_err(|_| bad(format!("not a process id: {pid:?}")))?,
            }),
            _ => Err(bad(format!("not a ready line: {line:?}"))),
        }
    }
}

impl Request {
    /// The line the service writes; a path that cannot travel in one field is an error.
    pub fn encode(&self) -> Result<String, ProtocolError> {
        match self {
            Self::Convert { input, output } => Ok(format!(
                "convert\t{}\t{}",
                path_field(input)?,
                path_field(output)?
            )),
        }
    }

    /// The request line the service wrote.
    pub fn parse(line: &str) -> Result<Self, ProtocolError> {
        match fields(line).as_slice() {
            ["convert", input, output] if !input.is_empty() && !output.is_empty() => {
                Ok(Self::Convert {
                    input: PathBuf::from(input),
                    output: PathBuf::from(output),
                })
            }
            _ => Err(bad(format!("not a request: {line:?}"))),
        }
    }
}

impl Answer {
    /// The line the instance writes.
    pub fn encode(&self) -> String {
        match self {
            Self::Converted { load, export } => {
                format!("ok\t{}\t{}", load.as_millis(), export.as_millis())
            }
            Self::NotLoaded(text) => format!("not_loaded\t{}", text_field(text)),
            Self::ExportFailed(text) => format!("export_failed\t{}", text_field(text)),
            Self::BadRequest(text) => format!("bad_request\t{}", text_field(text)),
        }
    }

    /// The answer line the instance wrote.
    pub fn parse(line: &str) -> Result<Self, ProtocolError> {
        match fields(line).as_slice() {
            ["ok", load, export] => Ok(Self::Converted {
                load: Duration::from_millis(millis(load)?),
                export: Duration::from_millis(millis(export)?),
            }),
            ["not_loaded", text] => Ok(Self::NotLoaded((*text).to_string())),
            ["export_failed", text] => Ok(Self::ExportFailed((*text).to_string())),
            ["bad_request", text] => Ok(Self::BadRequest((*text).to_string())),
            _ => Err(bad(format!("not an answer: {line:?}"))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ready_round_trips() {
        let ready = Ready {
            init_ms: 71,
            pid: 4242,
        };
        assert_eq!(ready.encode(), "ready\t71\t4242");
        assert_eq!(Ready::parse("ready\t71\t4242\n"), Ok(ready));
    }

    #[test]
    fn anything_else_is_not_a_ready_line() {
        for line in [
            "",
            "ready",
            "ready\t1",
            "ready\tx\t1",
            "ready\t1\t-1",
            "ok\t1\t2",
        ] {
            assert!(Ready::parse(line).is_err(), "{line:?}");
        }
    }

    #[test]
    fn a_convert_request_round_trips() {
        let request = Request::Convert {
            input: "/tmp/c/input.docx".into(),
            output: "/tmp/c/output.pdf".into(),
        };
        let line = request.encode().unwrap();
        assert_eq!(line, "convert\t/tmp/c/input.docx\t/tmp/c/output.pdf");
        assert_eq!(Request::parse(&format!("{line}\n")), Ok(request));
    }

    #[test]
    fn a_path_that_cannot_travel_is_refused_when_encoded() {
        for input in ["/tmp/a\tb.docx", "/tmp/a\nb.docx", ""] {
            let request = Request::Convert {
                input: input.into(),
                output: "/tmp/out.pdf".into(),
            };
            assert!(request.encode().is_err(), "{input:?}");
        }
    }

    #[test]
    fn a_malformed_request_is_an_error() {
        for line in [
            "",
            "convert",
            "convert\t/a",
            "convert\t/a\t/b\t/c",
            "convert\t\t/b",
            "load\t/a\t/b",
        ] {
            assert!(Request::parse(line).is_err(), "{line:?}");
        }
    }

    #[test]
    fn every_answer_round_trips() {
        let answers = [
            Answer::Converted {
                load: Duration::from_millis(12),
                export: Duration::from_millis(34),
            },
            Answer::NotLoaded("loadComponentFromURL returned an empty reference".into()),
            Answer::ExportFailed("General Error".into()),
            Answer::BadRequest("not a request: \"x\"".into()),
        ];
        for answer in answers {
            assert_eq!(Answer::parse(&answer.encode()), Ok(answer));
        }
    }

    #[test]
    fn an_error_text_is_flattened_into_one_field() {
        let line = Answer::NotLoaded("first\tsecond\nthird\r".into()).encode();
        assert_eq!(line, "not_loaded\tfirst second third ");
        assert_eq!(
            Answer::parse(&line),
            Ok(Answer::NotLoaded("first second third ".into()))
        );
    }

    #[test]
    fn a_malformed_answer_is_an_error() {
        for line in [
            "",
            "ok",
            "ok\t1",
            "ok\t1\tx",
            "not_loaded",
            "ready\t1\t2",
            "fine",
        ] {
            assert!(Answer::parse(line).is_err(), "{line:?}");
        }
    }
}
