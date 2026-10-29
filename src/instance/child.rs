//! Instance mode: the inside of an instance process.
//!
//! `docx-to-pdf instance [--max-image-dpi=N] <profile dir>`: writes `UseLocking=false` into the profile, initialises LibreOfficeKit
//! once with it, prints the ready line, then serves one request per stdin line until stdin closes, handing the
//! heap's free memory back to the kernel after each conversion ([`trim_heap`]). The server makes the profile directory
//! and deletes it afterwards.
//!
//! The protocol goes to a private copy of the original stdout; file descriptor 1 is pointed at stderr before
//! LibreOffice loads, so nothing LibreOffice prints can land in the pipe. Its output, all of it on stderr, is
//! what the server logs.

use std::fs::File;
use std::io::{BufRead, Write};
use std::os::fd::FromRawFd;
use std::path::Path;
use std::time::Instant;

use super::lok::Office;
use super::protocol::{Answer, Ready, Request};
use super::{PROGRAM_DIR, START_FAILED_PREFIX};

/// What the instance writes to `<profile>/user/registrymodifications.xcu` before LOK's init:
/// - `Office.Common/Misc/UseLocking=false` (`officecfg/…/Common.xcs:5005-5011`), so no lock file appears beside
///   the input. LOK's load cannot ask for `ReadOnly` (`init.cxx:2962-2968`).
/// - `Office.Common/Filter/PDF/Export/ReduceImageResolution`, `true` with `MaxImageResolution` at 300 in
///   LibreOffice's own defaults: reducing decodes, resamples and re-encodes every picture, which cost the typical
///   report two thirds of its conversion's CPU (1.2-1.7 against 0.5 CPU s). Unless
///   `max_image_dpi` is set (`DOCX_TO_PDF_PDF_MAX_IMAGE_DPI`), pictures are embedded as they are, a JPEG byte for
///   byte; set, a picture above it is reduced to it. In the profile rather than in `saveAs`'s `FilterData`:
///   any `FilterData` at all makes the export take every other option from the filter's own defaults (no tagged
///   PDF, comments as annotations), not from this configuration.
/// - No content from outside the document, since a picture linked to a URL is otherwise fetched while the
///   document loads, from any address it names (server-side request forgery from inside the cluster, and a host
///   that never answers holds the instance): `Office.Common/Security/Scripting/
///   BlockUntrustedRefererLinks=true` with no trusted location configured, so a picture linked to a URL or a local
///   file is not loaded; and `org.openoffice.Inet/Settings`' HTTP and HTTPS proxy set to 127.0.0.1:9, where nothing
///   listens, with no exceptions, so any fetch past the first setting fails at once instead of reaching the network.
pub fn registry_modifications(max_image_dpi: Option<u32>) -> String {
    let images = match max_image_dpi {
        None => {
            r#"<prop oor:name="ReduceImageResolution" oor:op="fuse"><value>false</value></prop>"#
                .to_owned()
        }
        Some(dpi) => format!(
            r#"<prop oor:name="ReduceImageResolution" oor:op="fuse"><value>true</value></prop><prop oor:name="MaxImageResolution" oor:op="fuse"><value>{dpi}</value></prop>"#
        ),
    };
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<oor:items xmlns:oor="http://openoffice.org/2001/registry" xmlns:xs="http://www.w3.org/2001/XMLSchema" xmlns:xsi="http://www.w3.org/2001/XMLSchema-instance">
<item oor:path="/org.openoffice.Office.Common/Misc"><prop oor:name="UseLocking" oor:op="fuse"><value>false</value></prop></item>
<item oor:path="/org.openoffice.Office.Common/Filter/PDF/Export">{images}</item>
<item oor:path="/org.openoffice.Office.Common/Security/Scripting"><prop oor:name="BlockUntrustedRefererLinks" oor:op="fuse"><value>true</value></prop></item>
<item oor:path="/org.openoffice.Inet/Settings"><prop oor:name="ooInetProxyType" oor:op="fuse"><value>1</value></prop><prop oor:name="ooInetHTTPProxyName" oor:op="fuse"><value>127.0.0.1</value></prop><prop oor:name="ooInetHTTPProxyPort" oor:op="fuse"><value>9</value></prop><prop oor:name="ooInetHTTPSProxyName" oor:op="fuse"><value>127.0.0.1</value></prop><prop oor:name="ooInetHTTPSProxyPort" oor:op="fuse"><value>9</value></prop><prop oor:name="ooInetNoProxy" oor:op="fuse"><value></value></prop></item>
</oor:items>
"#
    )
}

/// Exit status when the instance cannot start: no ready line was printed.
pub const EXIT_START_FAILED: i32 = 3;
/// Exit status when the command line is wrong.
pub const EXIT_USAGE: i32 = 2;

/// The flag that caps a picture's resolution in the PDF (`DOCX_TO_PDF_PDF_MAX_IMAGE_DPI`, [`registry_modifications`]).
pub const MAX_IMAGE_DPI_FLAG: &str = "--max-image-dpi=";

/// Instance mode's arguments: the optional picture resolution cap, then the profile directory.
fn parse_args(args: &[String]) -> Option<(Option<u32>, &Path)> {
    match args {
        [profile] => Some((None, Path::new(profile))),
        [flag, profile] => {
            let dpi = flag.strip_prefix(MAX_IMAGE_DPI_FLAG)?.parse().ok()?;
            Some((Some(dpi), Path::new(profile)))
        }
        _ => None,
    }
}

/// Writes the profile's settings ([`registry_modifications`]).
pub fn prepare_profile(profile_dir: &Path, max_image_dpi: Option<u32>) -> std::io::Result<()> {
    let user = profile_dir.join("user");
    std::fs::create_dir_all(&user)?;
    std::fs::write(
        user.join("registrymodifications.xcu"),
        registry_modifications(max_image_dpi),
    )
}

/// Instance mode's entry point; `args` are the ones after `instance`. Never returns: the process ends with
/// `_exit`, so LibreOffice's static destructors do not run (under LOK `terminate()` is a no-op anyway,
/// `init.cxx:8504-8505`).
#[allow(
    unsafe_code,
    reason = "`_exit` and descriptor duplication, which std has no form of"
)]
pub fn run(args: &[String]) -> ! {
    let code = if let Some((max_image_dpi, profile)) = parse_args(args) {
        serve(profile, max_image_dpi)
    } else {
        eprintln!("usage: docx-to-pdf instance [{MAX_IMAGE_DPI_FLAG}N] <profile dir>");
        EXIT_USAGE
    };
    // SAFETY: ends the process; nothing after it runs.
    unsafe { libc::_exit(code) }
}

fn serve(profile: &Path, max_image_dpi: Option<u32>) -> i32 {
    let started = Instant::now();
    if let Err(e) = prepare_profile(profile, max_image_dpi) {
        eprintln!("instance: profile {}: {e}", profile.display());
        return EXIT_START_FAILED;
    }
    let mut protocol = match private_stdout() {
        Ok(file) => std::io::LineWriter::new(file),
        Err(e) => {
            eprintln!("instance: redirecting stdout: {e}");
            return EXIT_START_FAILED;
        }
    };
    let mut office = match Office::init(PROGRAM_DIR, profile) {
        Ok(office) => office,
        Err(e) => {
            eprintln!("{START_FAILED_PREFIX}{e}");
            return EXIT_START_FAILED;
        }
    };
    let ready = Ready {
        init_ms: u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
        pid: std::process::id(),
    };
    if writeln!(protocol, "{}", ready.encode()).is_err() {
        return EXIT_START_FAILED;
    }
    serve_requests(
        std::io::stdin().lock(),
        &mut protocol,
        |input, output| office.convert(input, output),
        || {
            trim_heap();
        },
    );
    0
}

/// Answers one request per line of `requests` until they end or an answer cannot be written. After each conversion,
/// and before its answer is written, `trim` hands the heap's free memory back to the kernel ([`trim_heap`]).
///
/// Before the answer rather than after it: the server reads the instance's resident memory as soon as the answer
/// arrives, to recycle an instance above `DOCX_TO_PDF_INSTANCE_MAX_MEMORY`, and that read must see the trimmed heap,
/// not the free memory a picture-heavy document leaves (tens of MiB, at times over a hundred). A trim after the
/// answer would save the client only the trim itself (a few milliseconds; most of its cost is the next conversion
/// faulting the returned pages back in), and the next request would wait for it all the same.
fn serve_requests(
    requests: impl BufRead,
    answers: &mut impl Write,
    mut convert: impl FnMut(&Path, &Path) -> Answer,
    mut trim: impl FnMut(),
) {
    for line in requests.lines() {
        let Ok(line) = line else { break };
        let answer = match Request::parse(&line) {
            Ok(Request::Convert { input, output }) => {
                let answer = convert(&input, &output);
                trim();
                answer
            }
            Err(e) => Answer::BadRequest(e.to_string()),
        };
        if answers
            .write_all(format!("{}\n", answer.encode()).as_bytes())
            .is_err()
        {
            break;
        }
    }
}

/// Hands the free memory of every glibc heap back to the kernel (`malloc_trim(0)`); whether any was. A conversion
/// of a document with large pictures frees most of what it took, but glibc keeps it: its mmap threshold rises with
/// the largest block freed, so later large blocks come from the heap and stay there, and the instance would sit
/// tens of MiB above its floor after every such document. On a soak of 140 typical reports with one instance, the trim
/// (2-20 ms a call) lowered the container's memory over the soak's second half by about 50 MiB, on amd64 and arm64
/// alike; a conversion took 2.3 % longer on a Xeon and no measurably longer on an Apple M1. The peak during a
/// conversion, which the memory limits are sized from, moved by 3-8 MiB.
#[cfg(all(target_os = "linux", target_env = "gnu"))]
#[allow(unsafe_code, reason = "malloc_trim has no std form")]
fn trim_heap() -> bool {
    // SAFETY: malloc_trim takes no pointer; it only walks and releases the allocator's own free memory.
    unsafe { libc::malloc_trim(0) == 1 }
}

/// No glibc heap to trim outside Linux with glibc (a development build on macOS).
#[cfg(not(all(target_os = "linux", target_env = "gnu")))]
fn trim_heap() -> bool {
    false
}

/// A copy of file descriptor 1 for the protocol, with descriptor 1 itself pointed at stderr.
#[allow(
    unsafe_code,
    reason = "`_exit` and descriptor duplication, which std has no form of"
)]
fn private_stdout() -> std::io::Result<File> {
    // SAFETY: plain descriptor calls; the duplicate is owned by the returned File and nothing else.
    unsafe {
        let copy = libc::dup(1);
        if copy < 0 {
            return Err(std::io::Error::last_os_error());
        }
        if libc::fcntl(copy, libc::F_SETFD, libc::FD_CLOEXEC) < 0 || libc::dup2(2, 1) < 0 {
            let e = std::io::Error::last_os_error();
            libc::close(copy);
            return Err(e);
        }
        Ok(File::from_raw_fd(copy))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::time::Duration;

    #[test]
    fn the_resolution_cap_is_an_optional_flag_before_the_profile() {
        let args = |a: &[&str]| {
            a.iter()
                .map(std::string::ToString::to_string)
                .collect::<Vec<_>>()
        };
        assert_eq!(parse_args(&args(&["/p"])), Some((None, Path::new("/p"))));
        assert_eq!(
            parse_args(&args(&["--max-image-dpi=300", "/p"])),
            Some((Some(300), Path::new("/p")))
        );
        assert_eq!(parse_args(&args(&["--max-image-dpi=x", "/p"])), None);
        assert_eq!(parse_args(&args(&["--other", "/p"])), None);
        assert_eq!(parse_args(&args(&[])), None);
    }

    #[test]
    fn the_profile_switches_lock_files_off() {
        let dir = tempfile::tempdir().unwrap();
        prepare_profile(dir.path(), None).unwrap();
        let xcu =
            std::fs::read_to_string(dir.path().join("user/registrymodifications.xcu")).unwrap();
        assert!(xcu.contains(r#"oor:path="/org.openoffice.Office.Common/Misc""#));
        assert!(
            xcu.contains(
                r#"<prop oor:name="UseLocking" oor:op="fuse"><value>false</value></prop>"#
            )
        );
    }

    #[test]
    fn the_profile_blocks_linked_content_and_points_http_at_a_closed_port() {
        for max_image_dpi in [None, Some(300)] {
            let xcu = registry_modifications(max_image_dpi);
            assert!(xcu.contains(
                r#"<item oor:path="/org.openoffice.Office.Common/Security/Scripting"><prop oor:name="BlockUntrustedRefererLinks" oor:op="fuse"><value>true</value></prop></item>"#
            ));
            assert!(xcu.contains(r#"oor:path="/org.openoffice.Inet/Settings""#));
            for (name, value) in [
                ("ooInetProxyType", "1"),
                ("ooInetHTTPProxyName", "127.0.0.1"),
                ("ooInetHTTPProxyPort", "9"),
                ("ooInetHTTPSProxyName", "127.0.0.1"),
                ("ooInetHTTPSProxyPort", "9"),
                ("ooInetNoProxy", ""),
            ] {
                assert!(
                    xcu.contains(&format!(
                        r#"<prop oor:name="{name}" oor:op="fuse"><value>{value}</value></prop>"#
                    )),
                    "{name}"
                );
            }
        }
    }

    /// What a request loop did, in order: each conversion, each trim and each answer line written.
    struct Recorder<'a>(&'a RefCell<Vec<String>>);

    impl Write for Recorder<'_> {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            let line = String::from_utf8_lossy(buf);
            self.0
                .borrow_mut()
                .push(format!("answer {}", line.trim_end()));
            Ok(buf.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn the_heap_is_trimmed_after_each_conversion_before_its_answer_is_written() {
        let events = RefCell::new(Vec::new());
        let requests = "convert\t/in/a.docx\t/out/a.pdf\nconvert\t/in/b.docx\t/out/b.pdf\n";
        serve_requests(
            requests.as_bytes(),
            &mut Recorder(&events),
            |input, _| {
                events
                    .borrow_mut()
                    .push(format!("convert {}", input.display()));
                Answer::Converted {
                    load: Duration::from_millis(1),
                    export: Duration::from_millis(2),
                }
            },
            || events.borrow_mut().push("trim".to_owned()),
        );
        assert_eq!(
            events.into_inner(),
            [
                "convert /in/a.docx",
                "trim",
                "answer ok\t1\t2",
                "convert /in/b.docx",
                "trim",
                "answer ok\t1\t2",
            ]
        );
    }

    #[test]
    fn a_request_line_it_cannot_read_is_answered_without_a_conversion() {
        let events = RefCell::new(Vec::new());
        serve_requests(
            &b"nonsense\n"[..],
            &mut Recorder(&events),
            |_, _| unreachable!("no conversion for a bad request"),
            || events.borrow_mut().push("trim".to_owned()),
        );
        let events = events.into_inner();
        assert_eq!(events.len(), 1, "{events:?}");
        assert!(events[0].starts_with("answer bad_request\t"), "{events:?}");
    }

    /// The trim is glibc's; this shows it runs in the process and hands back the free memory it finds.
    #[cfg(all(target_os = "linux", target_env = "gnu"))]
    #[test]
    fn the_trim_hands_free_heap_memory_back_to_the_kernel() {
        // Many small blocks (below the mmap threshold, so they come from the heap), freed with one kept at the end so
        // free() itself cannot shrink the heap: the freed pages stay with the allocator until a trim.
        let blocks: Vec<Vec<u8>> = (0..20_000)
            .map(|i| vec![u8::try_from(i % 251).unwrap(); 1024])
            .collect();
        let keep = vec![1_u8; 1024];
        drop(blocks);
        assert!(
            trim_heap(),
            "malloc_trim(0) found no free memory to hand back"
        );
        drop(keep);
    }

    const PDF_EXPORT: &str = r#"oor:path="/org.openoffice.Office.Common/Filter/PDF/Export""#;

    #[test]
    fn without_a_maximum_the_profile_embeds_pictures_as_they_are() {
        let xcu = registry_modifications(None);
        assert!(xcu.contains(PDF_EXPORT));
        assert!(xcu.contains(
            r#"<prop oor:name="ReduceImageResolution" oor:op="fuse"><value>false</value></prop>"#
        ));
        assert!(!xcu.contains("MaxImageResolution"));
    }

    #[test]
    fn with_a_maximum_the_profile_reduces_pictures_above_it() {
        let xcu = registry_modifications(Some(150));
        assert!(xcu.contains(PDF_EXPORT));
        assert!(xcu.contains(
            r#"<prop oor:name="ReduceImageResolution" oor:op="fuse"><value>true</value></prop>"#
        ));
        assert!(xcu.contains(
            r#"<prop oor:name="MaxImageResolution" oor:op="fuse"><value>150</value></prop>"#
        ));
    }
}
