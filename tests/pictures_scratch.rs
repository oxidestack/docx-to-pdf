//! A progressive JPEG's scan copy (the first of its two passes) goes to the pictures step's scratch directory,
//! like every other scratch file of the step, and not to TMPDIR. This binary points TMPDIR at a directory that does
//! not exist, so a scratch file made there fails and the picture would keep its placeholder.

use std::io::{Cursor, Write};

use docx_to_pdf::pictures::{Outcome, replace_oversized};

const CT: &[u8] = br#"<Types><Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/></Types>"#;

/// A 4:2:0 progressive JPEG of `width × height`, a gradient.
fn progressive_jpeg(width: u16, height: u16) -> Vec<u8> {
    let mut out = Vec::new();
    let mut encoder = jpeg_encoder::Encoder::new(&mut out, 80);
    encoder.set_sampling_factor(jpeg_encoder::SamplingFactor::F_2_2);
    encoder.set_progressive(true);
    let pixels: Vec<u8> = (0..u32::from(height))
        .flat_map(|y| {
            (0..u32::from(width)).flat_map(move |x| [x.to_le_bytes()[0], y.to_le_bytes()[0], 7])
        })
        .collect();
    encoder
        .encode(&pixels, width, height, jpeg_encoder::ColorType::Rgb)
        .unwrap();
    out
}

fn docx(name: &str, picture: &[u8]) -> Vec<u8> {
    use zip::write::SimpleFileOptions;
    let mut writer = zip::ZipWriter::new(Cursor::new(Vec::new()));
    let deflated =
        SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);
    for (name, content) in [
        ("[Content_Types].xml", CT),
        ("word/document.xml", b"<w:document/>".as_slice()),
        (name, picture),
    ] {
        writer.start_file(name, deflated).unwrap();
        writer.write_all(content).unwrap();
    }
    writer.finish().unwrap().into_inner()
}

#[test]
#[allow(unsafe_code, reason = "edition 2024's environment calls are unsafe")]
fn the_scan_copy_of_a_progressive_jpeg_is_made_in_the_scratch_directory_not_in_tmpdir() {
    let target = std::path::Path::new(env!("CARGO_TARGET_TMPDIR"));
    let scratch = tempfile::tempdir_in(target).unwrap();
    let missing = scratch.path().join("no-such-tmpdir");
    // SAFETY: this binary's only test sets TMPDIR before anything in it reads the environment or starts a thread.
    unsafe { std::env::set_var("TMPDIR", &missing) };
    assert!(
        tempfile::tempfile().is_err(),
        "a scratch file in TMPDIR fails here"
    );

    let zip = docx("word/media/image1.jpeg", &progressive_jpeg(400, 300));
    let mut out = Vec::new();
    let changed =
        replace_oversized(&mut Cursor::new(zip), &mut out, 50_000, scratch.path()).unwrap();
    assert_eq!(changed.len(), 1);
    assert!(
        matches!(changed[0].outcome, Outcome::Downscaled(_)),
        "downscaled with its scan copy in the scratch directory: {:?}",
        changed[0].outcome
    );
    assert_eq!(
        std::fs::read_dir(scratch.path()).unwrap().count(),
        0,
        "no scratch file left behind"
    );
}
