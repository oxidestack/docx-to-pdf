//! One LibreOffice instance, with the same process model as upstream and Gotenberg: a child process running this
//! binary in instance mode, hosting LibreOfficeKit, in its own process group with its own temporary profile.
//!
//! - [`protocol`]: the lines on the instance's stdin and stdout.
//! - [`child`]: instance mode, the inside of the child process (`docx-to-pdf instance <profile dir>`).
//! - [`driver`]: the server's side: start an instance, send conversions, kill it, delete its profile.

pub mod child;
pub mod driver;
#[cfg(test)]
pub(crate) mod group;
#[allow(
    unsafe_code,
    reason = "the LibreOfficeKit FFI: dlopen and the C structs of function pointers"
)]
mod lok;
pub mod protocol;

/// The argument that puts the binary in instance mode: `docx-to-pdf instance <profile dir>`.
pub const INSTANCE_MODE: &str = "instance";

/// How the instance's start-failure line on stderr begins; the rest of the line is the backend's own error
/// text (such as `dlerror()`'s). The server logs that text as `backend_error` under "the backend did not
/// start", never as a message, so a log does not reveal which backend runs.
pub const START_FAILED_PREFIX: &str = "instance: the backend did not start: ";

/// Where Debian's `libreoffice-core-nogui` puts `libmergedlo.so`, the image's LibreOffice.
pub const PROGRAM_DIR: &str = "/usr/lib/libreoffice/program";
