# The service's image: LibreOffice 26.8 from trixie-backports, Debian 13's packaged LibreOffice; free fonts only (a
# font whose licence forbids redistribution never enters the image), chosen and configured so that LibreOffice 26.8 on
# fontconfig 2.15 substitutes fonts as moalhaddar/docx-to-pdf does; the binary; a non-root user. One Dockerfile for
# linux/amd64 and linux/arm64: the packages come from the same dated snapshot on both, the binary is cross-compiled
# on the build platform, so an amd64 image built on Apple Silicon runs only apt and fc-cache under emulation, never
# rustc.
#
#   make image                 the host's architecture, loaded as docx-to-pdf:dev
#   make image-multiarch       both architectures, the build cache only (PUSH=<ref> pushes the manifest)
#   make image-test            image/test.sh against docx-to-pdf:dev
#
# Build arguments: SNAPSHOT moves LibreOffice and every package together, so a bump is a deliberate change, never a
# side effect of a rebuild; EXTRA_FONTS=0 leaves out the broad free set beyond the packages that match
# moalhaddar/docx-to-pdf's substitutions (only to measure its cost).
ARG SNAPSHOT=20261026T202550Z

# rust:1.98.0-trixie, by digest (an index: the build platform's manifest is picked).
FROM --platform=$BUILDPLATFORM rust@sha256:620dbcd124499c59e2406d3741574b5c5838cf9eb9656f0c3a03948f79b02959 AS build
ARG TARGETARCH
ARG BUILDARCH
# DL3008: the cross toolchain is whatever rust's trixie base resolves; the build stage ships nothing of it.
# hadolint ignore=DL3008
RUN if [ "$TARGETARCH" != "$BUILDARCH" ] && [ "$TARGETARCH" = amd64 ]; then \
      apt-get -o Acquire::Retries=3 update && apt-get -o Acquire::Retries=3 install -y --no-install-recommends gcc-x86-64-linux-gnu libc6-dev-amd64-cross \
      && rustup target add x86_64-unknown-linux-gnu; \
    elif [ "$TARGETARCH" != "$BUILDARCH" ]; then \
      apt-get -o Acquire::Retries=3 update && apt-get -o Acquire::Retries=3 install -y --no-install-recommends gcc-aarch64-linux-gnu libc6-dev-arm64-cross \
      && rustup target add aarch64-unknown-linux-gnu; \
    fi
# cargo-auditable embeds the crate list in the binary (a `.dep-v0` section), so Trivy scans the service's
# Rust dependencies in the image and syft's SBOM lists them.
RUN --mount=type=cache,id=docx-to-pdf-cargo-registry,target=/usr/local/cargo/registry \
    cargo install --locked --version 0.7.7 cargo-auditable
WORKDIR /src
COPY Cargo.toml Cargo.lock ./
COPY src src
# Nothing links LibreOffice at build time: the instance loads libmergedlo.so through LibreOfficeKit with dlopen.
RUN --mount=type=cache,id=docx-to-pdf-cargo-registry,target=/usr/local/cargo/registry \
    --mount=type=cache,id=docx-to-pdf-target-$TARGETARCH,target=/src/target \
    case "$TARGETARCH" in amd64) T=x86_64-unknown-linux-gnu ;; arm64) T=aarch64-unknown-linux-gnu ;; \
      *) echo "unsupported TARGETARCH=$TARGETARCH" >&2; exit 1 ;; esac \
 && CARGO_TARGET_X86_64_UNKNOWN_LINUX_GNU_LINKER=x86_64-linux-gnu-gcc \
    CARGO_TARGET_AARCH64_UNKNOWN_LINUX_GNU_LINKER=aarch64-linux-gnu-gcc \
    cargo auditable build --locked --release --bin docx-to-pdf --target "$T" \
 && cp "target/$T/release/docx-to-pdf" /docx-to-pdf

# debian:13-slim (trixie-slim), by digest: the base the LibreOfficeKit instance and the font substitutions were
# proven on.
FROM debian@sha256:62bd423aeee912639ca8ee681c2b762cb3e25137e0a92b20be455898a9f034ad
ARG SNAPSHOT
ARG EXTRA_FONTS=1
# Packages from one dated snapshot: trixie, its updates, its security updates and its backports. Trivy found fixed
# Debian security advisories missing (expat), because trixie-security, a separate archive on snapshot.debian.org, was
# not a source; it is now, at the same date, and the base image's packages are upgraded to it. LibreOffice (writer,
# core, math, calc; no X11/GTK, no Python, no JRE) and OpenSymbol from backports (the default candidate is 25.2, so
# backports is asked for by name); math because without it formula objects are not drawn, calc because without it an
# embedded spreadsheet (an OLE object of class Excel.Sheet.12) draws nothing, neither its cells nor its preview
# picture: the import builds a Calc object from the embedded workbook and keeps no replacement graphic.
# moalhaddar/docx-to-pdf's font families from trixie. tini is PID 1: SIGTERM reaches the binary, and an orphan of a
# killed instance is reaped.
# EXTRA_FONTS adds the broad free coverage moalhaddar/docx-to-pdf lacks (CJK, UI families, emoji; OFL-1.1 /
# Apache-2.0): the more fonts, the fewer substitutions. libreoffice-uiconfig-writer, which writer-nogui does not pull
# in; without modules/swriter/ui/annotation.ui a LOK load of any document with a comment fails ("empty reference").
# LibreOffice's shared extension folder, outside every profile, exists in the image. uid 10001 never creates it, but
# the image run as root (`--user 0`) would race to: two first starts, the loser exits 1 (NameClashException). With
# the folder present LibreOffice skips the creation.
# DL3008: every version is pinned by SNAPSHOT, one dated snapshot.debian.org archive, not package by package.
# hadolint ignore=DL3008
RUN rm -f /etc/apt/sources.list.d/* /etc/apt/sources.list \
 && printf 'Types: deb\nURIs: http://snapshot.debian.org/archive/debian/%s/\nSuites: trixie trixie-updates trixie-backports\nComponents: main\nSigned-By: /usr/share/keyrings/debian-archive-keyring.gpg\n' "$SNAPSHOT" \
      > /etc/apt/sources.list.d/snapshot.sources \
 && printf 'Types: deb\nURIs: http://snapshot.debian.org/archive/debian-security/%s/\nSuites: trixie-security\nComponents: main\nSigned-By: /usr/share/keyrings/debian-archive-keyring.gpg\n' "$SNAPSHOT" \
      > /etc/apt/sources.list.d/snapshot-security.sources \
 && apt-get -o Acquire::Check-Valid-Until=false -o Acquire::Retries=3 update \
 && apt-get -o Acquire::Retries=3 upgrade -y --no-install-recommends \
 && apt-get -o Acquire::Retries=3 install -y --no-install-recommends -t trixie-backports \
      libreoffice-writer-nogui libreoffice-core-nogui libreoffice-math-nogui libreoffice-calc-nogui \
      libreoffice-uiconfig-writer fonts-opensymbol \
 && apt-get -o Acquire::Retries=3 install -y --no-install-recommends \
      tini fontconfig fonts-crosextra-carlito fonts-crosextra-caladea fonts-dejavu fonts-liberation \
      fonts-liberation-sans-narrow fonts-linuxlibertine fonts-noto-core fonts-noto-extra fonts-noto-mono \
      fonts-noto-ui-core fonts-sil-gentium fonts-sil-gentium-basic \
 && if [ "$EXTRA_FONTS" = 1 ]; then \
      apt-get -o Acquire::Retries=3 install -y --no-install-recommends \
        fonts-noto-cjk fonts-noto-cjk-extra fonts-noto-ui-extra fonts-noto-color-emoji; \
    fi \
 && apt-get clean && rm -rf /var/lib/apt/lists/* /var/log/apt /var/log/dpkg.log \
 && mkdir -p /usr/share/fonts/custom /usr/share/fonts/docx-to-pdf /var/spool/libreoffice/uno_packages/cache/uno_packages
# Upstream's free fonts (NOTICE; its other two families are commercial) live beside, not in, the operator's mount
# point /usr/share/fonts/custom, which stays empty so a volume there hides nothing.
COPY upstream/docx-to-pdf/fonts/DroidKufi-Regular.ttf upstream/docx-to-pdf/fonts/DroidKufi-Bold.ttf \
     upstream/docx-to-pdf/fonts/Cantarell-ExtraBold.otf /usr/share/fonts/docx-to-pdf/
# Drop the strong serif or sans that LibreOffice 24.8+ appends to its fontconfig query, which beats the metric aliases
# (Times became a serif generic, not Liberation Serif), and restore fontconfig 2.13's generic order, DejaVu first.
COPY tools/instance-test/fontconfig/09-docx-to-pdf-drop-lo-generic.conf /etc/fonts/conf.d/
COPY tools/instance-test/fontconfig/local.conf /etc/fonts/local.conf
# system-ui (where fontconfig 2.15 sends Segoe UI) prefers DejaVu Sans, as moalhaddar/docx-to-pdf resolves Segoe UI,
# not upstream's Cantarell Extra Bold, which fontconfig's own rules would put first.
COPY image/fontconfig/52-docx-to-pdf-system-ui.conf /etc/fonts/conf.d/
# fontconfig keeps each directory's mtime in its cache to the nanosecond, and an image layer keeps only whole
# seconds (and drops a change to a directory's mtime alone): without this, every system cache is stale at run time
# and each container's first scan rewrites them all under /tmp/.cache (4.7 MB, measured). Each directory's mtime is
# rounded down to its second, the value the layer will carry, before the cache is written.
# Dockle CIS-DI-0008: no file keeps a setuid or setgid bit. The service runs as uid 10001 and never
# changes user; su, mount, passwd and the like would only be a way up from it.
RUN find / -xdev -type f -perm /6000 -exec chmod ug-s {} + \
 && find /usr/share/fonts /usr/local/share/fonts -type d \
      -exec sh -c 'for d; do touch -c -d "@$(stat -c %Y "$d")" "$d"; done' sh {} + \
 && fc-cache -f
COPY --from=build /docx-to-pdf /usr/local/bin/docx-to-pdf
# The attribution and licence (NOTICE: upstream, its two free font families, LibreOffice); the Debian packages carry
# theirs in /usr/share/doc/<package>/copyright, and the Apache-2.0 text is /usr/share/common-licenses/Apache-2.0.
COPY LICENSE NOTICE /usr/share/doc/docx-to-pdf/
# The code's licence, as SPDX; the image's other contents are attributed in NOTICE.
LABEL org.opencontainers.image.licenses="Apache-2.0"
# The chart's security context (charts/docx-to-pdf): an arbitrary uid with no passwd entry, a read-only root
# filesystem, only /tmp writable. Profiles, conversions and fontconfig's cache for a mounted font directory all
# go under /tmp, which is on disk unless the deployment chooses tmpfs.
ENV HOME=/tmp TMPDIR=/tmp XDG_CACHE_HOME=/tmp/.cache
USER 10001:10001
EXPOSE 8080
# Probes: GET /livez and GET /readyz on 8080; the image carries no HTTP client for a HEALTHCHECK.
ENTRYPOINT ["/usr/bin/tini", "--", "/usr/local/bin/docx-to-pdf"]
