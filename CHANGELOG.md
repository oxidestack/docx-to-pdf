# Changelog

<!-- markdownlint-configure-file { "MD013": false, "MD024": { "siblings_only": true } } -->

All notable changes to docx-to-pdf, newest release first. The layout is
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/)'s, with sections for the people who run the service: Action
required, Features, Bug fixes, Performance and Other changes. The versions follow
[Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## Unreleased

The changes since the last release are the release notes of the commits after its tag; a release gathers them into
its section below.

## [1.3.0] - 2026-11-13

A log line for every request without OTLP, and an image rebuilt on Debian's fixes for apt and zlib. Nothing to change in
a deployment; the image and the chart upgrade in place from 1.2.1.

### Features

- Every request now leaves one `info` line on standard output when it ends, without OTLP: the method, the route, the
  status code, the duration, the request's and the answer's body sizes, and `error.type` when it failed, with the
  request's `trace_id` and `span_id`. The probes (`/livez`, `/readyz`, `/metrics`) are logged at `debug` only. Set
  `DOCX_TO_PDF_ACCESS_LOG=false` to turn the line off. (telemetry, 15737ae)

### Other changes

- The image's Debian packages come from the snapshot of 9 November 2026, which fixes the advisories in apt and zlib,
  and its LibreOffice is 26.8.2. (deps, 45eeebc)

[1.3.0]: https://github.com/oxidestack/docx-to-pdf/compare/v1.2.1...v1.3.0

## [1.2.1] - 2026-11-01

The whole guide in Russian, Chinese and Spanish, and the chart's logo and recommendations on Artifact Hub. Nothing to
change in a deployment; the image and the chart upgrade in place from 1.2.0.

### Other changes

- Every page of the guide is translated into Russian, Chinese and Spanish: the documentation site's language picker
  shows each page in the reader's language, and on GitHub each page links its translations. A translation records the
  version of the English page it translates, and a page whose original has changed since says it may be out of date.
  (3591235)
- The Helm chart carries the project's logo, which Artifact Hub shows on its page. (chart, 2993b4e)
- The chart's Artifact Hub page recommends the OpenTelemetry Collector and kube-prometheus-stack charts, which receive
  the service's telemetry. (chart, 8e8daed)

[1.2.1]: https://github.com/oxidestack/docx-to-pdf/compare/v1.2.0...v1.2.1

## [1.2.0] - 2026-10-31

The documentation, by release and in four languages: the site serves each release's documentation with a version
picker, a new upgrade guide says what each release asks of a deployment, and the home page and the upgrade guide are
in Russian, Chinese and Spanish too. An Excel spreadsheet embedded in a document now reaches the PDF, and the chart is
listed on Artifact Hub again. Nothing to change in a deployment; the image and the chart upgrade in place from 1.1.0.

### Bug fixes

- An Excel spreadsheet embedded in a document is drawn with its cells. It was left out of the PDF, with neither its
  cells nor its preview picture, because the image did not carry LibreOffice Calc. (image, 616d2f3)
- The Helm chart is listed on Artifact Hub again: its 1.1.0 was refused for a category Artifact Hub does not accept
  yet, which the chart no longer names. (chart, e345065)
- The installation guide and the release notes say that `cosign verify-blob` needs cosign 3 or later for the release
  files' Sigstore bundles (cosign 2 with `--new-bundle-format`). (release, 2885366)
- The Benchmarks page's soak verdict says "grows" only when memory still rises over the soak's second half by at least
  1 % of LibreOffice's level per 1000 conversions: release 1.0.0's soak, +0.5 MiB per 1000 at 391 MiB, now reads "does
  not grow", and a rise the second half cannot confirm reads "rose, second half unresolved". (bench, 1bf99fb)

### Other changes

- The documentation site at <https://oxidestack.github.io/docx-to-pdf/> serves the latest release's documentation at
  its root and every release's under its version, with a version picker on every page and, on an older release, a
  banner linking the same page in the latest. The new
  [Upgrading](https://oxidestack.github.io/docx-to-pdf/upgrading/) page says what each release asks of a deployment.
  (eaaa257)
- The site has a language picker: EN (English), RU (Русский), ZH (中文) and ES (Español). The home page and the upgrade
  guide are translated; every other page is shown in English under each language until it is. (e35b01d)

[1.2.0]: https://github.com/oxidestack/docx-to-pdf/compare/v1.1.0...v1.2.0

## [1.1.0] - 2026-10-29

A small release: a fix for a malformed PNG picture, and release files you can verify. Nothing to change in a
deployment; the image and the chart upgrade in place from 1.0.0.

### Features

- Every file attached to a GitHub release (the SBOMs, the chart's archive, the compose examples and the Helm
  repository's `index.yaml`) has a Sigstore bundle beside it, `<file>.sigstore.json`, and the release carries their
  SLSA build provenance, `docx-to-pdf-<version>.intoto.jsonl`: `cosign verify-blob` and `gh attestation verify` check
  them, as the installation guide's "Verifying a release" shows. (release, 91f640f)
- The chart is described for Artifact Hub: its category, links to the documentation, the benchmarks and the issues,
  and a changelog of each chart version written from the release's notes. (chart, 813e500)

### Bug fixes

- A PNG picture too large to decode in memory whose palette is malformed (its length not a multiple of 3, or more
  entries than its bit depth can index) is refused before it is decoded and replaced by a placeholder, as any picture
  that cannot be scaled down safely is. The PDF is the same as before, when the PNG decoder panicked on it and the
  service caught the panic. (pictures, 0c34312)

[1.1.0]: https://github.com/oxidestack/docx-to-pdf/compare/v1.0.0...v1.1.0

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
