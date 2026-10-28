//! The LibreOfficeKit binding: a hand-written declaration of the few members used, not a third-party
//! crate (none is tested against LibreOffice 26.x).
//!
//! Checked against `include/LibreOfficeKit/LibreOfficeKit.h` of core `libreoffice-26-8`: the leading members of
//! `LibreOfficeKitClass` (`:52-82`) and `LibreOfficeKitDocumentClass` (`:190-200`), as a LibreOfficeKit prototype
//! used them. Nothing links at build time: `libmergedlo.so` is `dlopen`ed at run time and
//! `libreofficekit_hook_2` is called as `LibreOfficeKitInit.h:329-380` does.

use std::ffi::{CStr, CString, c_char, c_int};
use std::fmt::Write as _;
use std::path::Path;
use std::time::Instant;

use super::protocol::Answer;

#[repr(C)]
struct LokOffice {
    class: *const LokOfficeClass,
}

// Every member is declared so the layout matches; not every one is called.
#[allow(dead_code)]
#[repr(C)]
struct LokOfficeClass {
    n_size: usize,
    destroy: unsafe extern "C" fn(*mut LokOffice),
    document_load: unsafe extern "C" fn(*mut LokOffice, *const c_char) -> *mut LokDocument,
    get_error: unsafe extern "C" fn(*mut LokOffice) -> *mut c_char,
    document_load_with_options:
        unsafe extern "C" fn(*mut LokOffice, *const c_char, *const c_char) -> *mut LokDocument,
    free_error: unsafe extern "C" fn(*mut c_char),
}

#[repr(C)]
struct LokDocument {
    class: *const LokDocumentClass,
}

#[allow(dead_code)]
#[repr(C)]
struct LokDocumentClass {
    n_size: usize,
    destroy: unsafe extern "C" fn(*mut LokDocument),
    save_as: unsafe extern "C" fn(
        *mut LokDocument,
        *const c_char,
        *const c_char,
        *const c_char,
    ) -> c_int,
}

type Hook2 = unsafe extern "C" fn(*const c_char, *const c_char) -> *mut LokOffice;

/// `saveAs`'s format: `pdf` maps to `writer_pdf_Export` for a text document (`init.cxx:307`).
const FORMAT: &str = "pdf";
/// `saveAs`'s options. `{}` keeps `FilterData` empty, which is upstream's store descriptor exactly
/// (`UnoService.kt:79-83`), so the export takes its options from the profile's configuration
/// (`child::registry_modifications`). Any `FilterData` would replace all of them by the filter's own defaults
/// (no tagged PDF, comments as annotations).
const FILTER_OPTIONS: &str = "{}";

/// LibreOfficeKit, initialised once in this process (it cannot be initialised twice, `init.cxx:8449-8468`).
pub struct Office {
    office: *mut LokOffice,
}

fn cstring(s: &str) -> Result<CString, String> {
    CString::new(s).map_err(|_| format!("a NUL byte in {s:?}"))
}

/// A `file:` URL for an absolute path, every byte outside the unreserved set and `/` percent-encoded.
pub fn file_url(path: &Path) -> String {
    use std::os::unix::ffi::OsStrExt;
    let mut url = String::from("file://");
    for &b in path.as_os_str().as_bytes() {
        if b.is_ascii_alphanumeric() || b"-._~/".contains(&b) {
            url.push(b as char);
        } else {
            let _ = write!(url, "%{b:02X}");
        }
    }
    url
}

impl Office {
    /// `dlopen`s `<program_dir>/libmergedlo.so` and initialises LOK with the profile at `profile_dir`.
    pub fn init(program_dir: &str, profile_dir: &Path) -> Result<Self, String> {
        let lib = cstring(&format!("{program_dir}/libmergedlo.so"))?;
        let program_dir = cstring(program_dir)?;
        let profile_url = cstring(&file_url(profile_dir))?;
        // SAFETY: dlopen/dlsym are called with NUL-terminated strings; the symbol is LOK's documented entry
        // point with the `Hook2` signature (`LibreOfficeKitInit.h`); the class size is checked before any
        // member past `n_size` is used.
        unsafe {
            let handle = libc::dlopen(lib.as_ptr(), libc::RTLD_LAZY | libc::RTLD_GLOBAL);
            if handle.is_null() {
                return Err(format!("dlopen: {}", dl_error()));
            }
            let sym = libc::dlsym(handle, c"libreofficekit_hook_2".as_ptr());
            if sym.is_null() {
                return Err(format!("no entry point: {}", dl_error()));
            }
            let hook: Hook2 = std::mem::transmute::<*mut libc::c_void, Hook2>(sym);
            let office = hook(program_dir.as_ptr(), profile_url.as_ptr());
            if office.is_null() {
                return Err("the entry point returned null".into());
            }
            let size = (*(*office).class).n_size;
            let need = std::mem::size_of::<LokOfficeClass>();
            if size < need {
                return Err(format!("the function table is too small: {size} < {need}"));
            }
            Ok(Self { office })
        }
    }

    /// Loads `input`, stores it as a PDF at `output`, destroys the document.
    pub fn convert(&mut self, input: &Path, output: &Path) -> Answer {
        let (input_url, output_url) = match (cstring(&file_url(input)), cstring(&file_url(output)))
        {
            (Ok(i), Ok(o)) => (i, o),
            (Err(e), _) | (_, Err(e)) => return Answer::BadRequest(e),
        };
        // SAFETY: `self.office` came from a successful init and its class was size-checked; the document
        // pointer is checked for null and destroyed exactly once; every string is NUL-terminated and lives
        // across the call.
        unsafe {
            let class = &*(*self.office).class;
            let started = Instant::now();
            let doc =
                (class.document_load_with_options)(self.office, input_url.as_ptr(), c"".as_ptr());
            let load = started.elapsed();
            if doc.is_null() {
                return Answer::NotLoaded(self.error());
            }
            let doc_class = &*(*doc).class;
            let started = Instant::now();
            let format = cstring(FORMAT).expect("a constant");
            let options = cstring(FILTER_OPTIONS).expect("a constant");
            let stored =
                (doc_class.save_as)(doc, output_url.as_ptr(), format.as_ptr(), options.as_ptr());
            let export = started.elapsed();
            let answer = if stored == 0 {
                Answer::ExportFailed(self.error())
            } else {
                Answer::Converted { load, export }
            };
            (doc_class.destroy)(doc);
            answer
        }
    }

    /// LOK's last error text.
    fn error(&self) -> String {
        // SAFETY: as in `convert`; the returned string is LOK's and is freed with LOK's `freeError`.
        unsafe {
            let class = &*(*self.office).class;
            let text = (class.get_error)(self.office);
            if text.is_null() {
                return "(no error text)".into();
            }
            let s = CStr::from_ptr(text).to_string_lossy().into_owned();
            (class.free_error)(text);
            if s.is_empty() {
                "(empty error text)".into()
            } else {
                s
            }
        }
    }
}

fn dl_error() -> String {
    // SAFETY: dlerror returns null or a NUL-terminated string owned by the C library.
    unsafe {
        let e = libc::dlerror();
        if e.is_null() {
            "(no dlerror)".into()
        } else {
            CStr::from_ptr(e).to_string_lossy().into_owned()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_plain_path_is_a_plain_url() {
        assert_eq!(
            file_url(Path::new("/tmp/c-1/input.docx")),
            "file:///tmp/c-1/input.docx"
        );
    }

    #[test]
    fn reserved_bytes_are_percent_encoded() {
        assert_eq!(
            file_url(Path::new("/tmp/a b#c%d?é")),
            "file:///tmp/a%20b%23c%25d%3F%C3%A9"
        );
    }
}
