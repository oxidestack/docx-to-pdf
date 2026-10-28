# Corpus

The **corpus**: real documents, converted by oxidestack/docx-to-pdf and checked against moalhaddar/docx-to-pdf's PDFs
and against oxidestack/docx-to-pdf's own approved rendering baseline. The harness is `tests/corpus.rs`.

## What is in it

The bench's documents (`tools/bench/generate.py`, `make bench-generate`), the user's choice of 2026-10-22, every
one whose PDF from oxidestack/docx-to-pdf is at most 1 MB, so the inputs and both PDFs fit in the repository: 55
documents, 3.7 MB of inputs.

- The 44 feature documents of the visual review (`tests/rendering-baseline/`), as a sibling project's corpus fetcher
  fetched them (their names in the manifest still start `objects-`):
  - `documents/third-party/libreoffice/`: 37 test documents of LibreOffice core, under the Mozilla Public License 2.0;
  - `documents/third-party/apache-poi/`: 2 test documents of Apache POI, under the Apache License 2.0;
  - `documents/docx2pdf/`: 5 plain text documents of the project author's docx2pdf fixtures (`harvest11`,
    `ramp_adv_1`, `sample1`, `sample2`, `size40`), generated there: the author's own work, not a
    third party's.

  Each `third-party/<origin>/` folder holds its documents unmodified, the origin's licence text (`LICENSE`, and
  Apache POI's `NOTICE`), and a `README.md` naming each document's path and git blob in the origin's repository.
  `NOTICE` at the repository's root names them too.
- `documents/{text,report,tables,px}/`: the generated classes the bench seeds, so the bytes are the same on every
  run: plain text of 1, 10 and 100 pages, the report shape without pictures (2, 5, 20 pages), 10 tables, one
  picture of 4 to 256 megapixels.

Left out, bench only: the documents whose PDF runs to megabytes (`bytes-*`, `mix-*`, `report-img*`, `tables-100`,
`tables-1000`, `text-500p`; 2 to 53 MB each) and the hostile ones (a 1-gigapixel picture, and `tdf38575`, which hangs
moalhaddar/docx-to-pdf for good).

`manifest.tsv` has one row per document: `name`, `document` (the path under `documents/`), its `origin`
(`libreoffice`, `apache-poi`, `docx2pdf` or `generated`; `tests/corpus.rs` holds a third-party one to its folder and
licence), its `sha256`, the `feature` it was chosen for, and `baseline`, the approved PDF under
`tests/rendering-baseline/` (empty until the user approves one).

## The checks

| Against | What | Gates |
| --- | --- | --- |
| moalhaddar/docx-to-pdf's PDF, `oracle/<name>.pdf` | page count, page sizes, the document's words | yes, unless `divergences.tsv` lists it |
| moalhaddar/docx-to-pdf's PDF | the rendered pages: the largest differing share and its page | no, reported |
| the rendering baseline | PDF equivalence at its default thresholds | yes; without a baseline PDF, reported |

`divergences.tsv` lists the known differences from moalhaddar/docx-to-pdf, each a deliberate difference or an open
divergence: `name`, `check` (`pages`, `sizes` or `text`) and `divergence`, the reason in words. The text is compared
as the document's words, without their order (`document_words_difference`): LibreOffice 26.8 fits a line more or
less on a page than 7.3 and extracts a floating table or a footnote elsewhere in the flow, which moves text across
pages without changing it. A row whose difference is gone fails the run, so the list empties as the divergences do.
The check `converts` names a document moalhaddar/docx-to-pdf does not convert at all (`tdf38575_fullWidthLine` hangs
it): it has no golden, is not sent at recording, and is checked against the baseline only.

The rendered pages against moalhaddar/docx-to-pdf are reported, not gated: the same layout changes alter many
pages in ways only a person can rate, so they were judged once, by eye, into the rendering baseline, and it is the
baseline that gates rendering.

## Commands

| Task | Command |
| --- | --- |
| Record moalhaddar/docx-to-pdf's PDFs into `oracle/` (`ONLY=a,b`: some) | `make corpus-record [ONLY=]` |
| Run the corpus against the image, under a Kubernetes deployment's security context | `make image-corpus [IMAGE=]` |

The run writes `target/corpus/report.md`, one row per document, and oxidestack/docx-to-pdf's PDFs to
`target/corpus/ours/`. CI runs it in the image job (`make image-ci`). It runs against the image only: the
rendering baseline was made with the image's fonts (free families only: no font with a commercial licence is
redistributed), and a comparison across LibreOffice versions means something only with the same font set on both
sides.

## Approving a rendering

A document without a baseline PDF is reported as awaiting approval. When the user approves
oxidestack/docx-to-pdf's PDF of it (`target/corpus/ours/<name>.pdf`), copy it to `tests/rendering-baseline/`, add
a row to that manifest, and name it in this manifest's `baseline` column.
