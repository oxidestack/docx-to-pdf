# Rendering baseline

oxidestack/docx-to-pdf's PDFs of the corpus's 7 third-party and docx2pdf documents approved by eye in a visual review
beside moalhaddar/docx-to-pdf's and Word's renderings (2026-10-21: all ok, none to fix), of its 11 generated documents
approved the same way (2026-10-24: all ok), and of its 37 feature documents, looked at page by page when they replaced
the review's LibreOffice test documents (2026-10-30; the user's review of them is pending). Later output of
oxidestack/docx-to-pdf is compared against them by PDF equivalence at its default thresholds (`Thresholds::default()`
in `src/pdf_equivalence/`), so a rendering regression fails while the layout changes from LibreOffice 7.3 to 26.8 are
judged once, by eye; `tests/corpus.rs` runs that comparison.

`manifest.tsv` names each PDF's input: `fixture` is the path it was reviewed under in the fixture tree
(the third-party and docx2pdf documents), or `corpus/<path>` under `tests/corpus/documents/` for the generated and
feature documents, `fixture_sha256` the input's hash, `built_from` the commit the oxidestack/docx-to-pdf image was built
from (`make image`, arm64). The inputs are in the corpus (`tests/corpus/documents/`, the same bytes);
the corpus manifest's `baseline` column names the PDF here that each document is checked against.
