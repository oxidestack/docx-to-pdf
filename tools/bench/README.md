# The bench

What the service costs under load, and the sizing model built from it: generated documents, a k6 load, the
container's cgroup sampled from the side, runs in random order, and the graphs. It has run on arm64 (an M1 Max) and
on amd64 (a Xeon Gold 6248R cloud VM).

## Steps

1. `make bench-tools` builds the tool image (`docx-to-pdf-bench-tools:dev`): the generator, the sampler and the
   analysis.
2. `make bench-generate` writes the documents to `.bench/docs/` (git-ignored, about 650 MB) with
   `manifest.tsv`. The inputs are seeded, so they are the same bytes every time. Add `HOSTILE=1` for the
   1-gigapixel PNG and the document that hangs moalhaddar/docx-to-pdf. `bench.py generate --heavy --only …`
   adds the heavy documents (several GB, never in throughput runs): `bytes-*`/`mix-*` from 300 MB to
   1.1 GB, `text-*` to 250 000 pages, and the content bombs `tables-2000..10000`, `px-2048/4096mpx`,
   `px-8x1024mpx` and `pages-500000p` (one paragraph repeated, under 2 MB). The text, tables, pixels and pages ones
   are streamed by `heavy.py` (standard library, tested in `tools/tests/test_bench_heavy.py`), so writing them takes
   little memory; the `bytes`/`mix` ones go through python-docx and take about three times their size.

   The `objects` class is copied, not generated: 36 of the 37 documents in
   [`tests/corpus/documents/features/`](../../tests/corpus/documents/features/README.md), 834 kB in all, each
   with a rendering approved by eye. They are `arabic-lists`, `bidi-worksheet`, `case-study-pictures`,
   `chart-ole-textbox`, `chinese-spec`, `citations-comments`, `column-section-breaks`, `content-control-form`,
   `drop-cap-dashes`, `endnote-continuation`, `equation-captions`, `floating-table-math`, `floating-tables-columns`,
   `footnotes-per-page`, `japanese-vertical`, `legacy-form-fields`, `letterhead-sections`, `long-footnotes`,
   `long-toc-form`, `math-paper-damaged-table`, `multilevel-numbering`, `page-background-gradient`, `page-x-of-y`,
   `relative-width-lines`, `smartart-pictures`, `smartart-sections`, `strict-ooxml`, `table-formulas-unicode`,
   `table-vertical-text`, `theme-fonts`, `toc-field-f`, `toc-section-margins`, `toc-styleref`,
   `tracked-changes-sections`, `tracked-table-cells` and `tracked-table-columns`: charts, SmartArt, Office Math,
   floating and damaged tables, sections, fields, notes, tracked changes, form fields, right-to-left and vertical
   text; the manifest's `feature` column says what each exercises. The 37th, `full-width-line`, hangs
   moalhaddar/docx-to-pdf, so it is in the `hostile` class instead. The bench needs nothing outside the
   repository.
3. `make image IMAGE=docx-to-pdf:bench` builds the subject.
4. Run the measurements:
   - `make bench-matrix MATRIX=tools/bench/matrices/screen.tsv REPS=3` runs every cell × replicate in random
     order and resumes where it stopped.
   - `tools/bench/bench.py cell NAME --docs … --pool …` runs one cell; `--help` lists the options.
5. `make bench-report` writes `summary.json` and `timeseries.png` per run, then `.bench/report/`: `runs.csv`,
   `analysis.json`, `cells.md` with `cells.json`, and the graphs. A cell's replicates (`r1..rN`) make one figure:
   `cells.md`, also printed, gives each as the mean ± σ (n), σ the sample standard deviation, under the hosts and
   commits the runs recorded; `latency.png` and `scaling.png` draw the same mean ± σ. `bench.py report RUNS
   --out .bench/report-b --against .bench/report` judges every figure against the same cell of another report:
   two figures agree when |Δ| ≤ 2·√(σ₁²/n₁ + σ₂²/n₂), about 95 % (`against.md`). With two or three replicates σ is
   itself uncertain, so a difference calls for an interleaved, paired measurement rather than a conclusion.

## Subjects

`--subject ours` (the default) runs `docx-to-pdf:bench` under the chart's security context. `--subject oracle` runs
upstream's image from `oracle.env` as an installation runs it: `POOL_SIZE`, the multipart limits raised past the
documents (upstream's 10 MB answers 500 above it) and Tomcat's connection timeout at 120 s (upstream's 30 ms fails
uploads); ready on `/actuator/health/readiness`; no `/metrics` and no tracing. `matrix --subject`
overrides a matrix's, and `--runs .bench/runs-oracle` keeps the two apart. The analysis finds moalhaddar/docx-to-pdf's
instances as its `soffice.bin` processes.

On a Linux host (not a macOS Docker VM) Docker's cgroup driver is systemd; the bench finds the container's cgroup either
way.

## What a run does

Every run uses fresh containers, because `memory.peak` cannot be reset on the VM's kernel 6.8.

1. A preflight records the other containers and the VM's pressure. Above 5 % CPU pressure it warns.
2. It starts the service under the chart's security context on cpus `0..C-1`, with memory equal to swap and
   `/tmp` on a volume or a tmpfs. Optionally it starts a Collector that drops everything (`otelcol.yaml`).
3. It starts the sampler as a sidecar in the service's PID namespace.
4. Warm-up: each instance converts each document twice. These requests are discarded.
5. It runs the steady window with k6 in the chosen workload model (`load.js`).
6. It stops the sampler and keeps the service's log and state.
7. It removes every container, the volume and the network, also on Ctrl-C.
8. It cools down for 15 s (`--cooldown`).

## Raw data per run

All in `.bench/runs/<matrix>/<cell>/r<n>/`:

| File | What |
| --- | --- |
| `run.json` | the settings, image ids, preflight, timings, the service's end state; its `provenance` below |
| `steady/k6.csv.gz` | every request: start (ms), duration, status, document |
| `steady/k6-summary.json` | k6's own summary, `dropped_iterations` included |
| `samples.jsonl` | one line a second, described below |
| `service.log` | the last 5 000 lines |

Each `samples.jsonl` line records:

- the service cgroup: `cpu.stat`, `memory.current`, `.peak`, `.stat`, `.events`, `io.stat` and the pressure
  files;
- the VM's pressure;
- per process: RSS (anonymous, file and shmem), `VmHWM` and CPU ticks;
- every 5 s, the service's `/metrics`.

`run.json`'s `provenance` (`provenance.py`) says where the figures were measured and how to measure them again:

- `host`: the Docker host the service ran on, as a container sees it: the CPU model, logical CPUs, sockets, cores
  and threads a core, the clocks and caches the kernel exposes, memory, OS and kernel, Docker, the cgroup version,
  and the hypervisor's technology when it is a VM (never a provider's name);
- `machine`: the machine `bench.py` ran on. On a Mac it names the chip, which the Docker VM hides: its performance
  levels, their cores and caches, the memory, macOS, and the Docker VM's vCPUs, memory, architecture and OS as
  `docker info` reports them. macOS does not expose an Apple chip's clocks. On Linux it is the Docker host;
- `pinning`: the cpusets inside the VM (the service on `0..C-1`, k6 and the sampler on the last vCPU, the Collector
  on the one before); how the VM's vCPUs map to the host's cores is not visible from the guest;
- `command` and, for a matrix cell, `cell_command`, the matrix, its seed, and `replicate` of `replicates` (n);
- `commit`: the checkout's commit and whether tracked files differed from it;
- `images`: the service's, k6's, the Collector's and the tool image by id, digests and their revision label;
- `documents`: each document's SHA-256, size and generator seed (the first 8 bytes of SHA-256 of its name; none
  for the copied feature documents).

## Matrices

| File | Question | Runs |
| --- | --- | --- |
| `screen.tsv` | which of document, pool, tracing and `/tmp` matter: a 2^4 factorial | 16 × 3 × ~7 min ≈ 6 h |
| `usl.tsv` | saturation throughput against pool size, the USL fit and the best pool (text-10p, mix-50mb) | 16 × 3 × ~7 min ≈ 6 h |
| `size.tsv` | memory and time against document size and kind, one at a time | 22 × ~6 min ≈ 2.5 h |
| `io.tsv` | disk (capped at 120 MB/s or not) against tmpfs for large documents | 6 × 3 × ~7 min ≈ 2 h |
| `ramp.tsv` | latency against offered load, open model; set the rates from `usl.tsv` first | 2 × ~20 min |
| `soak.tsv` | an hour at 70 % load, recycling off and on; set the rate first | 2 × 1 h |
| `grid.tsv` | the suprema per combination of vCPU × memory × disk × pool, for the two installation workloads | 132 × ~6 min ≈ 13 h a replicate |
| `memory.tsv` | the largest documents under falling memory limits, pool 1 and 2: where OOM starts | 60 × ~4 min ≈ 4 h |
| `profile-4cpu-4g.tsv` | the worked example: 4 vCPU, 4 GiB, 120 MB/s disk, pool 2-4 | 6 × 3 × ~12 min ≈ 3.5 h |

A matrix TSV has one row per cell, and its column names are `cell` options (`docs`, `pool`, `cpus`, `memory`,
`tmp`, `tmpfs_size`, `tracing`, `mode`, `rate`, `vus`, `stages`, `duration`, `io_bps`, `cooldown`). `env` holds
`KEY=VALUE;KEY=VALUE` for the service.

## The quick plan

`matrices/quick/` is a plan for a few hours rather than days: the typical document only (text, tables and
30-60 pictures, 10-20 MB), one resource at a time.

1. `1-pool.tsv` finds the best pool per vCPU count.
2. `2-memory.tsv` finds the memory that best pool needs.
3. `3-disk.tsv` checks whether the disk or tracing matters.
4. `4-ramp.tsv` measures latency against offered load.

Steps 2-4 are filled in from step 1's `runs.csv`. Step 0 (`0-calibrate.tsv`) sets how many conversions a cell runs.
It ran on arm64 first; the matrices hold the values it used.

## Sizing

`coefficients.py` turns the quick plan's report into the coefficients of the sizing model:
`tools/bench/coefficients/<platform>-<workload>.json` (throughput per vCPU and pool, CPU-seconds per conversion, the
service time's shape, the memory fit and headroom, what a conversion writes and the disk's share). `sizing.py` is the
model (its docstring states it), and `bench.py size` the calculator over it, standard library only:

```sh
make bench-report    # or: bench.py report .bench/runs/1-pool .bench/runs/2-memory .bench/runs/3-disk .bench/runs/4-ramp
docker run --rm -v $PWD:/repo -v $PWD/.bench:/data -w /repo/tools/bench docx-to-pdf-bench-tools:dev \
  python coefficients.py /data/report /data/runs --out /repo/tools/bench/coefficients/arm64-report.json \
  --platform "…" --workload "…"
tools/bench/bench.py size hardware --cpus 4 --memory 4 --disk-mbps 120      # hardware → settings
tools/bench/bench.py size target --latency 5 --quantile 0.95 --rate 60/min  # response time and rate → pods
```

The platform's three numbers (`s1`, contention, the disk's MB/s) come from `bench.py probe` on a Docker host, or
from a running service's probe endpoint (`DOCX_TO_PDF_PROBE_PORT`, `docs/guide/operations.md`, "Measuring a
platform: the sizing probe"): `bench.py probe --url http://localhost:9090 --out platform.json` sends the reference mix
from `.bench/docs/`, and `bench.py size … --platform platform.json` takes the answer.
The endpoint converts with the service's HTTP path quiet, so it adds h = 0.045 × its measured `s1`, the HTTP path's
CPU per conversion of the report mix (receiving, spooling and sniffing the upload, streaming the PDF back), to both
`s1` and `S_sat`, and works `contention` out from the corrected pair. Its `platform` block keeps the conversion's
own beside them: `s1_conversion`, `s_sat_conversion`, `http_cpu_s` (h) and `http_cpu_share`; `by_document_s` is the
conversion's own. `bench.py probe` without `--url` measures through HTTP,
so h is in its numbers, and its `s1`, a client's latency, also includes the loopback transfer of the upload and the
PDF (~3 %): it reads about 3 % above the endpoint's.
`bench.py probe` runs two count cells on a pool of C instances on C vCPU: `alone` (one client, 20 conversions) gives
`s1`, the mean latency; `busy` (C + 2 clients, 120 + 3 × C conversions) gives `S_sat = C / X`. The two extra clients
keep a queue, so an instance starts its next conversion as it ends one and never waits for its client's upload or
download: `S_sat` is the conversion's own with every vCPU converting, as the endpoint measures it (with
C clients the instances idled and `S_sat` counted the transfers). The platform file's `busy` block shows it:
`cpu_steady`, the share of the C vCPU in use where X is counted (CPU-seconds a conversion × X / C), and the server's
mean pool wait (above 0: requests queued). A conversion itself uses 0.95-0.97 of a vCPU, so `cpu_steady`
tops out a little under 1; the probe warns under 0.9.

The calculator takes `amd64-report.json` (a Xeon Gold 6248R VM, measured again once pictures were no longer
reduced) by default; `--coefficients FILE` takes another platform's, such as `arm64-report.json` (an M1 Max). It
sizes memory from the worst case: the fit's service term plus, per pool slot, the 2 GiB hard
per-conversion limit and 256 MiB of growth past it within one 100 ms read, 256 + pool × 2304 MiB with no headroom, so no
document mix can OOM the pod; `/tmp` from the worst case too, per conversion max(3 × `DOCX_TO_PDF_MAX_DOCUMENT_SIZE`,
256 MiB), as measured on the heavy documents and the bombs; `size hardware` prints that `/tmp` size, on disk or as a
tmpfs counted in the memory limit (`--tmpfs`). The typical fit stays in `sizing.py` as `basis="typical"`, and
`size hardware|target --typical` selects it: its two terms rounded up to powers of two (256 + 256 × pool MiB on both)
before the headroom, and the typical mix's `/tmp`. It is a gamble on the documents, not a bound.
`sizing_figures.py REPORT COEFFICIENTS OUT` draws the sizing graphs; `compare_figures.py OUT
LABEL=REPORT=RUNS=COEFFICIENTS ...` draws several reports on one set of graphs. Both use `theme.py`: a dark theme,
three hues validated for colour-blind readers, a marker per series.

### Running on a remote amd64 VM

The amd64 runs were on a VM in a VMware cloud. `matrices/quick-amd64/` holds that plan: step 1 for both
subjects, steps 2-4 per subject (`ours/`, `oracle/`), moalhaddar/docx-to-pdf's runs under `--runs .bench/runs-oracle`.
On an LVM root, cap `/dev/sda` (`io_device`), the disk under the volume Docker's data lives on.

## The VM

Everything runs in the current Docker context's VM. The VM needs at least C + 2 cpus: the service takes C, k6 and
the sampler share the last cpu, and the Collector takes the one before it.

For measurements, use a VM that nothing else uses, on a quiet host: the VM's vCPUs are host threads, so a build on
the host slows conversions without the VM seeing it (2-15× slower under a concurrent `cargo clippy` on the host). Each
run records the host's load average and warns above one per host cpu. For example, run
a dedicated VM with 8 vCPU, 40 GiB of memory and a 100 GiB disk, with any other VM stopped for the
duration. Then build the images in it again (`make bench-tools`, `make image IMAGE=docx-to-pdf:bench`). The
documents and the runs stay where they are: `.bench/` is on the host.

`--io-bps` caps the block device that Docker's volumes live on, `/dev/vdb` by default (`--io-device`). Check
`lsblk` inside a new VM.

Two things do not carry over to amd64:

- the latency and throughput numbers of an M1 core;
- node disks.

Memory per document, the shape of the scaling curve, and the tracing and tmpfs effects do carry over.

## The torture test

`torture.py` (standard library) sends hostile documents to oxidestack/docx-to-pdf,
Gotenberg 8.38.1 as shipped and moalhaddar/docx-to-pdf, each alone in a fresh container on a Docker host (amd64:
moalhaddar/docx-to-pdf has no other image; not the M1 Max). `hostile` sends one document with a typical report
alongside and records the answer, the container's memory, and whether the service is still up and when it next
converts; `one` converts documents one container each and lists the PDF's pictures; `pause` sends an upload in pieces
and one that stops; `egress` runs `image/egress`'s documents against a listener for any service; `soak` runs
thousands of conversions and records every process's RSS. The bombs come from `heavy.py`, and the container's cgroup
is found by `bench.py`'s `cgroup_dir` (systemd's driver or cgroupfs), so `bench.py` goes where `torture.py` goes.

Run it through `make compare` ([examples/compare/README.md](../../examples/compare/README.md)): its full mode
(`tools/compare/compare.py`) runs the whole sequence on the three services and prints the table beside the figures on
record, and `make compare QUICK=1` runs a subset in minutes on a laptop.
