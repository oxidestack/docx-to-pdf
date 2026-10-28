# Compared with Gotenberg and moalhaddar/docx-to-pdf

Two other services turn office documents into PDF over HTTP with LibreOffice. **Gotenberg** is the most used: a
general document API around Chromium, LibreOffice and PDF tools. **moalhaddar/docx-to-pdf** is the service whose
interface this one keeps. This page says what each does, then compares speed, memory and what each does with a
hostile document.

Every figure on this page is a measurement on record, given with the machine, the dates and the conditions it was
taken under. A measured figure is the mean ± σ over its n replicates, σ being the sample standard deviation, and two
figures agree when their means are within 2·√(σ₁²/n₁ + σ₂²/n₂); where a figure is one run, the page says so. The
commands that repeat the measurements are at the end. Where the record has no measurement, the page says so. A test
holds every number here to the measurements it was quoted from.

The versions measured: Gotenberg 8.38.1, as it ships and with LibreOffice's restarts turned off
(`LIBREOFFICE_RESTART_AFTER=0`); moalhaddar/docx-to-pdf 2.1.0-13; oxidestack/docx-to-pdf built from this
repository on the day of each run.

## Which to choose

- **Gotenberg**, when the job is more than Word documents to PDF: HTML, URLs or Markdown through Chromium,
  spreadsheets and presentations, merging and splitting PDFs, PDF/A or PDF/UA, webhooks, or options on the request
  (page ranges, a watermark, a password). It does all of that, and has years in production behind it.
- **oxidestack/docx-to-pdf**, when the job is Word documents to PDF behind one HTTP call, at volume, from
  documents you do not control. On the workload measured it used less CPU (not yet shown in an interleaved run) and
  about half the memory of Gotenberg, and a hostile document is stopped at a limit instead of left running.
- **moalhaddar/docx-to-pdf**, only to keep exactly what it does today, LibreOffice 7.3's layout included. To leave
  it, the [migration guide](migration.md) lists what a client and a deployment change.

## What each does

Gotenberg's column is checked against its documentation and its source at the version measured.

| | Gotenberg 8.38.1 | moalhaddar/docx-to-pdf | oxidestack/docx-to-pdf |
| --- | --- | --- | --- |
| What it converts | every format LibreOffice opens (text documents, spreadsheets, presentations, drawings), and HTML, URLs and Markdown through Chromium | Word documents (DOCX, DOC, ODT, RTF and their variants); anything else is handed to LibreOffice unchecked | Word documents only: DOCX, DOCM, DOTX, DOC, ODT, OTT, flat ODT, RTF ([the formats accepted](api.md#the-formats-accepted)) |
| Conversion routes | `/forms/libreoffice/convert`, `/forms/chromium/convert/url`, `/forms/chromium/convert/html`, `/forms/chromium/convert/markdown`, and screenshots of each Chromium input | `POST /pdf`, `POST /docx-to-pdf` | `POST /pdf`, `POST /docx-to-pdf` |
| How the format is decided | the file name's extension | not checked | from the document's bytes; anything else is `415` before LibreOffice sees it |
| Options on a request | many on the LibreOffice route: page ranges, landscape, a password, PDF/A and PDF/UA, merging several files into one, a watermark, tagged PDF | none | none |
| PDF tools | merge, split, PDF/A and PDF/UA, metadata, bookmarks, attachments, Factur-X, flatten, optimize, watermark, stamp, rotate, encrypt (`/forms/pdfengines/…`) | none | none |
| Asynchronous work | webhooks; input fetched from a URL (`downloadFrom`) | none | none |
| Authentication, TLS | basic authentication or OIDC; TLS | not documented | none: put it behind your ingress |
| A conversion that runs too long | the API answers `503` at 30 s; LibreOffice goes on with the document | no timeout | killed at 180 s and answered `504`; the instance is replaced |
| A document that needs too much memory | no limit of its own | no limit of its own | the instance is killed past 2 GiB and the request answered `413`; a ZIP that inflates past 256 MiB is `413` before conversion |
| A picture too large for LibreOffice | dropped, answered `200` | dropped, answered `200` | downscaled, and the `Docx-To-Pdf-Pictures` header says so |
| A linked picture in a document | not fetched | fetched, from public and private addresses | not fetched |
| LibreOffice | 26.8.1, restarted every 10 conversions by default | 7.3 | 26.8, kept running |
| Telemetry | OpenTelemetry traces, metrics and logs; Prometheus | none | OpenTelemetry traces, metrics and logs; Prometheus `GET /metrics` |
| Images | amd64, arm64 and three more architectures | amd64 only | amd64 and arm64 |

The limits in oxidestack/docx-to-pdf's column are its defaults; every one is a setting
([Configuration](configuration.md)). The linked-picture row and the limit rows are measured in the torture test
below; Gotenberg's and moalhaddar/docx-to-pdf's are as each ships.

## Speed and memory

Measured on 2026-10-23 on a VMware VM of an Intel Xeon Gold 6248R (Cascade Lake; the model has 24 cores and 48
threads, a 3.0 GHz base and 4.0 GHz turbo clock and 35.75 MB of L3 cache): 8 vCPUs, seen by the guest as 8 cores of
one socket with 1 thread each, their placement on the physical cores left to the hypervisor; 32 GiB; Ubuntu 24.04
with kernel 6.8, Docker with cgroup v2. Each service had one pinned vCPU, one LibreOffice (pool 1), 16 GiB of memory
and `/tmp` on the VM's disk, with tracing off. Three clients sent the typical report mix in a closed loop: four
reports of 10-20 MB with 30-60 pictures each, 52 conversions a replicate after one warm-up per document, 3
replicates. CPU and memory come from the container's cgroup, latency from the load generator. The table gives the
mean ± σ over the 3 replicates. Every replicate converted 52 of 52 with no error.
oxidestack/docx-to-pdf ran at 07:34-07:36 UTC, Gotenberg at 08:50-09:05 and moalhaddar/docx-to-pdf at
08:52-09:07, Gotenberg's and moalhaddar/docx-to-pdf's cells shuffled together.

| | oxidestack/docx-to-pdf | Gotenberg, no restarts | Gotenberg, as shipped | moalhaddar/docx-to-pdf |
| --- | --- | --- | --- | --- |
| conversions/s | **0.998** ± 0.043 | 0.854 ± 0.025 | 0.762 ± 0.026 | 0.299 ± 0.004 |
| CPU-seconds a conversion | **0.933** ± 0.026 | 1.114 ± 0.022 | 1.191 ± 0.033 | 3.205 ± 0.002 |
| p50 latency, s | **2.85** ± 0.06 | 3.47 ± 0.06 | 3.58 ± 0.11 | 9.79 ± 0.08 |
| p95 latency, s | **3.34** ± 0.22 | 3.90 ± 0.19 | 4.61 ± 0.20 | 10.88 ± 0.07 |
| p99 latency, s (close to the slowest of 52) | **3.62** ± 0.17 | 4.00 ± 0.24 | 4.93 ± 0.42 | 11.18 ± 0.09 |
| container `memory.peak`, MiB | **411** ± 0.5 | 771 ± 63 | 725 ± 3 | 1 063 ± 110 |
| anonymous memory peak, MiB | **323** ± 0.7 | 655 ± 83 | 629 ± 13 | 925 ± 111 |
| written to disk a conversion, MB | 28.07 ± 0.003 | 28.15 ± 0.28 | 27.85 ± 0.10 | 29.81 ± 0.0004 |
| `docker run` to ready (polled every 0.5 s), s | 0.51 ± 0.02 | 1.01 ± 0.02 | 1.01 ± 0.01 | 13.28 ± 0.60 |

What the table can and cannot say:

- **Against moalhaddar/docx-to-pdf it is settled**: it used 3.434 ± 0.056 times the CPU of
  oxidestack/docx-to-pdf and gave 0.300 ± 0.008 of its throughput (the ratio of the means ± its standard error), far
  beyond the 14 % this VM type was seen to drift between hours.
- **Against Gotenberg the direction is consistent, but not proven.** oxidestack/docx-to-pdf's replicates ran
  75 minutes before Gotenberg's, so the runs were not interleaved, and a drift of the host in between would move all
  of Gotenberg's replicates together. Every gap in throughput, CPU, latency and memory passes the 2σ test, but the
  test sees only the replicates' repeatability, not a drift between them. Two facts on record bound the drift without
  excluding it: oxidestack/docx-to-pdf's own cost moved no more than about 4-8 % over the morning, and
  moalhaddar/docx-to-pdf ran inside Gotenberg's window as fast as a day earlier. What the record supports is that
  Gotenberg used 19 ± 2 % more CPU than oxidestack/docx-to-pdf without restarts and 28 ± 3 % more as shipped, on
  this workload and this VM. The interleaved run below measured it again.
- **p99 against Gotenberg without restarts differs only narrowly** (1.105 ± 0.049 of oxidestack/docx-to-pdf's;
  the difference, 0.378 s, against 2σ = 0.341 s); against Gotenberg as shipped it is 1.362 ± 0.076.
- **Memory is about half of Gotenberg's** (1.875 ± 0.089 and 1.765 ± 0.005 times oxidestack/docx-to-pdf's
  `memory.peak`). Gotenberg's figure is the whole container: its Go server, LibreOffice and its converter process.
- **Disk is about the same for all three**, 28-30 MB a conversion.
- **One vCPU, one LibreOffice, one document mix.** Gotenberg runs one LibreOffice per container, so this is the cell
  that compares directly; a larger pool against several Gotenberg containers was not measured.
- **arm64**: the only comparison on record is one document on an M1 Max under heavy load from other work, CPU only,
  interleaved, on 2026-10-22/23: 0.44-0.48 CPU-seconds a conversion against Gotenberg's 0.61-0.69 without
  restarts. These are ranges, not mean ± σ: the host's load was not controlled. It points the same way.
  moalhaddar/docx-to-pdf has no arm64 image, so no arm64 figure.

One document at a time against moalhaddar/docx-to-pdf (no Gotenberg on record), on the same VM type on 2026-10-23,
one vCPU, pool 1, one series each (percentiles of one series, not means of replicates): a typical report in 0.89 s
at the median and 1.40 s at p95, against 2.98 and 4.24 s; ready 0.62-0.66 s after `docker run`, polled finely,
against 11.7-12.9 s.

## Hostile documents

Measured on 2026-10-26 on a VM of the same kind (Intel Xeon Gold 6248R, 6 vCPU, 24 GiB, Ubuntu 24.04, kernel 6.8).
Each document went to each service alone, in a fresh container pinned to two vCPUs with 12 GiB of memory, each
service as it ships (oxidestack/docx-to-pdf at its defaults: pool 1, a 180 s timeout, a 2 GiB hard memory limit), one
run per cell. One run is enough here because the outcomes are set by limits, not by noise; the memory figures are one run's.
oxidestack/docx-to-pdf's rows below the three bombs, and moalhaddar/docx-to-pdf's from `tdf38575` down, are quoted
there from earlier notes and tests. "Alongside" is what a report sent while the document was in flight got.

| Document | oxidestack/docx-to-pdf | Gotenberg, as shipped | moalhaddar/docx-to-pdf |
| --- | --- | --- | --- |
| A deflate bomb (7.4 MiB, inflates to 1.6 GB) | **`413` in 0.01 s**, 237 MiB peak; the next report `200` in 0.62 s | `503` in 30.0 s; LibreOffice goes on to 7 600 MiB, and requests get `503` until the first `200`, 220 s after the answer | no answer in 300 s; 9 233 MiB |
| Eight 32 000² pictures (8.3 MiB) | **`413` in 0.02 s**, 235 MiB peak, refused by its picture budget before any picture is decoded (the comparison command's full run, 2026-10-27; it took 71.4 s and 1 827 MiB before the budget); the next report `200` in 0.75 s | `503` in 30.0 s; LibreOffice goes on to 8 085 MiB; the first `200` 33 s after the answer | no answer in 300 s; 2 564 MiB |
| A document that converts for minutes (`tables-10000`) | `504` in 180.1 s, the instance killed; the next report `200` in 0.66 s. At the default pool of one, a report waiting behind it gets `503` after the 60 s pool wait | `503` in 30.0 s; LibreOffice goes on to 1 931 MiB; the first `200` 218 s after the answer | no answer in 300 s; 3 501 MiB at 10 min, still rising |
| A picture over LibreOffice's bound (gray 32 768², RGB 18 919²) | downscaled, the header says so | `200` in 0.7-1.1 s with an empty frame where the picture was | dropped, answered `200` |
| A linked picture aimed at a listener, 68 kinds of document | no connection | **no connection** | 33 connections, public and private addresses; 10.2 s held per picture at an address that never answers, 100.8 s for ten |
| An RTF `INCLUDEPICTURE` of a local file | embedded | embedded | not reached |
| LibreOffice's `tdf38575` | `200` in about 0.3 s | `200` in 0.9 s | no answer in 180-300 s |
| An empty file | `415` | `200`, a 6 KB PDF | `200` |
| 1 MiB of random bytes | `415` | `200` in 20.3 s, a 3.2 MB PDF of the bytes as text | — |
| 10 MiB of random bytes | `415` | `503` at 30.0 s | 9 MB: no answer after 15 min |
| An upload with 2 s pauses | converts | converts, `200` in 7.1-7.8 s | `500` at Tomcat's timeout |
| An upload that stops halfway | `408` at 120 s | **`500` at 30.0 s** | `500` at Tomcat's timeout |

Where Gotenberg does as well or better here: it makes no network connection either, it converts `tdf38575`, and it
gives up on a stalled upload sooner. Where it differs: its 30 s answer does not stop LibreOffice, which goes on with
the document while every request behind it is refused. Every container was still running after every bomb; none
was killed for memory.

**Over thousands of conversions**, one request at a time through the same mix of 51 documents (Gotenberg on the
2026-10-26 VM; the other two on 2026-10-23; one run each): on one LibreOffice that is never restarted,
oxidestack/docx-to-pdf held 477 ± 21 MiB over the second half of 3000 conversions (the slope +0.8 ± 0.5 MiB per 1000,
a least-squares fit ± its standard error), and Gotenberg 462 ± 32 MiB (+1.4 ± 0.8 MiB per 1000): LibreOffice 26.8
does not leak on this mix, in either. moalhaddar/docx-to-pdf's LibreOffice 7.3 grew +152.4 ± 6.1 MiB per 1000.
Gotenberg as shipped restarts LibreOffice every 10 conversions, which keeps it at 254 ± 37 MiB, about half, at the
throughput cost in the speed table.

## Rendering

Gotenberg's rendering of the corpus has never been measured: no PDF from Gotenberg has been compared with
oxidestack/docx-to-pdf's, so this page claims nothing about it. Both run LibreOffice 26.8, so a difference
would be expected in fonts and export options, not in layout, and that expectation is unmeasured.
Against moalhaddar/docx-to-pdf (the corpus run of 2026-10-23), 34 of the corpus's 55 documents match in page count, page
sizes and words; the other 21 are differences between LibreOffice 7.3 and 26.8.

## Repeating it

**The speed table** is one matrix, `bench.py matrix tools/bench/matrices/compare-1cpu.tsv --reps 3`. Run
it on an amd64 host: moalhaddar/docx-to-pdf has only an amd64 image. `make bench-generate` writes several hundred
megabytes of documents first.

```sh
make bench-tools
make bench-generate
make image IMAGE=docx-to-pdf:bench
make bench-matrix MATRIX=tools/bench/matrices/compare-1cpu.tsv REPS=3
make bench-report
```

**Both tables in one command**: `make compare` starts each service fresh, runs the speed cells in blocks (each block
runs every service once, in a shuffled order, so a drift of the host moves every service alike) and the hostile
documents, and prints the tables with the figures on record beside the new ones. Only Docker is needed.

```sh
make compare QUICK=1   # minutes on a laptop: one short block and four hostile documents
make compare           # the full run, for a quiet VM: hours
```

The quick mode shows that the command works; its numbers are not comparable to the record. The full mode is
comparable to the record only on the record's kind of host, an amd64 Xeon Gold 6248R VM. It ran there once, on
2026-10-27, in 3 h 15 min on 8 vCPU, 3 blocks. Of the speed table's 36 figures, 33 agree with it at 2σ; the four
services' replicates ran interleaved within 21 minutes, and every gap between oxidestack/docx-to-pdf and another
service in CPU, throughput, latency and memory passes the 2σ test. Gotenberg without restarts used 1.14 ± 0.04
times its CPU a conversion, as shipped 1.23 ± 0.04. oxidestack/docx-to-pdf's own CPU a conversion came out 4.8 %
above the table's (0.979 ± 0.033 against 0.933 ± 0.026), within 2σ but narrowly, on a newer image than the one
measured. A later run on another VM
of the same kind ran the image measured and the current one interleaved, in 8 blocks, and found no difference
between them: the current one used 0.991× the CPU, with a 95 % interval of 0.955-1.026. The 5 % was the host's.
That VM ran the image measured 4 % faster than the table, and the current one 9.5 % faster than the full run.
The hostile documents gave the answers in the table, except the picture bomb, refused sooner since, and the random
bytes, which the command seeds rather than reproducing the bytes measured. Its soaks cycle the seven reports only, not
the 51 documents above: on them moalhaddar/docx-to-pdf grew +46.0 ± 8.5 MiB per 1000 conversions, and the other two
stayed flat.
What it runs and what it does not show is in [examples/compare](../../examples/compare/README.md).
