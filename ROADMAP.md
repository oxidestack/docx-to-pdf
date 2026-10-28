# Roadmap

Where docx-to-pdf is and where it is going. docx-to-pdf converts office documents to PDF with LibreOffice over HTTP,
with the interface of [moalhaddar/docx-to-pdf](https://github.com/moalhaddar/docx-to-pdf) 2.1.0-13, so that a client
of moalhaddar/docx-to-pdf moves to it without a change. The stages below run in order; a finished stage carries
the month it was finished.

To suggest something for the roadmap, start a discussion in
[Ideas](https://github.com/oxidestack/docx-to-pdf/discussions/categories/ideas). [GOVERNANCE.md](GOVERNANCE.md) says
how a larger change is decided.

## Done

### The reference, recorded — October 2026

What moalhaddar/docx-to-pdf answers to every request it accepts, recorded as scenarios that every build of
docx-to-pdf replays; a comparison of two PDFs by their pages, text and rendering, so that a test tells a different
document from a different timestamp.

### The service — October 2026

The HTTP interface and conversions of moalhaddar/docx-to-pdf, made reliable: a pool of LibreOffice instances that
replaces a failed or stuck instance, retries a conversion it lost, times out a conversion that hangs and recycles an
instance after a number of conversions; an answer with a clear status for every error; the formats allowed in;
configuration by environment variables; OpenTelemetry traces, logs and metrics from the first request.

### The image — October 2026

A container image for linux/amd64 and linux/arm64 with a current LibreOffice and free fonts, running as a non-root
user on a read-only root file system; conversion and pool telemetry; every recorded scenario replayed against the
image.

### Conformance at scale — October 2026

A corpus of real and generated documents converted by both services and held to an approved rendering; the image
checked on amd64; latency, throughput and memory measured per kind of document and the defaults set from the
measurements; limits that keep a hostile document — a decompression bomb, a gigapixel picture, a linked resource on
the network — from taking the service down.

## Now: publication

- The repository public on GitHub under Apache-2.0, with the documentation under CC-BY-4.0, a documentation site, and
  user guides to install, configure, operate, troubleshoot and migrate to the service.
- GitHub Actions on every push and pull request: every quality gate, every test, and the image built and tested
  natively on amd64 and arm64.
- A Helm chart with probes, a security context, limits and a NetworkPolicy.
- Repository security: code scanning, OpenSSF Scorecard, dependency review, actions pinned by commit, branch
  protection that requires every check, and a sign-off (DCO) check on every pull request.

## Next: the first release

- One version tag publishes everything: the image on `quay.io/oxidestack/docx-to-pdf` for both architectures, the
  Helm chart on quay.io as an OCI artifact, and a GitHub release with its notes, SBOMs, signatures and provenance.
  Both are mirrored to ghcr.io under the same paths and digests, signed the same way; a release stands on quay.io.
- An application to the CNCF Sandbox, with the project's entry in the Cloud Native Landscape.

## Later

- An OpenSSF Best Practices badge and an Artifact Hub listing for the chart.
- More maintainers, from more than one organisation ([GOVERNANCE.md](GOVERNANCE.md)).
- What adopters ask for: a document that converts badly is the most useful report there is.
