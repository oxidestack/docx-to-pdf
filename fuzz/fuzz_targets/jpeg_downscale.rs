//! The jpeg downscale on any input: it may refuse, never panic. The first byte picks the bound: a factor from 1 to
//! past 8 of the bitmap the picture's header claims.
#![no_main]

libfuzzer_sys::fuzz_target!(|data: &[u8]| {
    let Some((&pick, picture)) = data.split_first() else {
        return;
    };
    let bitmap = docx_to_pdf::pictures::header(picture).map_or(1 << 16, |h| h.bitmap());
    let bound = (bitmap >> (2 * u32::from(pick % 8))).max(1);
    let _ = docx_to_pdf::pictures::jpeg::downscale(picture, std::io::sink(), bound);
});
