# API reference

The service converts one office document to PDF per request. This page lists every path it answers, what a request
carries, every answer it can give, and every error. A test compares the error tables with the answers the code can
give, and replays each `curl` example below against the service, comparing its answer with the one shown, so the page
cannot drift from the service.

The examples assume the service on `localhost:8080`, its default port. Answers are shown as `curl -i` prints them,
trimmed to the headers that matter; a value in `<angle brackets>` varies.

## Paths

| Method and path | What it does |
| --- | --- |
| `POST /pdf` | Converts the document in the request to PDF. |
| `POST /docx-to-pdf` | The same conversion under a second name; the two are interchangeable. |
| `GET /livez` | Liveness: `200` while the process serves HTTP. |
| `GET /readyz` | Readiness: `200` once the service can convert, `503` before that and from the start of shutdown. |
| `GET /metrics` | Prometheus metrics, in the text exposition format. |
| `POST /probe` | The platform probe, on a port of its own and only when it is turned on (see [The platform probe](#the-platform-probe)). |

Every path that answers `GET` also answers `HEAD`. Any other path is `404`; a known path with another method is `405`
with an `Allow` header naming the methods it takes.

## Converting a document

```sh
curl -sS -D - -o report.pdf -F document=@report.docx http://localhost:8080/pdf
```

```http
HTTP/1.1 200 OK
content-type: application/pdf
content-length: <bytes>
```

The request is `multipart/form-data` with the document in a **file part named `document`**: a part whose
`Content-Disposition` carries a `filename` (or `filename*`). `curl -F document=@<file>` sends exactly that.

- **Only the first file part named `document` is converted.** Any later one, and every other field, is read past
  and ignored. A plain form field named `document` (one without a file name) is not a document.
- **The file name and the part's `Content-Type` play no part.** The format is decided from the document's bytes, so
  a DOCX named `scan.pdf` converts, and a picture named `letter.docx` is refused. A file name in any language is
  accepted and never reaches the file system.
- **The body may be sent with `Content-Length` or chunked.** A request that declares a `Content-Length` above the
  limit is refused with `413` before any of the body is read, so a client that sends `Expect: 100-continue` gets the
  `413` instead of `100 Continue`, and does not upload the file at all.
- **The document may be up to 10 MiB** by default
  ([`DOCX_TO_PDF_MAX_DOCUMENT_SIZE`](configuration.md#limits-on-what-a-request-may-carry)), and the whole request 64 KiB
  more, for the multipart framing and other fields.

The answer to a converted document is `200` with `Content-Type: application/pdf`, a `Content-Length`, and the PDF as
the body. Nothing else in the request changes the PDF: there are no options.

### The formats accepted

| Format | How it is recognised |
| --- | --- |
| DOCX (Word 2007 and later), DOCM (macro-enabled; macros never run), DOTX (Word template) | A ZIP archive whose `[Content_Types].xml` declares the Word main document type of that format. |
| ODT (OpenDocument text), OTT (its template) | A ZIP archive whose `mimetype` entry is `application/vnd.oasis.opendocument.text`, or `…text-template`. |
| Flat ODT (`.fodt`, OpenDocument text as one XML file) | An XML document whose root is `office:document` with `office:mimetype="application/vnd.oasis.opendocument.text"`, the attribute ending within the first 4000 bytes. |
| DOC (Word 97-2003) | A compound file (OLE) whose root holds a stream named `WordDocument`. |
| RTF | The bytes start with `{\rtf`. An RTF preceded by a UTF-8 byte-order mark is refused. |

Everything else gets `415`, before any conversion starts: spreadsheets, presentations, PDFs, pictures, plain text,
HTML, an empty file. Only the structure that identifies a format has to be intact: a truncated DOCX whose
`[Content_Types].xml` survived, or a DOC with a damaged text stream, is accepted, and LibreOffice decides whether it
can open it. When it cannot, the answer is `422`.

### Pictures changed before conversion

LibreOffice silently drops a picture whose decoded bitmap is above about 1 GiB, which leaves an empty frame in the
PDF. To prevent that, a PNG or JPEG in a DOCX, DOCM, DOTX, ODT or OTT whose decoded bitmap is larger than
[`DOCX_TO_PDF_PICTURE_MAX_BITMAP`](configuration.md#pictures) (256 MiB by default) is scaled down before conversion,
or replaced by a placeholder when it cannot be decoded safely. When that happens, the `200` answer carries a header
that says so:

```http
Docx-To-Pdf-Pictures: downscaled=1, replaced=0
```

The header is absent when no picture was changed.

Before any picture is decoded, the service adds up what LibreOffice would hold for the document's PNG and JPEG
pictures, each as it will be after any downscale, and counting a picture with transparency at two and a half times its
bitmap. When that passes what
[`DOCX_TO_PDF_INSTANCE_HARD_MEMORY`](configuration.md#the-pool-of-libreoffice-instances) leaves (the limit less
192 MiB), the answer is the memory `413` at once, without the minute of downscaling the conversion would not survive.
Documents whose pictures need downscaling take their turn: at most as many at once as
[`DOCX_TO_PDF_POOL_SIZE`](configuration.md#the-pool-of-libreoffice-instances), the others waiting.

### Calling from a browser

The two conversion paths answer cross-origin requests from any origin: an answer to a request with an `Origin`
header carries `Access-Control-Allow-Origin: *` and exposes `Docx-To-Pdf-Pictures` to the page. A preflight is
answered with `200`:

```sh
curl -sS -i -X OPTIONS -H 'Origin: https://app.example' -H 'Access-Control-Request-Method: POST' -H 'Access-Control-Request-Headers: traceparent' http://localhost:8080/pdf
```

```http
HTTP/1.1 200 OK
access-control-allow-origin: *
access-control-allow-methods: POST
access-control-allow-headers: traceparent
access-control-max-age: 1800
```

The headers a preflight asks for are allowed, whichever they are, and the browser may cache the answer for 30
minutes. A preflight always names `POST` alone, whatever method it asked about. The other paths have no
cross-origin headers.

## Errors

Every error is a problem document ([RFC 9457](https://www.rfc-editor.org/rfc/rfc9457)) with
`Content-Type: application/problem+json` and exactly four fields:

| Field | Value |
| --- | --- |
| `type` | Always `about:blank`: the status says what kind of error it is. |
| `title` | The status's reason phrase, such as `Unsupported Media Type`. |
| `status` | The status code, as a number. |
| `detail` | A fixed sentence for the situation, from the table below. It never contains the file name, a path, the document's content or a stack trace, so it is safe to show or log. |

```sh
curl -sS -i -F document=@photo.png http://localhost:8080/pdf
```

```http
HTTP/1.1 415 Unsupported Media Type
content-type: application/problem+json

{"type":"about:blank","title":"Unsupported Media Type","status":415,"detail":"the document is not DOCX, DOC, ODT or RTF"}
```

A client should decide by the status. The `detail` tells a person which situation it was.

### Every error the service answers

| Status | `detail` | When |
| --- | --- | --- |
| 400 | `the body is not multipart/form-data with a boundary` | The request has no `Content-Type`, another one, or a multipart one without a boundary. |
| 400 | `the multipart body is malformed` | The multipart body cannot be parsed, or ends before its closing boundary. |
| 400 | `the body has no file part named "document"` | No part is a file part named `document`. |
| 404 | `no such path; the service converts at POST /pdf and POST /docx-to-pdf` | The path is not one of those in [Paths](#paths). |
| 405 | `this path does not take this method; see Allow` | The path exists but not with this method; `Allow` names the ones it takes. A `HEAD` to a conversion path gets this status with no body. |
| 408 | `no part of the request arrived within the idle limit` | No byte of the request's head or body arrived for [`DOCX_TO_PDF_IDLE_TIMEOUT`](configuration.md#server) (2 minutes by default). The answer carries `Connection: close` and the connection is closed. |
| 413 | `the document or the request is over the size limit` | The document is larger than [`DOCX_TO_PDF_MAX_DOCUMENT_SIZE`](configuration.md#limits-on-what-a-request-may-carry), or the request than that plus 64 KiB. Answered as soon as the limit is crossed: the rest of the body is not read. |
| 413 | `the document's compressed parts inflate past the size limit` | A DOCX, DOCM, DOTX, ODT or OTT whose parts, decompressed, add up to more than [`DOCX_TO_PDF_MAX_INFLATED_SIZE`](configuration.md#limits-on-what-a-request-may-carry): a "zip bomb". Answered before any conversion starts. |
| 413 | `the document needs more memory to convert than the service allows` | The conversion took more memory than [`DOCX_TO_PDF_INSTANCE_HARD_MEMORY`](configuration.md#the-pool-of-libreoffice-instances) and was stopped; or, before any conversion, the document's pictures add up to more than that limit leaves ([Pictures changed before conversion](#pictures-changed-before-conversion)). A retry with the same document will most likely meet the same limit. |
| 415 | `the document is not DOCX, DOC, ODT or RTF` | The document is not one of [the formats accepted](#the-formats-accepted), or is empty. |
| 422 | `the document could not be loaded` | The format is accepted, but LibreOffice cannot open the document: it is damaged in a way it cannot read past. |
| 500 | `the service failed to answer this request` | Anything else: the LibreOffice instance died during the conversion and again during the automatic retry, LibreOffice could not write the PDF, or the service could not write the document to its temporary directory. The service logs the cause. |
| 503 | `the service is not ready to convert documents yet` | The service has just started and its LibreOffice instances are not up yet. `Retry-After: 30`. |
| 503 | `no instance came free within the wait limit` | Every instance stayed busy for [`DOCX_TO_PDF_POOL_WAIT_TIMEOUT`](configuration.md#the-pool-of-libreoffice-instances); or, for a document with a picture to downscale, as many other documents as `DOCX_TO_PDF_POOL_SIZE` were downscaling theirs all that time. `Retry-After: 10`. |
| 503 | `too many requests are already waiting for an instance` | [`DOCX_TO_PDF_POOL_MAX_WAITING`](configuration.md#the-pool-of-libreoffice-instances) requests are already queued. Answered at once. `Retry-After: 10`. |
| 503 | `the service is shutting down` | The service received `SIGTERM`. A request that had not reached an instance gets `Retry-After: 10`; one still running at the end of the shutdown grace gets `Retry-After: 1`. Another replica can take it. |
| 503 | `the service is measuring its platform; another replica can convert meanwhile` | The platform probe holds every instance. `Retry-After: 10`. |
| 504 | `the conversion did not finish within the conversion timeout` | The conversion ran longer than [`DOCX_TO_PDF_CONVERSION_TIMEOUT`](configuration.md#the-pool-of-libreoffice-instances) (3 minutes by default) and was stopped. |

Every `503` carries `Retry-After`, in whole seconds. The service retries a conversion once by itself when its
LibreOffice instance dies under it, so a `500` has already been retried. A `422`, a `413` and a `415` will not change
on a retry with the same document; a `503` and a `504` may.

The errors of the conversion paths carry the same cross-origin headers as their `200`, so a page in a browser can
read them.

### Examples

A form whose file field has another name:

```sh
curl -sS -i -F file=@report.docx http://localhost:8080/pdf
```

```http
HTTP/1.1 400 Bad Request
content-type: application/problem+json

{"type":"about:blank","title":"Bad Request","status":400,"detail":"the body has no file part named \"document\""}
```

A document over the size limit (here 11 MiB, the default limit being 10 MiB):

```sh
curl -sS -i -F document=@scan.docx http://localhost:8080/pdf
```

```http
HTTP/1.1 413 Payload Too Large
content-type: application/problem+json

{"type":"about:blank","title":"Payload Too Large","status":413,"detail":"the document or the request is over the size limit"}
```

A DOCX of a few hundred KiB whose parts decompress to 257 MiB, past the default limit of 256 MiB:

```sh
curl -sS -i -F document=@bomb.docx http://localhost:8080/pdf
```

```http
HTTP/1.1 413 Payload Too Large
content-type: application/problem+json

{"type":"about:blank","title":"Payload Too Large","status":413,"detail":"the document's compressed parts inflate past the size limit"}
```

A document in an accepted format that LibreOffice cannot open:

```sh
curl -sS -i -F document=@damaged.rtf http://localhost:8080/docx-to-pdf
```

```http
HTTP/1.1 422 Unprocessable Entity
content-type: application/problem+json

{"type":"about:blank","title":"Unprocessable Entity","status":422,"detail":"the document could not be loaded"}
```

A conversion path called with `GET`:

```sh
curl -sS -i http://localhost:8080/pdf
```

```http
HTTP/1.1 405 Method Not Allowed
allow: POST
content-type: application/problem+json

{"type":"about:blank","title":"Method Not Allowed","status":405,"detail":"this path does not take this method; see Allow"}
```

A path that does not exist:

```sh
curl -sS -i http://localhost:8080/actuator/health
```

```http
HTTP/1.1 404 Not Found
content-type: application/problem+json

{"type":"about:blank","title":"Not Found","status":404,"detail":"no such path; the service converts at POST /pdf and POST /docx-to-pdf"}
```

## Health checks

```sh
curl -sS -i http://localhost:8080/livez
```

```http
HTTP/1.1 200 OK
content-type: text/plain; charset=utf-8

ok
```

`/livez` answers `200 ok` for as long as the process serves HTTP, whatever state LibreOffice is in. Use it as the
liveness check: a failure means the process should be restarted.

```sh
curl -sS -i http://localhost:8080/readyz
```

```http
HTTP/1.1 200 OK
content-type: text/plain; charset=utf-8

ok
```

`/readyz` answers `200 ok` once the LibreOffice instances have started, and `503` with the plain-text body
`not ready` before that, while the platform probe runs, and from the moment the service receives `SIGTERM`. Use it as
the readiness check, so that a load balancer sends conversions only to a replica that can take them. These two
answers are plain text, not problem documents. How to wire them into a container platform, and how long a startup
check should allow, is in [Operations: health checks](operations.md#health-checks).

## Metrics

```sh
curl -sS -i http://localhost:8080/metrics
```

```http
HTTP/1.1 200 OK
content-type: text/plain; version=0.0.4; charset=utf-8

<the metrics>
```

`/metrics` serves the service's metrics in the Prometheus text format: the HTTP requests by path and status, the
conversions by outcome and their durations, the sizes of documents and PDFs, the instance pool, and the process. It
is always on and needs no setting. The same metrics, with traces and logs, can also be exported over OpenTelemetry;
see [Logs and telemetry](configuration.md#logs-and-telemetry). Every metric, its Prometheus name and what it means is
listed in [Operations: metrics](operations.md#metrics).

## The platform probe

`POST /probe` measures how fast this machine converts, for sizing a deployment. It exists only when
[`DOCX_TO_PDF_PROBE_PORT`](configuration.md#server) is set, and only on that port, never on the conversion port:
while it runs it holds every LibreOffice instance, so conversions get `503` and `/readyz` reports not ready. It takes a
minute or two. What its report means and how to turn it into a deployment's size is in
[Operations: measuring a platform](operations.md#measuring-a-platform-the-sizing-probe).

The request is `multipart/form-data` with 1 to 16 file parts named `document`, the documents to measure with, each
within the document size limit. Optional query parameters:

| Parameter | Default | Meaning |
| --- | --- | --- |
| `rounds` | `5` | How many times each document is converted one at a time (1 to 100). |
| `busy` | 120 + 3 × the pool size | How many conversions run with every instance busy (up to 10000). |
| `disk_mib` | `1024` | How many MiB are written to measure the disk (up to 65536); `0` skips it. |
| `cpus` | the pool size | The number of CPUs the answer's formulas are worked out for (1 to 1024). |
| `name` | the CPU model and count | A name for the platform in the answer. |

The answer is `200` with a JSON report. Its errors are problem documents, as above; besides the ones of the
conversion paths that apply to it, the probe has these:

| Status | `detail` | When |
| --- | --- | --- |
| 400 | `rounds, busy, cpus and disk_mib are whole numbers within their bounds` | A parameter is not a whole number, or is above its bound. |
| 400 | `the probe takes rounds, busy, disk_mib, cpus and name` | The query has another parameter. |
| 413 | `the document or the request is over the size limit` | A document is over the size limit, or the request carries more than 16 documents. |
| 422 | `a probe document could not be loaded` | LibreOffice cannot open one of the documents. |
| 503 | `a probe is already running` | Another probe holds the instances. `Retry-After: 30`. |
| 503 | `the conversions under way did not end in time` | The probe waits for the conversions already running to finish before it takes the instances; they did not finish within the conversion timeout plus the instance start timeout. `Retry-After: 30`. |
| 503 | `the service is shutting down` | The service received `SIGTERM`. `Retry-After: 30`. |
