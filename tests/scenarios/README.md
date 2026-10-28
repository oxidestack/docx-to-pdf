# Scenarios

A **scenario** is `tests/scenarios/<name>.json`: HTTP steps on the interface's success path and CORS, the part
where oxidestack/docx-to-pdf answers exactly as moalhaddar/docx-to-pdf does. `make record` sends them to
moalhaddar/docx-to-pdf and writes its answers as the **golden** `<name>.golden.json`, a PDF body beside it as
`<name>.<step>.golden.pdf`. `cargo test --test oracle` replays every scenario against oxidestack/docx-to-pdf in
process and compares. The harness is `tests/oracle/`.

## A scenario

```json
{
  "description": "what it shows",
  "pending": "what oxidestack/docx-to-pdf still lacks to pass it",
  "steps": [
    {
      "method": "POST",
      "path": "/pdf",
      "headers": [["Origin", "http://a.example"]],
      "multipart": [
        {"name": "other", "value": "x"},
        {"name": "document", "file": "upstream/docx-to-pdf/examples/ex1/input.docx", "filename": "Отчёт.docx"}
      ],
      "chunked": true,
      "expectContinue": true,
      "compare": ["access-control-allow-origin"],
      "note": "read by people only"
    }
  ]
}
```

- `description`: what the scenario shows and, for a fixture made for it, how the fixture was made.
- `headers`: request headers in order. The client adds `Host`, the body's framing and `Connection: close`.
- `multipart`: a `multipart/form-data` body, parts in order. A part is a repository file (`file`, path from
  the repository root; `filename` defaults to the file's name, `contentType` to `application/octet-stream`)
  or a plain field (`value`). Fixtures are files already in the repository: upstream's examples first.
- `chunked`: the body goes `Transfer-Encoding: chunked`, else with `Content-Length`.
- `expectContinue`: sends `Expect: 100-continue` and waits up to 10 s for the interim answer before the body.
- `pending`: what oxidestack/docx-to-pdf still lacks to pass the scenario, in words. Replay expects such a
  scenario to be **red** and fails once it passes; the change that turns it green deletes the line.
  `DOCX_TO_PDF_SCENARIOS_STRICT=1 cargo test --test oracle` ignores `pending`, for the change at work.

Unknown keys are refused, so a typo fails loudly.

## The comparison

Every step compares the interim statuses (the `100`), the status, `Content-Type` and the body, plus each
header named in `compare`: its value, or its absence when moalhaddar/docx-to-pdf sent none. Everything else is
recorded in the golden and left uncompared, so oxidestack/docx-to-pdf may add headers moalhaddar/docx-to-pdf
lacks (`traceparent`, `x-request-id`) and drop Spring's (`Vary`, which Spring adds to every answer of a
CORS-mapped handler although with `Access-Control-Allow-Origin: *` the answer does not vary by origin). Header
names compare case-insensitively; repeated fields join with a comma and a space, and their order is not
compared, since only the order of same-named fields carries meaning (RFC 9110 §5.3).

- **Normalisation**: `Date` becomes `<date>` before recording and before comparing. Nothing else is
  normalised; a new time-based value earns a line in `normalise` (`tests/oracle/scenario.rs`) and here.
- **Bodies**: an `application/pdf` body by PDF equivalence (`src/pdf_equivalence`, default thresholds): page
  count, page sizes and text gate, the rendered pages are reported only, because LibreOffice 26.8 lays a
  document out differently from moalhaddar/docx-to-pdf's 7.3 in ways only a person can rate. Never its bytes,
  which change from run to run. Any other body byte for byte, stored as `text` (UTF-8) or `hex`.

## What becomes a scenario

The success path and CORS: there oxidestack/docx-to-pdf promises moalhaddar/docx-to-pdf's answer. An error
answer is never recorded. oxidestack/docx-to-pdf answers every error with an RFC 9457 problem document and the
status the situation calls for, where moalhaddar/docx-to-pdf often answers 500 or a Spring error page, so each
error is tested directly by the test of its limit (`tests/multipart_limits.rs`, `tests/allowlist.rs`, …) and
the difference is stated in the migration guide (`docs/guide/migration.md`).

Some facts on the success path stay out too:

| Fact | Why not a scenario |
| --- | --- |
| text, an empty file or random bytes convert | the allowlist refuses them with 415 |
| moalhaddar/docx-to-pdf's log lines | logs are not the interface |
| `Allow: GET, HEAD, POST, PUT, DELETE, OPTIONS, PATCH` on a preflight answer | Spring's default and false (`GET /pdf` is 405); a browser ignores `Allow` on a preflight |
| `OPTIONS /pdf` without `Origin` | not CORS and not a conversion |
| a preflight for a method other than `POST` | an error answer |
| start-up time and readiness | no request on the interface; emulated durations mean nothing |
| `/actuator` | not ported: probes are `/livez`, `/readyz` and `/metrics` |
| a conversion that never ends | no answer to record |

## Recording

`make record` starts or reuses `docx-to-pdf-oracle` and records every scenario; `make record ONLY=a,b`
records the named ones. moalhaddar/docx-to-pdf is amd64 only, so on Apple Silicon it runs emulated and a
conversion takes seconds; a step gets five minutes. Fixtures stay small office documents:
`tdf38575_fullWidthLine.docx` never finishes converting there, and a large random binary hangs its only
instance for many minutes, with every later request waiting behind it. Re-record after changing a scenario's
steps; replay refuses a golden whose answer count differs from the step count.
