# `fuzz/`: two targets, named seeds that are the gate, and a soak that grows them

The targets are `fuzz_targets/png_downscale.rs` and `fuzz_targets/jpeg_downscale.rs`: the picture
downscales on any input, which may refuse it and must not panic. The first byte of an input picks the bound, the rest
is the picture.

A fuzzer is not a push gate: a short run finds nothing and is green whatever changed, a long one goes red on a commit
unrelated to what it finally reached. So the gate is the replay, and the fuzzer's job is to grow what is replayed.

## The gate: the seeds replay on every push

`fuzz/seeds/<target>/` is committed, and every file in it is named for what it is. `make lint ONLY=fuzz` (the
`fuzz` gate of `tools/lint/lint.sh`, the `lint-fuzz` job in CI) builds the targets on **stable** in the pinned rust
image, with debug assertions and overflow checks on as cargo-fuzz builds them, and runs every seed once through the
target binary itself, so the replay is the soak's own code. A panic aborts, libFuzzer names the input, and the gate
fails. A `[[bin]]` in `Cargo.toml` without a `seeds/` directory fails it too.

No AddressSanitizer there: it needs nightly, and the downscales are safe Rust. The soak below has it.

What the seeds are:

- `make-seeds.py` (Pillow, run by hand; the output is committed and deterministic) writes small PNGs and JPEGs of
  each layout the downscales branch on (grey, grey with alpha, 16-bit, bilevel, RGB, RGBA, palette; JPEG grey,
  4:2:0, 4:2:2, 4:4:4, progressive, CMYK) and malformed ones (a truncated IDAT or scan, an IHDR claiming
  65535 × 65535, the Adam7 flag with its CRC mended, nothing after the pick byte), each behind pick bytes 1 and 3.
- Every input that ever found something, minimised and named for the defect (below).

## The soak: cargo-fuzz on nightly, by hand

Not in the private CI: GitRiver 1.1.0 has no scheduled trigger, and a soak on every push is the flaky gate above. The
scheduled one is to be a weekly GitHub workflow. Locally:

```sh
rustup toolchain install nightly
cargo install cargo-fuzz --locked
# The first directory is where libFuzzer writes what it finds (ignored); the seeds are read too.
RUSTUP_TOOLCHAIN=nightly cargo fuzz run png_downscale corpus/png_downscale seeds/png_downscale -- \
    -max_total_time=900 -timeout=10 -rss_limit_mb=2048 -max_len=300000
RUSTUP_TOOLCHAIN=nightly cargo fuzz run jpeg_downscale corpus/jpeg_downscale seeds/jpeg_downscale -- \
    -max_total_time=900 -timeout=10 -rss_limit_mb=2048 -max_len=300000
```

Run from `fuzz/`. `RUSTUP_TOOLCHAIN` rather than `cargo +nightly`, which does not reach cargo-fuzz's inner build. On a
Mac whose command-line tools lack the C++ headers, `CXXFLAGS=-I<sdk>/usr/include/c++/v1` builds libfuzzer-sys.
`corpus/` (the grown pool, SHA-1 names) and `artifacts/` (crashes) are ignored: the pool is a soak artefact, and
libFuzzer re-derives most of it from the seeds in seconds.

## A crash

1. Minimise it: `RUSTUP_TOOLCHAIN=nightly cargo fuzz tmin <target> artifacts/<target>/crash-…`.
2. A test named for the defect, beside the downscale in `src/pictures/` or in `tests/pictures_fuzz.rs`, with the
   minimised input inline; red first.
3. Fix it.
4. Commit the minimised input as `seeds/<target>/<what-it-is>` (pick byte included), so the gate replays it and the
   next soak mutates it.
