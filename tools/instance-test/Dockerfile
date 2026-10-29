# LibreOffice 26.8 as the service's image installs it (Debian 13's trixie-backports, the nogui packages), with a Rust
# toolchain, to run the LibreOfficeKit tests (`tests/*_lok.rs`) on Linux. The repository is mounted at run time; see
# run.sh. Built for the native platform.
# The moalhaddar/docx-to-pdf scenarios convert here too, so the fonts and fontconfig are the service image's (what
# restores moalhaddar/docx-to-pdf's font choices), and poppler-utils is there for PDF equivalence. Upstream's
# DroidKufi and Cantarell-ExtraBold: no commercially licensed font, and upstream's examples use them.
# The service image's fontconfig (the root Dockerfile) in full, system-ui conf included.
# poppler-utils (pdfinfo, pdftotext, pdftoppm) is what `tests/pdf_equivalence.rs` needs. The service's runtime image
# does not carry it.
# The snapshot, the packages, the fonts and the fontconfig files are tools/ci/test-deps.sh's, the one list the CI
# `test` job runs as well, so the two cannot drift apart. The paths it reads are assembled under /deps from the named
# build contexts run.sh passes (`ci`, `fonts`, `image-fontconfig`) and this directory, and it runs there as root.
FROM rust:1.98.0-trixie@sha256:620dbcd124499c59e2406d3741574b5c5838cf9eb9656f0c3a03948f79b02959
# DL3022 (three times): `ci`, `fonts` and `image-fontconfig` are named build contexts (run.sh), not stages.
# hadolint ignore=DL3022
COPY --from=ci test-deps.sh /deps/tools/ci/
COPY fontconfig/ /deps/tools/instance-test/fontconfig/
# hadolint ignore=DL3022
COPY --from=fonts DroidKufi-Regular.ttf DroidKufi-Bold.ttf Cantarell-ExtraBold.otf /deps/upstream/docx-to-pdf/fonts/
# hadolint ignore=DL3022
COPY --from=image-fontconfig 52-docx-to-pdf-system-ui.conf /deps/image/fontconfig/
WORKDIR /deps
RUN sh tools/ci/test-deps.sh full && rm -rf /deps /var/lib/apt/lists/*
ENV DOCX_TO_PDF_REQUIRE_LOK=1 CARGO_TARGET_DIR=/tmp/target
WORKDIR /src
# Every test: the LibreOfficeKit ones run here instead of skipping, moalhaddar/docx-to-pdf scenarios convert for real.
CMD ["sh", "-c", "/usr/lib/libreoffice/program/soffice --version && cargo test --locked --no-fail-fast -- --nocapture"]
