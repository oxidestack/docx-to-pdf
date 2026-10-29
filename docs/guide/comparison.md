# Compared with Gotenberg and moalhaddar/docx-to-pdf

Two other services turn office documents into PDF over HTTP with LibreOffice. **Gotenberg** is the most used: a
general document API around Chromium, LibreOffice and PDF tools. **moalhaddar/docx-to-pdf** is the service whose
interface this one keeps. This page says what each does, then compares speed, memory and what each does with a
hostile document.

Every figure on this page is read from one file, the raw results of the latest full run of `make compare`, the
comparison anyone can rerun ([Repeating it](#repeating-it)); the [Benchmarks](https://oxidestack.github.io/docx-to-pdf/benchmarks/)
page shows all of it and the file itself. The site's build writes the figures below from that file and fails on a
figure typed here by hand. A figure is the mean ± σ (n) over the run's blocks, σ the sample standard deviation, and
two figures differ when their gap is more than 2σ = 2·√(σ₁²/n₁ + σ₂²/n₂); where a figure is one run, the page says so.

## Which to choose

- **Gotenberg**, when the job is more than Word documents to PDF: HTML, URLs or Markdown through Chromium,
  spreadsheets and presentations, merging and splitting PDFs, PDF/A or PDF/UA, webhooks, or options on the request
  (page ranges, a watermark, a password). It does all of that, and has years in production behind it.
- **oxidestack/docx-to-pdf**, when the job is Word documents to PDF behind one HTTP call, at volume, from
  documents you do not control. On the workload measured, in one interleaved run, it used less CPU and less memory
  than Gotenberg and converted more documents a second ([the ratios](#speed-and-memory)), and a hostile document is
  stopped at a limit instead of left running ([the cases](#hostile-documents)).
- **moalhaddar/docx-to-pdf**, only to keep exactly what it does today, LibreOffice 7.3's layout included. To leave
  it, the [migration guide](migration.md) lists what a client and a deployment change.

## What each does

Gotenberg's column is checked against its documentation and its source at the version measured, which
[Speed and memory](#speed-and-memory) names.

| | Gotenberg | moalhaddar/docx-to-pdf | oxidestack/docx-to-pdf |
| --- | --- | --- | --- |
| What it converts | every format LibreOffice opens (text documents, spreadsheets, presentations, drawings), and HTML, URLs and Markdown through Chromium | Word documents (DOCX, DOC, ODT, RTF and their variants); anything else is handed to LibreOffice unchecked | Word documents only: DOCX, DOCM, DOTX, DOC, ODT, OTT, flat ODT, RTF ([the formats accepted](api.md#the-formats-accepted)) |
| Conversion routes | `/forms/libreoffice/convert`, `/forms/chromium/convert/url`, `/forms/chromium/convert/html`, `/forms/chromium/convert/markdown`, and screenshots of each Chromium input | `POST /pdf`, `POST /docx-to-pdf` | `POST /pdf`, `POST /docx-to-pdf` |
| How the format is decided | the file name's extension | not checked | from the document's bytes; anything else is `415` before LibreOffice sees it |
| Options on a request | many on the LibreOffice route: page ranges, landscape, a password, PDF/A and PDF/UA, merging several files into one, a watermark, tagged PDF | none | none |
| PDF tools | merge, split, PDF/A and PDF/UA, metadata, bookmarks, attachments, Factur-X, flatten, optimize, watermark, stamp, rotate, encrypt (`/forms/pdfengines/…`) | none | none |
| Asynchronous work | webhooks; input fetched from a URL (`downloadFrom`) | none | none |
| Authentication, TLS | basic authentication or OIDC; TLS | not documented | none: put it behind your ingress |
| A conversion that runs too long | the API answers `503` at its timeout (`API_TIMEOUT`, `30s` by default); LibreOffice goes on with the document | no timeout | killed at `DOCX_TO_PDF_CONVERSION_TIMEOUT` (`180s` by default) and answered `504`; the instance is replaced |
| A document that needs too much memory | no limit of its own | no limit of its own | the instance is killed past `DOCX_TO_PDF_INSTANCE_HARD_MEMORY` (`2GiB` by default) and the request answered `413`; a ZIP that inflates past `DOCX_TO_PDF_MAX_INFLATED_SIZE` (`256MiB`) is `413` before conversion |
| A picture too large for LibreOffice | dropped, answered `200` | dropped, answered `200` | downscaled, and the `Docx-To-Pdf-Pictures` header says so |
| A linked picture in a document | not fetched | fetched, from public and private addresses | not fetched |
| LibreOffice | 26.8.1, restarted after `LIBREOFFICE_RESTART_AFTER` conversions (`10` by default) | 7.3 | 26.8, an instance replaced after `DOCX_TO_PDF_INSTANCE_MAX_CONVERSIONS` conversions (`200` by default) or past its memory ceiling |
| Telemetry | OpenTelemetry traces, metrics and logs; Prometheus | none | OpenTelemetry traces, metrics and logs; Prometheus `GET /metrics` |
| Images | amd64, arm64 and three more architectures | amd64 only | amd64 and arm64 |

The limits in oxidestack/docx-to-pdf's column are its defaults; every one is a setting
([Configuration](configuration.md)). The linked-picture row and the limit rows are measured in the torture test
below; Gotenberg's and moalhaddar/docx-to-pdf's are as each ships.

<!-- benchmarks:begin: `make evidence` writes this block from site/public/benchmarks/results.json -->

## Speed and memory

Measured by `make compare` on 2026-10-29 17:16 UTC, on an Intel(R) Xeon(R) Gold 6248R CPU @ 3.00GHz (x86_64) virtual
machine under VMware: 1 pinned vCPU, a pool of 1 and 16 GiB of memory a service; a closed load of the typical reports
`report-img30-10mb`, `report-img45-12mb`, `report-img60-15mb`, `report-img60-20mb`; 52 conversions a service in each of
3 blocks, every block running each service once in a shuffled order, so a drift of the host moves them alike. Gotenberg
8.38.1 ran twice, as it ships and with LibreOffice's restarts turned off. oxidestack/docx-to-pdf was release 1.0.0, its
published image pulled by digest.

Each figure is the other service's over oxidestack/docx-to-pdf's, the ratio of the means ± its standard error, with the
verdict of the 2σ test. A ratio measured inside one interleaved run holds from one host to the next better than an
absolute figure; each row links the absolute figures on
[Benchmarks](https://oxidestack.github.io/docx-to-pdf/benchmarks/).

| | Gotenberg, as shipped | Gotenberg, no restarts | moalhaddar/docx-to-pdf |
| --- | --- | --- | --- |
| [conversions/s](https://oxidestack.github.io/docx-to-pdf/benchmarks/#ratio-throughput) | 0.82 ± 0.02× (oxidestack/docx-to-pdf ahead) | 0.87 ± 0.03× (oxidestack/docx-to-pdf ahead) | 0.31 ± 0.01× (oxidestack/docx-to-pdf ahead) |
| [CPU-s a conversion](https://oxidestack.github.io/docx-to-pdf/benchmarks/#ratio-cpu) | 1.21 ± 0.02× (oxidestack/docx-to-pdf ahead) | 1.13 ± 0.04× (oxidestack/docx-to-pdf ahead) | 3.32 ± 0.06× (oxidestack/docx-to-pdf ahead) |
| [p99 latency, s](https://oxidestack.github.io/docx-to-pdf/benchmarks/#ratio-p99) | 1.34 ± 0.03× (oxidestack/docx-to-pdf ahead) | 1.15 ± 0.04× (oxidestack/docx-to-pdf ahead) | 3.34 ± 0.06× (oxidestack/docx-to-pdf ahead) |
| [container `memory.peak`, MiB](https://oxidestack.github.io/docx-to-pdf/benchmarks/#ratio-peak) | 1.65 ± 0.03× (oxidestack/docx-to-pdf ahead) | 1.86 ± 0.04× (oxidestack/docx-to-pdf ahead) | 2.27 ± 0.07× (oxidestack/docx-to-pdf ahead) |

Where Gotenberg is ahead of oxidestack/docx-to-pdf in this run:

- Gotenberg, no restarts, written a conversion, MB: 27.9 ± 0.2 (3) against 28.3 ± 0.2 (3)
  ([the row](https://oxidestack.github.io/docx-to-pdf/benchmarks/#speed-written))
- Gotenberg, `png-g-32767` (the same answer, 200, and the same pictures): answered in 9.78 s against 11.34 s, one run a
  side ([the row](https://oxidestack.github.io/docx-to-pdf/benchmarks/#torture-png-g-32767))
- Gotenberg, as shipped, LibreOffice's resident memory over the soak's second half: 289 ± 61 MiB over 1350 conversions
  against 391 ± 1 MiB over 1500 conversions, one soak a side
  ([the row](https://oxidestack.github.io/docx-to-pdf/benchmarks/#soaks))
- Gotenberg, no restarts, LibreOffice's resident memory over the soak's second half: 347 ± 65 MiB over 1500 conversions
  against 391 ± 1 MiB over 1500 conversions, one soak a side
  ([the row](https://oxidestack.github.io/docx-to-pdf/benchmarks/#soaks))

Where neither is ahead at 2σ:

- Gotenberg, as shipped, written a conversion, MB: 28.3 ± 0.001 (3) against 28.3 ± 0.2 (3)
  ([the row](https://oxidestack.github.io/docx-to-pdf/benchmarks/#speed-written))

## Hostile documents

Each case went to each service once, each service as it ships, in a fresh container with 12 GiB of memory on vCPUs 2-3,
waiting up to 300 s for the answer: the answer, its time, the container's memory peak and, for a picture, how many
pictures reached the PDF. The answers are set by limits, and so repeat; the times and peaks are one run a side, with no
σ.

| Case | oxidestack/docx-to-pdf | Gotenberg | moalhaddar/docx-to-pdf |
| --- | --- | --- | --- |
| [`pages-500000p`](https://oxidestack.github.io/docx-to-pdf/benchmarks/#torture-pages-500000p), a deflate bomb: a document that inflates past any limit | 413 in 0.01 s; peak 236 MiB; alongside none; first 200 after the answer: 0.77 s | 503 in 30.0 s; peak 7 609 MiB; alongside 1 × 503; first 200 after the answer: 218.53 s | no answer in 300.04 s; peak 9 373 MiB; alongside 1 × TimeoutError: timed out; first 200 after the answer: none |
| [`px-8x1024mpx`](https://oxidestack.github.io/docx-to-pdf/benchmarks/#torture-px-8x1024mpx), a picture bomb: eight gigapixel pictures | 413 in 0.02 s; peak 237 MiB; alongside none; first 200 after the answer: 0.78 s | 503 in 30.0 s; peak 8 076 MiB; alongside 1 × 503; first 200 after the answer: 35.43 s | no answer in 300.11 s; peak 2 505 MiB; alongside 1 × TimeoutError: timed out; first 200 after the answer: 227.54 s |
| [`tables-10000`](https://oxidestack.github.io/docx-to-pdf/benchmarks/#torture-tables-10000), a document that converts for minutes | 504 in 180.09 s; peak 1 401 MiB; alongside 2 × 503, 1 × 200; first 200 after the answer: 0.83 s | 503 in 30.0 s; peak 1 850 MiB; alongside 1 × 503; first 200 after the answer: 218.42 s | no answer in 300.1 s; peak 4 076 MiB; alongside 1 × TimeoutError: timed out; first 200 after the answer: 451.4 s |
| [`png-g-32767`](https://oxidestack.github.io/docx-to-pdf/benchmarks/#torture-png-g-32767), a gray picture just inside LibreOffice's bound | 200 in 11.34 s; peak 348 MiB; pictures in the PDF 1; docx-to-pdf-pictures: downscaled=1, replaced=0 | 200 in 9.78 s; peak 1 138 MiB; pictures in the PDF 1 | 200 in 93.6 s; peak 1 524 MiB; pictures in the PDF 1 |
| [`png-g-32768`](https://oxidestack.github.io/docx-to-pdf/benchmarks/#torture-png-g-32768), a gray picture just past LibreOffice's bound | 200 in 11.2 s; peak 347 MiB; pictures in the PDF 1; docx-to-pdf-pictures: downscaled=1, replaced=0 | 200 in 0.83 s; peak 108 MiB; pictures in the PDF 0 | 200 in 0.47 s; peak 322 MiB; pictures in the PDF 0 |
| [`png-rgb-18918`](https://oxidestack.github.io/docx-to-pdf/benchmarks/#torture-png-rgb-18918), a colour picture just inside LibreOffice's bound | 200 in 4.59 s; peak 152 MiB; pictures in the PDF 1; docx-to-pdf-pictures: downscaled=1, replaced=0 | 200 in 11.44 s; peak 1 139 MiB; pictures in the PDF 1 | 200 in 18.75 s; peak 1 442 MiB; pictures in the PDF 1 |
| [`png-rgb-18919`](https://oxidestack.github.io/docx-to-pdf/benchmarks/#torture-png-rgb-18919), a colour picture just past LibreOffice's bound | 200 in 4.39 s; peak 152 MiB; pictures in the PDF 1; docx-to-pdf-pictures: downscaled=1, replaced=0 | 200 in 0.75 s; peak 108 MiB; pictures in the PDF 0 | 200 in 0.45 s; peak 409 MiB; pictures in the PDF 0 |
| [`empty`](https://oxidestack.github.io/docx-to-pdf/benchmarks/#torture-empty), an empty file | 415 in 0.0 s; peak 34 MiB | 200 in 0.76 s; peak 102 MiB; pictures in the PDF 0 | 200 in 0.31 s; peak 339 MiB; pictures in the PDF 0 |
| [`random-1mb`](https://oxidestack.github.io/docx-to-pdf/benchmarks/#torture-random-1mb), random bytes | 415 in 0.01 s; peak 37 MiB | 500 in 0.63 s; peak 101 MiB | no answer in 300.04 s; peak 447 MiB |
| [`random-10mb`](https://oxidestack.github.io/docx-to-pdf/benchmarks/#torture-random-10mb), more random bytes | 415 in 0.02 s; peak 46 MiB | 500 in 0.7 s; peak 138 MiB | no answer in 300.15 s; peak 455 MiB |
| [`tdf38575`](https://oxidestack.github.io/docx-to-pdf/benchmarks/#torture-tdf38575), a document that hangs moalhaddar/docx-to-pdf's LibreOffice | 200 in 0.36 s; peak 91 MiB; pictures in the PDF 1 | 200 in 0.84 s; peak 107 MiB; pictures in the PDF 1 | no answer in 300.1 s; peak 350 MiB |
| [`pause`](https://oxidestack.github.io/docx-to-pdf/benchmarks/#torture-pause), uploads that pause, then one that stalls halfway | pauses: HTTP/1.1 200 OK in 7.04 s, HTTP/1.1 200 OK in 6.77 s, HTTP/1.1 200 OK in 6.76 s; stalled: HTTP/1.1 408 Request Timeout in 120.01 s | pauses: HTTP/1.1 200 OK in 7.5 s, HTTP/1.1 200 OK in 6.93 s, HTTP/1.1 200 OK in 6.89 s; stalled: HTTP/1.1 500 Internal Server Error in 30.0 s | pauses: HTTP/1.1 200  in 8.68 s, HTTP/1.1 200  in 8.44 s, HTTP/1.1 200  in 8.29 s; stalled: TimeoutError: timed out in 200.1 s |
| [`egress`](https://oxidestack.github.io/docx-to-pdf/benchmarks/#torture-egress), linked pictures aimed at a listener, public and private addresses | 68 documents: 68 × 200; connections the listener logged: 0 | 68 documents: 68 × 200; connections the listener logged: 0 | 68 documents: 51 × 200, 17 × TimeoutError: timed out; connections the listener logged: 36 (27 public, 9 private); 150 s a document |

The soaks, thousands of conversions on one LibreOffice, are on
[Benchmarks](https://oxidestack.github.io/docx-to-pdf/benchmarks/#soaks).

<!-- benchmarks:end -->

## What the figures cannot say

- **One vCPU, one LibreOffice, one document mix.** Gotenberg runs one LibreOffice per container, so this is the cell
  that compares directly; a larger pool against several Gotenberg containers was not measured.
- **One host.** An absolute time or CPU figure moves between virtual machines of one definition; a ratio measured
  inside one interleaved run is the figure that carries to another host. The run was on amd64: moalhaddar/docx-to-pdf
  has no arm64 image, and no arm64 run is in the results.
- **One image.** The run measured the one image of oxidestack/docx-to-pdf that the section above names; a later
  release is measured by a later run.
- **One run a side for each hostile document.** The answers repeat, because limits set them; a time or a peak is a
  single measurement.
- **Memory over thousands of conversions.** The soaks on [Benchmarks](https://oxidestack.github.io/docx-to-pdf/benchmarks/#soaks)
  are one soak a side, which does not settle whether a service's memory grows; this page claims nothing about it.

Where Gotenberg does as well or better with a hostile document: it makes no network connection either, it converts
`tdf38575`, and it gives up on a stalled upload sooner. Where it differs: its answer at its timeout does not stop
LibreOffice, which goes on with the document while the requests behind it are refused.

## Rendering

Gotenberg's rendering has never been measured: no PDF from Gotenberg has been compared with oxidestack/docx-to-pdf's,
so this page claims nothing about it. Both run LibreOffice 26.8, so a difference would be expected in fonts and export
options, not in layout, and that expectation is unmeasured. Against moalhaddar/docx-to-pdf, the documents that differ
differ where LibreOffice 7.3 and 26.8 lay a page out differently; the [migration guide](migration.md) says what a
deployment that moves should check.

## Repeating it

`make compare` starts each service fresh, runs the speed cells in blocks (each block runs every service once, in a
shuffled order, so a drift of the host moves every service alike), then the hostile documents and the soaks, and prints
both tables. Only Docker is needed.

```sh
make compare QUICK=1   # minutes on a laptop: one short block and four hostile documents
make compare           # the full run, for a quiet VM: hours
```

The quick mode shows that the command works; its numbers are not comparable with the ones above. The full mode writes
a `results.json` like the one this page is read from, comparable on the same kind of host, and moalhaddar/docx-to-pdf
runs only on amd64. What it runs and what it does not show is in [examples/compare](../../examples/compare/README.md).
