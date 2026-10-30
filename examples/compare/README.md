# Compare oxidestack/docx-to-pdf, Gotenberg and moalhaddar/docx-to-pdf yourself

One command builds what it needs, starts each service, measures it and prints two tables. You need Docker with
Compose v2 and a checkout with its submodule (`git clone --recurse-submodules`, or `git submodule update --init` in
an existing clone): oxidestack/docx-to-pdf's image takes fonts from `upstream/docx-to-pdf`, and its build fails
without them.

```sh
make compare QUICK=1          # a few minutes on a laptop
make compare                  # the full mode, for a quiet VM: hours
tools/compare/compare.sh --quick   # the same without make
```

The tables are printed at the end and kept in `.bench/compare/<stamp>/tables.md`. Every raw measurement is in
`results.json` beside them, and the drivers' own files (k6's requests, the cgroup samples, the services' logs) are
in the subdirectories. `results.json` also records where the figures were measured, in the same form as the bench's
`run.json` (`tools/bench/provenance.py`): the Docker host's CPU, topology, clocks, caches, memory, OS, kernel and the
hypervisor's technology when it is a VM, and, when the machine running `compare.sh` has a `python3`, that machine
(on a Mac, the chip the Docker VM hides). The tables print both on their `Host:` line.

Every run builds oxidestack/docx-to-pdf's image from the checkout as `docx-to-pdf:compare`; the build cache makes
that a no-op when nothing changed. The image carries the commit it was built from as its
`org.opencontainers.image.revision` label, by the rule `make image` uses: the full commit, `-dirty` when tracked files
differ from it. `results.json` records that label as `image_revision`, and the tables' `Command:` line prints it beside
the checkout's commit. An image given with `IMAGE=` is not built: it is recorded with the label it was built with.

## What it measures

**Speed and memory.** This is the cell of `tools/bench/matrices/compare-1cpu.tsv`, the run whose results [the comparison
page](../../docs/guide/comparison.md#speed-and-memory) and the Benchmarks page are generated from. Each service gets one
pinned vCPU and one LibreOffice (pool 1). Three clients in a closed loop send the typical report mix: four reports of
10-20 MB with 30-60 pictures each. The table gives conversions per second, CPU-seconds per conversion, p50/p95/p99
latency, the container's `memory.peak` and anonymous peak, the bytes written per conversion, and the time from
`docker run` to ready. Gotenberg runs twice: as shipped, and with `LIBREOFFICE_RESTART_AFTER=0`. The cells run in
blocks, and each block runs every cell once in a shuffled order. A host that slows down in the middle of a run therefore
slows every service alike.

**The torture test.** These are the hostile documents of [the comparison
page](../../docs/guide/comparison.md#hostile-documents). Each one goes to each service alone, in a fresh container, with
the service as it ships: a deflate bomb, eight gigapixel pictures, a document that never finishes, pictures on both
sides of LibreOffice's size bound, an empty file, random bytes, LibreOffice's `tdf38575`, an upload that pauses and one
that stalls, linked pictures aimed at a listener, and a soak of thousands of conversions. A row gives the answer, its
time and the container's memory peak. For a bomb, it also gives what happened to a report sent alongside and when the
service next converted.

Every row of both tables shows the figure on record next to the new one. Those records come from an amd64 Xeon Gold
6248R VM. A full run on such a host can be checked against them. On any other host, the two are only shown side by
side.

## Quick and full

| | `QUICK=1` | full |
| --- | --- | --- |
| speed | 1 block, 12 conversions a cell | 3 blocks, 52 conversions a cell, as on record |
| torture | the deflate bomb, the gigapixel picture at the bound, an empty file, 1 MiB of random bytes; 4 GiB a container | every hostile document on record, at 12 GiB, plus a soak (`--soak N`, default 3000 a service) |
| time | about 5 minutes (an M1 Max, without moalhaddar/docx-to-pdf) | hours: the bombs alone take minutes each on moalhaddar/docx-to-pdf |

The options of `tools/compare/compare.py` are passed through `compare.sh`: `--only speed|torture`, `--blocks N`,
`--conversions N`, `--soak N`, `--soak-moalhaddar N`, `--soak-alone`, `--linked-cap-moalhaddar S`,
`--subjects ours,gotenberg`.

Each torture case waits at most 300 s for an answer, as on record, with one exception. moalhaddar/docx-to-pdf's
linked-picture run waits 150 s a document (`--linked-cap-moalhaddar`). After the ODT document with an OLE link, it
answers nothing more, and at 300 s the 17 documents after it took 85 minutes of the full mode. Its slowest answer
before that takes about 100 s (ten pictures linked to an address that never answers, 10 s each), so the same documents
answer at 150 s as at 300 s. The number of connections the listener logs grows with the run's length, because its
LibreOffice reconnects to the private address every 300 s; the addresses it reaches, public and private, do not.

The soaks measure memory, not time. On a host with a vCPU free for each of them (8 vCPUs), they run alongside the
torture cases, each on its own vCPU, as the run on record did; `--soak-alone` runs them one after another at the
end instead. moalhaddar/docx-to-pdf's soak stops at 1000 conversions, the length on record, unless
`--soak-moalhaddar` says otherwise: at about 0.3 conversions a second, 3000 would take hours on its own.

A soak's row gives LibreOffice's memory over the first tenth and the second half, and two slopes: over the last nine
tenths, and over the second half. A soak rose when its nine tenths rise at 2σ and more steeply than the row's floor,
1 % of the second half's level per 1000 conversions; under the floor it does not grow: at a spread of 1 MiB a
3000-conversion soak resolves a fraction of a MiB per 1000 from zero, which is no leak. The second half then says
whether it still rises. When the second half is below the rise at 2σ, the memory climbed early and levelled off. It
grows when the second half itself rises at 2σ and more steeply than the floor. When the second half shows neither,
the row says the second half is unresolved: over 500 requests a slow climb under a wide spread cannot be told from
flat, and a step early in the soak fits as a rise over nine tenths that a flat second half is not below at 2σ.
The nine-tenths slope is still printed and kept in `results.json`, so older runs stay comparable.

## What it does not show

- **moalhaddar/docx-to-pdf on arm64.** Its only image is linux/amd64. On an arm64 host, the run says so and skips it,
  because emulation would not be a measurement.
- **Rendering.** Gotenberg's PDFs of the corpus have never been compared with oxidestack/docx-to-pdf's. The tables do
  not claim anything about rendering.
- **The documents.** They are generated (seeded, the same bytes every time) or fetched by commit and checked by
  SHA-256. The random bytes are seeded too, so they are not the bytes on record, and what LibreOffice makes of random
  bytes depends on the bytes. The soak cycles the seven generated reports. The soak on record also used 44 of
  LibreOffice's test documents, which are not distributed here.

## The compose file

`compose.yaml` holds the three services as each ships, for trying them by hand
(`docker compose -f examples/compare/compose.yaml up -d`; moalhaddar/docx-to-pdf needs `--profile amd64`). It also
holds the runner. The comparison never reuses those long-running containers. Each measurement starts a fresh one,
because a container's memory peak cannot be reset.
