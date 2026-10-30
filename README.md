# docx-to-pdf

[![CI](https://github.com/oxidestack/docx-to-pdf/actions/workflows/ci.yml/badge.svg?branch=main)](https://github.com/oxidestack/docx-to-pdf/actions/workflows/ci.yml?query=branch%3Amain)
[![OpenSSF Scorecard](https://api.scorecard.dev/projects/github.com/oxidestack/docx-to-pdf/badge)](https://scorecard.dev/viewer/?uri=github.com/oxidestack/docx-to-pdf)
[![OpenSSF Best Practices](https://www.bestpractices.dev/projects/16076/badge)](https://www.bestpractices.dev/projects/16076)

**Documentation: [oxidestack.github.io/docx-to-pdf](https://oxidestack.github.io/docx-to-pdf/)**

<!-- benchmarks:begin: `make evidence` writes this block from site/public/benchmarks/results.json -->

> **docx-to-pdf turns office documents into PDF faster and with less memory than Gotenberg and moalhaddar/docx-to-pdf on
> the same documents, and no document — however large, malicious or broken — can take it down, hold it past its timeout
> or make it reach the network.**

- **Less CPU than Gotenberg**: Gotenberg uses 1.21 ± 0.02× the CPU a conversion as it ships, and 1.13 ± 0.04× without
  its restarts ([the row](https://oxidestack.github.io/docx-to-pdf/benchmarks/#ratio-cpu)).
- **Less memory than Gotenberg**: its container peaks at 1.65 ± 0.03× the memory as it ships, and 1.86 ± 0.04× without
  its restarts ([the row](https://oxidestack.github.io/docx-to-pdf/benchmarks/#ratio-peak)).
- **The service it replaces, outrun**: 3.24 ± 0.06× moalhaddar/docx-to-pdf's throughput, which uses 3.32 ± 0.06× the CPU
  a conversion and 2.27 ± 0.07× the memory
  ([the row](https://oxidestack.github.io/docx-to-pdf/benchmarks/#ratio-throughput)).
- **A deflate bomb stopped at once**: `pages-500000p`, a document that inflates past any limit, gets 413 in 0.01 s at a
  236 MiB peak; Gotenberg answers 503 in 30 s while its LibreOffice goes on to 7 609 MiB, and moalhaddar/docx-to-pdf
  gives no answer in 300.04 s at 9 373 MiB, one run a side
  ([the row](https://oxidestack.github.io/docx-to-pdf/benchmarks/#torture-pages-500000p)).

Measured by `make compare` on 2026-10-29 17:16 UTC, on an Intel(R) Xeon(R) Gold 6248R CPU @ 3.00GHz (x86_64), 1 pinned
vCPU and a pool of 1 a service: mean ± σ (n) over 3 interleaved blocks, a ratio ± its standard error. The image measured
is release 1.0.0, pulled by digest. [Benchmarks](https://oxidestack.github.io/docx-to-pdf/benchmarks/) has every figure,
the rows where Gotenberg wins, and the raw results.

<!-- benchmarks:end -->

An HTTP service that turns an office document into a PDF with LibreOffice.

It answers the same requests as [moalhaddar/docx-to-pdf](https://github.com/moalhaddar/docx-to-pdf) 2.1.0-13, so a
client of that service moves to this one without changing a line, and it is built to stay up under load that
service does not survive: a document that never finishes converting, a request that never finishes uploading, a
file that is not a document, a burst of more requests than there are LibreOffice processes. It is written in Rust,
ships a current LibreOffice, and reports what it does over OpenTelemetry.

## Quick start

```sh
docker run --rm -p 8080:8080 quay.io/oxidestack/docx-to-pdf:1.1.0
```

In another terminal, once it has started:

```sh
curl -F document=@report.docx http://localhost:8080/pdf -o report.pdf
```

`report.pdf` is the document as LibreOffice lays it out. A test replays this `curl` against the service.

Every release is signed and carries its SBOM and build provenance;
[Verifying a release](docs/guide/installation.md#verifying-a-release) checks an image or a chart with `cosign` and
`gh attestation verify`.

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

The guide is published at [oxidestack.github.io/docx-to-pdf](https://oxidestack.github.io/docx-to-pdf/); the same
pages are in `docs/guide/`:

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
- `make lint` runs every quality gate the CI runs, each in its own pinned container; CONTRIBUTING.md's
  [checks](CONTRIBUTING.md#the-checks) lists them, and the CI badge above is their state on `main`.

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
