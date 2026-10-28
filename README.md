# docx-to-pdf

An HTTP service that turns an office document into a PDF with LibreOffice.

It answers the same requests as [moalhaddar/docx-to-pdf](https://github.com/moalhaddar/docx-to-pdf) 2.1.0-13, so a
client of that service moves to this one without changing a line, and it is built to stay up under load that
service does not survive: a document that never finishes converting, a request that never finishes uploading, a
file that is not a document, a burst of more requests than there are LibreOffice processes. It is written in Rust,
ships a current LibreOffice, and reports what it does over OpenTelemetry.

## Quick start

```sh
docker run --rm -p 8080:8080 quay.io/oxidestack/docx-to-pdf:0.1.0
```

In another terminal, once it has started:

```sh
curl -F document=@report.docx http://localhost:8080/pdf -o report.pdf
```

`report.pdf` is the document as LibreOffice lays it out. A test replays this `curl` against the service.

## What it gives

- **Word and OpenDocument text in, PDF out**: DOCX, DOCM, DOTX, DOC, ODT, OTT, flat ODT and RTF. The format is
  decided from the document's bytes, not its name; anything else is refused with `415` before any conversion starts.
- **The interface of moalhaddar/docx-to-pdf**: `POST /pdf` and `POST /docx-to-pdf`, the document in a multipart file
  part named `document`, the PDF as the answer.
- **Reliability**: a time limit on every conversion, a bounded wait for a free LibreOffice process, limits on the
  size of a document and of what it inflates to, LibreOffice processes replaced when they crash, hang or have run
  a set number of conversions, and a graceful shutdown that finishes the conversions in progress. Every error is an
  [RFC 9457](https://www.rfc-editor.org/rfc/rfc9457) problem document with a status that says what went wrong.
- **Telemetry**: traces, logs and metrics over OTLP, Prometheus metrics at `/metrics`, and the health checks
  `/livez` and `/readyz` for Kubernetes.
- **A locked-down container** for `linux/amd64` and `linux/arm64`: a non-root user, a read-only root file system, no
  Linux capabilities, and only free fonts, metric-compatible with the common Microsoft ones.

## Documentation

- [Installation](docs/guide/installation.md) — `docker run`, Docker Compose and Kubernetes with the Helm chart, and the
  resources to give it
- [Configuration](docs/guide/configuration.md) — every environment variable, its default and its effect
- [API reference](docs/guide/api.md) — the endpoints, the formats accepted, every status and its detail
- [Operations](docs/guide/operations.md) — the container, health checks, Kubernetes, telemetry and metrics, limits,
  sizing and fonts
- [Troubleshooting](docs/guide/troubleshooting.md) — every error answer and other problems, what each means and what to
  do
- [Migrating from moalhaddar/docx-to-pdf](docs/guide/migration.md) — what stays the same, what a client or a
  deployment changes, and every deliberate difference
- [Changelog](CHANGELOG.md) — what each release changed, and what an operator has to do when a release asks it
- [Compared with Gotenberg and moalhaddar/docx-to-pdf](docs/guide/comparison.md) — what each does, speed, memory and
  hostile documents, from the measurements on record, and the commands that repeat them

## Building and testing

The repository carries moalhaddar/docx-to-pdf as a git submodule, which the tests read, so clone it with its
submodules:

```sh
git clone --recursive https://github.com/oxidestack/docx-to-pdf.git
```

In a clone made without `--recursive`, run `git submodule update --init` once.

- `make image` builds the container image for your machine's architecture as `docx-to-pdf:dev`.
- `make check` runs formatting, clippy and the library tests. `cargo test` runs every test; it needs a Rust toolchain
  and poppler on `PATH` (`pdfinfo`, `pdftotext`, `pdftoppm`: `brew install poppler` on macOS,
  `apt-get install poppler-utils` on Debian), and the tests that convert real documents need LibreOffice.
  `tools/instance-test/run.sh` runs them in a Linux container that carries all three.

## Getting help

Questions go to [Discussions](https://github.com/oxidestack/docx-to-pdf/discussions), bugs to
[issues](https://github.com/oxidestack/docx-to-pdf/issues/new/choose), vulnerabilities to a private report
([SECURITY.md](SECURITY.md)). [CONTRIBUTING.md](CONTRIBUTING.md) says how to build, test and send a change.

## Licence

The code is licensed under the [Apache License, Version 2.0](LICENSE) (`Apache-2.0`). The documentation — this README,
the guides under `docs/` and the chart's README — is licensed under
[Creative Commons Attribution 4.0 International](https://creativecommons.org/licenses/by/4.0/) (`CC-BY-4.0`). The
material taken from moalhaddar/docx-to-pdf, the fonts and LibreOffice keep their own licences and are attributed in
[`NOTICE`](NOTICE).
