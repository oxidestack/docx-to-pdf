# Migrating from moalhaddar/docx-to-pdf

For anyone who runs `moalhaddar/docx-to-pdf` (2.1.0-x) or calls it, and wants to move to this service. It converts
the same documents through the same interface, with a current LibreOffice, without the original's failure modes, and
with OpenTelemetry built in. This page says what stays the same, what a client changes, what a deployment changes,
and, at the end, every deliberate difference with its reason. A test holds the page to the list of differences and to
the other guide pages, so a difference cannot exist without a row here.

## What stays the same

- Port **8080**.
- `POST /pdf` and `POST /docx-to-pdf`, a `multipart/form-data` body with the document in the file part named
  **`document`**. The part's file name and `Content-Type` play no part.
- Success is **`200`**, `Content-Type: application/pdf`, the PDF as the body.
- Cross-origin requests to those two paths: `Access-Control-Allow-Origin: *`, and a preflight answered with
  `Access-Control-Allow-Methods: POST`, the requested headers allowed and `Access-Control-Max-Age: 1800`.
- The documents that convert: DOCX, DOC, ODT and RTF, and the Word-family variants the original really converts:
  DOCM, DOTX, OTT and flat ODT ([the formats accepted](api.md#the-formats-accepted)).
- The look of the PDF: the same pages, page sizes and words. The PDF is not byte for byte the same, because
  LibreOffice is newer.
- The free fonts the original shipped, and the same substitutes for the common Word fonts, such as Calibri, Arial and
  Times New Roman ([Fonts](operations.md#fonts)).

A client that sends a document, reads the PDF back, and treats every other answer as a failure needs no change.

## What a client changes

### Statuses

The original answered most errors with `500`, and some inputs that are not documents with a PDF. This service
answers each situation with its own status, and every error body is a problem document
([Errors](api.md#errors)).

| Situation | moalhaddar/docx-to-pdf | This service | Retry? |
| --- | --- | --- | --- |
| The body is not multipart, is malformed, or has no file part named `document` | 500 or 400 | 400 | no |
| An unknown path, every `/actuator` path included | 404 | 404 | no |
| A known path with another method (`GET /pdf`) | 405 | 405 | no |
| No byte of the request arrives for two minutes | 500 | 408 | no |
| The document is over the size limit (10 MiB by default) | 500, after the upload | 413, before the rest of the upload | no |
| A zip bomb, or a document that needs more memory than an instance may take | no limit: the container runs out of memory or disk first | 413 | no |
| Text, an empty file, a spreadsheet, a picture, random bytes | 200, a PDF of whatever LibreOffice made of it | 415 | no |
| A document LibreOffice cannot open | 500 | 422 | no |
| Every instance busy past the wait limit, the queue full, starting, or shutting down | waited without limit | 503 with `Retry-After` | yes, after `Retry-After` |
| A conversion that runs past the conversion timeout (3 minutes by default) | waited without limit | 504 | once, at most |
| Anything else | 500 | 500 | no |

What to change:

- **Error bodies.** Spring's `{"timestamp","status","error","path"}` document and the HTML "Whitelabel Error Page"
  are gone. A client that reads them reads `status`, or only the HTTP status. Do not match on `detail`: it is a fixed
  sentence per situation, meant for a person ([every error](api.md#every-error-the-service-answers)).
- **Retries.** The original never refused work, so a client's only failure was its own read timeout. This service
  answers `503` when it is saturated. A client that retries only connection errors should also retry `503` after the
  `Retry-After` it carries; retrying a `504` once is reasonable; a `4xx` will not change on a retry.
- **Read timeout.** Set it above the wait limit plus the conversion timeout, 60 + 180 seconds by default, with a
  margin, so the client receives the service's `503` or `504` rather than cutting the connection itself.
- **Inputs that are not documents.** Plain text, empty files and other formats now get `415`. Convert them to DOCX
  first.
- **Pictures changed.** A `200` may carry `Docx-To-Pdf-Pictures: downscaled=<n>, replaced=<n>` when a picture too
  large to convert safely was scaled down or replaced ([Pictures changed before
  conversion](api.md#pictures-changed-before-conversion)). A client that wants to tell its user reads it; any other can
  ignore it.

## What a deployment changes

### Health checks and metrics

| Purpose | moalhaddar/docx-to-pdf | This service |
| --- | --- | --- |
| Liveness | `GET /actuator/health/liveness` | `GET /livez` |
| Readiness, and waiting for a start | `GET /actuator/health/readiness`, `GET /actuator/health` | `GET /readyz` |
| Prometheus metrics | — | `GET /metrics` |
| Traces, logs and metrics over OTLP | — | set `OTEL_EXPORTER_OTLP_ENDPOINT` |

Every `/actuator` path answers `404`, so replace each reference to it: Kubernetes probes, Testcontainers wait
strategies (`.forPath("/actuator/health")` becomes `.forPath("/readyz")`), monitoring checks, scripts.
[Health checks](operations.md#health-checks) says what each path answers and how long a startup check should wait;
[Metrics](operations.md#metrics) lists every metric, and [Telemetry](configuration.md#logs-and-telemetry) the
OpenTelemetry settings.

### Settings

Every setting is an environment variable; [Configuration](configuration.md) lists them all with their defaults. What
changes for a deployment of moalhaddar/docx-to-pdf is only what it used to set:

| moalhaddar/docx-to-pdf's setting | In this service |
| --- | --- |
| `SERVER_PORT` | Still read. `DOCX_TO_PDF_PORT` wins when both are set. |
| `POOL_SIZE`, `pool.size` | Still read, in that order. `DOCX_TO_PDF_POOL_SIZE` wins over both. |
| `SPRING_SERVLET_MULTIPART_MAX_FILE_SIZE`, `SPRING_SERVLET_MULTIPART_MAX_REQUEST_SIZE` | Not read: set `DOCX_TO_PDF_MAX_DOCUMENT_SIZE`. A value such as `1000MB` keeps its meaning. Unchanged, the limit is 10 MiB and a larger document gets `413`. |
| `SERVER_TOMCAT_CONNECTION_TIMEOUT` | Not read: `DOCX_TO_PDF_IDLE_TIMEOUT` is the nearest, a duration rather than milliseconds; `120000` is its default, `120s`. |
| `SERVER_TOMCAT_MAX_HTTP_FORM_POST_SIZE` | Not read, and nothing replaces it: no path takes a URL-encoded form. |
| `JAVA_TOOL_OPTIONS` | Not read: there is no JVM. A `-Dpool.size` in it is not read either. Memory is sized from `DOCX_TO_PDF_INSTANCE_HARD_MEMORY` ([Sizing](operations.md#sizing)). |
| `LOGGING_LEVEL_*` | Not read: `RUST_LOG`. |
| other `SPRING_*`, `SERVER_TOMCAT_*`, `MANAGEMENT_*` | Not read. Remove them. |

Two settings are new and worth a look on any deployment: `DOCX_TO_PDF_CONVERSION_TIMEOUT` and
`DOCX_TO_PDF_POOL_WAIT_TIMEOUT`, which bound what the original waited for without limit.

### Memory, disk and CPU

Without a JVM, the container's memory is set by the LibreOffice instances. Each is killed and replaced, and its
request answered `413`, if it passes `DOCX_TO_PDF_INSTANCE_HARD_MEMORY` (2 GiB by default), so a memory limit of
256 MiB + pool size × (that limit + 256 MiB) cannot be passed, whatever the documents. A deployment that raised the
document limit for large documents with pictures raises the hard memory limit with it. `/tmp` must be writable, on
disk by default. [Sizing](operations.md#sizing) gives the formulas, and a probe that measures your own machine.

### The image

- Built for `linux/amd64` and `linux/arm64`: on an Arm machine, Apple Silicon included, it runs natively rather than
  under emulation.
- It runs as user `10001` with no passwd entry, any other non-root user works, the root file system may be
  read-only, and `/tmp` is the one directory it writes ([The container](operations.md#the-container)).
- PID 1 is `tini`; `SIGTERM` starts a graceful stop. Give the runtime the shutdown grace plus about 40 seconds
  before it kills the process ([Stopping](operations.md#stopping)).
- It carries no HTTP client, so no Docker `HEALTHCHECK`: use the orchestrator's HTTP checks.
- Two fonts the original image carried, Brown Std and GE Dinar Two, are under commercial licences and are not in this
  image: documents set in them get a substitute. Mount the font files you hold a licence for
  ([Mounting your own](operations.md#mounting-your-own)).

### The network

The service opens no connection of its own but to an OpenTelemetry collector, if one is set. The Helm chart's
NetworkPolicy is on by default and holds the pod to that: ingress on port 8080 only, egress to DNS (UDP and TCP 53, at
the cluster's `kube-dns` pods) and to the port of `otel.endpoint` only. A cluster with a node-local DNS cache sets
`networkPolicy.dnsTo` to its address, a collector to be reached at given addresses only sets `networkPolicy.otlpTo`,
and `networkPolicy.enabled: false` leaves the pod's network to a policy of your own
([Network](installation.md#network-the-service-fetches-nothing)).

### A checklist

1. Replace the image reference with this service's image.
2. Replace every `/actuator/health*` with `/livez` for liveness and `/readyz` for readiness and start-up waits.
3. Translate the settings with the table above, and remove the ones that are not read.
4. Raise the client's read timeout above the wait limit plus the conversion timeout, and retry `503` after
   `Retry-After`.
5. Look for code that parses the original's error body, relies on a `500`, or sends something other than a
   word-processing document.
6. Point `OTEL_EXPORTER_OTLP_ENDPOINT` at a collector, or scrape `/metrics`.
7. Run the client's tests against the new image.

## Every deliberate difference

Each difference below is on purpose: a bug of the original not carried over, a limit it lacked, or a Spring detail
that is not part of the interface. The PDF itself differs only as a newer LibreOffice lays it out.

| Difference | moalhaddar/docx-to-pdf | This service | Why |
| --- | --- | --- | --- |
| A newer LibreOffice | LibreOffice 7.3: page numbers and list numbers in some fields left empty, a long floating table that hangs the conversion, one line fewer per page at Word's 1.15 line spacing | A current LibreOffice: those fields filled, that table converted in under a second, one line more per page; the words are the same | LibreOffice 7.3 is no longer maintained, and the newer one renders these documents better. |
| Pictures in the PDF | every picture decoded and reduced to at most 300 DPI | embedded as they are, a JPEG byte for byte; `DOCX_TO_PDF_PDF_MAX_IMAGE_DPI=300` reduces them as the original did | A typical report converts with a third of the CPU, and pictures lose nothing; a picture placed above 300 DPI makes a larger PDF. |
| Content outside the document | a picture linked to a URL or a file, a remote template or an OLE link may be fetched | never fetched or read: a linked picture converts to its embedded copy, or to nothing | A document must not make the service reach other hosts on its network, or wait on a host that never answers. |
| Chinese, Japanese and Korean text | no font for it: empty boxes or a wrong fallback | drawn with Noto CJK | Text in those scripts should be readable in the PDF. |
| A request that is not a form upload | 500, or 400 with Spring's error | 400 | The request is the client's mistake, which is what a 4xx says. |
| A document over the size limit | 500 after the whole upload | 413, before the rest of the body is read | An oversized upload is the client's mistake, and reading it to its end costs the service for nothing. |
| A file that is not a word-processing document | 200, a PDF of whatever LibreOffice made of it, an RTF behind a byte-order mark typeset as its own markup | 415 | A PDF of garbage looks like success; only the formats the original really converts are accepted. |
| A damaged document | 500 | 422 | The document is at fault, not the service. |
| A document that needs too much memory | no limit: the container runs out of memory and every conversion in it is lost | 413 at once; the instance is replaced and other conversions go on | One document must not take the whole service down. |
| A zip bomb | no check: LibreOffice decompresses it to disk while it loads | 413 before any conversion | A few megabytes must not fill gigabytes of disk and memory. |
| A picture too large to decode | 200; above about 1 GiB decoded the picture silently missing | 200 with the picture scaled down, or replaced by a placeholder, and a `Docx-To-Pdf-Pictures` header saying so | Silently losing content is a bug, and refusing the document would leave the client with nothing. |
| A conversion that does not end | no answer, the instance held for as long as it runs | 504 after the conversion timeout, the instance replaced | A stuck document must not hold an instance, and the client, forever. |
| More requests than instances | they wait without limit | a bounded queue; past it, or past the wait limit, 503 with `Retry-After` | A client should learn that the service is saturated, and another replica can take the request. |
| A slow upload | 500 when the upload pauses for a few milliseconds, as real networks do | waited for, however long the whole upload takes | A pause in the network is not the client's error. |
| A client that stops sending | 500 after the connection timeout | 408 after two minutes without a byte, and the connection closed | The client stopped, which is what 408 says. |
| A client that disconnects during a conversion | the conversion runs on and holds the instance | the conversion is stopped and the instance freed at once | Nobody is waiting for that PDF. |
| Error bodies | Spring's JSON document, or an HTML page for a browser | `application/problem+json`, whose `detail` never names the converter | The standard format for HTTP errors, without the framework's internals. |
| Health checks and metrics | the Spring Actuator under `/actuator`; health reports up while the only instance is stuck in a conversion | `/livez`, `/readyz` and `/metrics`; `/readyz` is `503` while no instance can take a conversion | Readiness should say whether a request can be served now, so traffic goes to a replica that can. |
| The platform probe | none | `POST /probe` on a port of its own, off by default, measures the machine for sizing | Sizing a deployment needs numbers from the machine it runs on. |
| Cross-origin requests outside a conversion | 403 `Invalid CORS request` for a preflight on another path or for another method, and Spring's `Allow` headers | no cross-origin handling outside the two conversion paths; a preflight names `POST` alone | The browser enforces cross-origin rules by itself; the 403 and the headers were Spring's. |
| An instance that fails to start | the service fails to start | the start is tried again, up to three times, with `/readyz` at `503` meanwhile | A transient failure should not put the service in a restart loop. |
| A pool larger than the CPUs | the start can hang for good when the pool is larger than the CPUs the JVM sees | every instance starts, at any pool size | A start that never ends is a bug. |
| Spring, Tomcat and JVM settings | read | not read; their replacements are in [Settings](#settings) | There is no Spring, Tomcat or JVM in this service. |
