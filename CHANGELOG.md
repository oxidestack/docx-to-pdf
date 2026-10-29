# Changelog

<!-- markdownlint-configure-file { "MD013": false, "MD024": { "siblings_only": true } } -->

All notable changes to docx-to-pdf, newest release first. The layout is
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/)'s, with sections for the people who run the service: Action
required, Features, Bug fixes, Performance and Other changes. The versions follow
[Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## Unreleased

The changes since the last release are the release notes of the commits after its tag; a release gathers them into
its section below.

## [1.0.0] - 2026-10-29

The first release. docx-to-pdf is an HTTP service that turns office documents (DOCX, DOC, ODT and RTF) into PDF with
LibreOffice. A pool of LibreOffice instances, each in a process of its own, converts one document at a time. Every
conversion has a timeout and a memory limit, and a document that runs past either is stopped without touching the
other conversions. Documents are checked before LibreOffice sees them: a format outside the allowlist, a deflate or
picture bomb, or a document past the size limit is refused with an RFC 9457 problem answer, and no document can make
LibreOffice reach the network. Health checks, Prometheus metrics and OpenTelemetry traces and logs come built in.

It ships as a multi-architecture image (linux/amd64, linux/arm64) on `quay.io/oxidestack/docx-to-pdf`, mirrored to
`ghcr.io/oxidestack/docx-to-pdf`, and as a Helm chart on `oci://quay.io/oxidestack/charts/docx-to-pdf`. Both are
signed with cosign and carry SLSA provenance; the image carries an SPDX SBOM for each architecture.

### Performance

- An instance returns the memory a conversion freed to the system before it answers, so a pod converting
  picture-heavy documents sits about 50 MiB lower between conversions. Peak memory and the sizing formula are
  unchanged. (instance, 04ebdc7)

[1.0.0]: https://github.com/oxidestack/docx-to-pdf/releases/tag/v1.0.0
