# Configuration

Every setting is an environment variable, read once when the service starts. This page lists all of them; a test
compares its tables with the variables the code reads, so a setting cannot exist without a row here.

## How values are read

- **Unset or empty** is the same thing: a variable holding only spaces counts as unset, and the default applies.
  Leading and trailing spaces are trimmed from every value.
- **A value the service cannot use stops it at start**, with one line on standard error that names the variable
  and what is wrong, and a non-zero exit code. It never falls back to the default on a bad value.
- **Durations** take a unit: `500ms`, `30s`, `2m`, `1h`. A whole number, no fraction, no space inside; `30` alone
  is refused.
- **Sizes** are plain bytes (`1048576`) or a whole number with a unit, case-insensitive: `B`, `KiB`, `MiB`, `GiB`.
  `KB`, `MB` and `GB` are accepted and are **binary**, as in Spring's settings, so a value copied from a Spring
  application keeps its meaning: `10MB` is 10 485 760 bytes, the same as `10MiB`.
- **Ports** are whole numbers from 1 to 65535. One exception, for Kubernetes: a Service named `docx-to-pdf` makes
  Kubernetes inject `DOCX_TO_PDF_PORT=tcp://<cluster IP>:<port>` into every pod of its namespace (a "service
  link"), and one named `docx-to-pdf-probe` does the same to `DOCX_TO_PDF_PROBE_PORT`. A value of that form
  (`tcp://`, `udp://` or `sctp://` followed by an IP address and a port) in either of the two is not a setting: it
  is ignored with a warning in the log naming the variable, as though the variable were unset. Any other value that
  is not a port still stops the start. To use another port, set it explicitly, or set `enableServiceLinks: false`
  on the pod.

## Server

| Variable | Value | Default | Effect |
| --- | --- | --- | --- |
| `DOCX_TO_PDF_PORT` | port | `8080` | The port the service listens on, on every interface, for the conversion endpoint, the health checks and the metrics. A Kubernetes service link (`tcp://<ip>:<port>`) here is ignored with a warning, and the port falls back to `SERVER_PORT`, else `8080`. |
| `SERVER_PORT` | port | — | The same setting under the name moalhaddar/docx-to-pdf used. Read only when `DOCX_TO_PDF_PORT` is unset. |
| `DOCX_TO_PDF_PROBE_PORT` | port, not the same as the service port | unset: no probe | Turns on the platform probe, `POST /probe`, on a port of its own. The probe measures how fast this machine converts, for sizing a deployment; while it runs it holds every LibreOffice instance, so conversions get 503 and `/readyz` reports not ready. That is why it never shares the conversion port: keep this port away from clients. A Kubernetes service link here is ignored with a warning, and the probe stays off. |
| `DOCX_TO_PDF_IDLE_TIMEOUT` | duration above 0 | `120s` | The longest a request may go without receiving a byte of its head or body. Past it the client gets 408 and the connection is closed. A slow upload that keeps sending is waited for however long it takes in all; the clock does not run while a conversion runs or its PDF is sent. A kept-alive connection with no request in flight is closed after the same time, without an answer. |
| `DOCX_TO_PDF_SHUTDOWN_GRACE` | duration, `0s` allowed | the value of `DOCX_TO_PDF_CONVERSION_TIMEOUT` (`180s`) | After `SIGTERM`, how long requests already in flight may finish. A request still running after it gets 503; a PDF still being sent is cut. The default lets a conversion that started just before the signal run to its own limit. |

**Shutdown takes at most** the grace plus about 2 seconds for HTTP, then up to 5 seconds to stop the LibreOffice
instances, then, when telemetry is exported, one final flush bounded by the exporter's timeout. Give the container
runtime more than that before it kills the process: Kubernetes' `terminationGracePeriodSeconds` at the grace plus
about 40 seconds (`220` with the defaults), or the same number to `docker stop -t`.

## The pool of LibreOffice instances

The service converts with a fixed pool of LibreOffice processes ("instances"), one conversion per instance at a
time. A request that finds every instance busy waits in a queue.

| Variable | Value | Default | Effect |
| --- | --- | --- | --- |
| `DOCX_TO_PDF_POOL_SIZE` | whole number, 1 or more | `1` | How many instances run, so how many documents convert in parallel. Each instance is a separate LibreOffice process with its own memory; see `DOCX_TO_PDF_INSTANCE_HARD_MEMORY` for sizing the container. It also bounds how many documents have their pictures downscaled at once (`DOCX_TO_PDF_PICTURE_MAX_BITMAP`), a step that runs before a document takes an instance. |
| `POOL_SIZE` | whole number, 1 or more | — | The pool size under the name moalhaddar/docx-to-pdf used. Read only when `DOCX_TO_PDF_POOL_SIZE` is unset. |
| `pool.size` | whole number, 1 or more | — | moalhaddar/docx-to-pdf's other spelling of the pool size. Read only when neither `DOCX_TO_PDF_POOL_SIZE` nor `POOL_SIZE` is set. |
| `DOCX_TO_PDF_POOL_MAX_WAITING` | whole number, 0 or more | `16` | How many requests may wait for an instance at once. The next one gets 503 at once instead of joining the queue. `0`: no request waits; when every instance is busy, the answer is 503 straight away. |
| `DOCX_TO_PDF_POOL_WAIT_TIMEOUT` | duration, `0s` allowed | `60s` | The longest a request waits in the queue for a free instance, and the longest a document with a picture to downscale waits for its turn at that. Past it, 503. Set a client's read timeout above this plus `DOCX_TO_PDF_CONVERSION_TIMEOUT`, or the client gives up on a request the service would still answer. |
| `DOCX_TO_PDF_INSTANCE_START_TIMEOUT` | duration above 0 | `30s` | The longest an instance may take from being started to being ready. One that takes longer is killed and another start is tried after a pause. When the service starts, an instance that fails three times in a row stops the service with an error. An instance normally starts in well under a second. |
| `DOCX_TO_PDF_CONVERSION_TIMEOUT` | duration above 0 | `180s` | The longest one conversion may run. Past it the instance is killed and replaced, and the client gets 504. The default is about three times the slowest document measured within the default size limit (a document of 1000 tables, about a minute on one CPU), so a slower or busier machine still converts it; a typical report converts in about a second. |
| `DOCX_TO_PDF_INSTANCE_MAX_CONVERSIONS` | whole number, 0 or more | `200` | An instance is replaced by a fresh one after this many conversions; `0` never replaces it for that reason. Instances have shown no growth over thousands of conversions, so this is insurance; a replacement costs about a third of a second. |
| `DOCX_TO_PDF_INSTANCE_MAX_MEMORY` | size, `0` allowed | `1GiB` | An instance whose resident memory is above this after a conversion is replaced before it takes the next one; `0` sets no such ceiling. Typical reports leave an instance at 450 to 650 MiB; a very large document can leave it above 1 GiB for good, which this catches. |
| `DOCX_TO_PDF_INSTANCE_HARD_MEMORY` | size, `0` allowed; above `DOCX_TO_PDF_INSTANCE_MAX_MEMORY` unless one of the two is `0` | `2GiB` | While an instance converts, its resident memory is read every 100 ms; above this value the instance is killed at once, replaced, and the request gets 413. `0` sets no hard limit. A document that would take the whole container down costs one request instead. A DOCX or ODT whose pictures, as they will be after any downscale, add up to more than this value less 192 MiB (a picture with transparency counted at two and a half times its bitmap) gets the same 413 before any picture is decoded. Size the container's memory from it: **256 MiB + pool size × (this value + 256 MiB)**, which is 2.5 GiB for one instance at the default. |

## Limits on what a request may carry

| Variable | Value | Default | Effect |
| --- | --- | --- | --- |
| `DOCX_TO_PDF_MAX_DOCUMENT_SIZE` | size above 0 | `10MiB` | The largest document accepted. Past it the client gets 413 while the body is still arriving, so an oversized upload is not read to its end. The whole request may be 64 KiB larger, for the multipart framing and other form fields; a declared `Content-Length` above that gets 413 before any of the body is read. Raising it for documents with many pictures also needs more memory per instance: LibreOffice keeps every picture decoded while it converts, about 12 times the document's size for photographs. |
| `DOCX_TO_PDF_MAX_INFLATED_SIZE` | size; `0` for no limit, otherwise not below `DOCX_TO_PDF_MAX_DOCUMENT_SIZE` | `256MiB`, or 3 × `DOCX_TO_PDF_MAX_DOCUMENT_SIZE` when that is more | A DOCX or ODT is a ZIP archive. When its parts, decompressed, add up to more than this, the request gets 413 before any conversion starts. The parts are actually decompressed to count them; the sizes the archive declares are not trusted. This stops a "zip bomb", a small file that expands to gigabytes. Every ordinary document within the default size limit stays well under the default. |

## Pictures

| Variable | Value | Default | Effect |
| --- | --- | --- | --- |
| `DOCX_TO_PDF_PICTURE_MAX_BITMAP` | size above 0 | `256MiB` | A PNG or JPEG inside a DOCX or ODT whose decoded bitmap (width × height × 1, 3 or 4 bytes a pixel) is larger than this is changed before conversion: scaled down by the smallest power of two that brings it under the value, or, when it cannot be decoded safely, replaced by a placeholder. The answer's `Docx-To-Pdf-Pictures` header says what was changed. LibreOffice itself silently drops a picture above 1 073 741 823 bytes decoded; a higher value acts as that bound. |
| `DOCX_TO_PDF_PDF_MAX_IMAGE_DPI` | whole number, 1 or more | unset: pictures as they are | A picture placed in the document at a resolution above this is reduced to it in the PDF. Unset, every picture is embedded as it is (a JPEG byte for byte): a typical report converts with about a third of the CPU, but a document with pictures placed above 300 DPI gives a larger PDF. `300` reproduces what moalhaddar/docx-to-pdf produced. |

## Logs and telemetry

Logs always go to standard output: one JSON object per line, or human-readable text when standard output is a
terminal. Each JSON line carries the `trace_id` and `span_id` of the request it belongs to. Prometheus metrics are
always served at `GET /metrics`. Traces, logs and metrics are also exported over OTLP, the OpenTelemetry protocol,
once an endpoint is set; nothing is exported without one.

| Variable | Value | Default | Effect |
| --- | --- | --- | --- |
| `RUST_LOG` | a filter: a level (`error`, `warn`, `info`, `debug`, `trace`), or targets and levels such as `docx_to_pdf=debug,info` | `info` | Which log lines reach standard output. It does not change what is exported over OTLP, which is `info` and above. |
| `OTEL_EXPORTER_OTLP_ENDPOINT` | URL | unset: no export | The collector to send all three signals to, e.g. `http://otel-collector:4318` (HTTP) or `http://otel-collector:4317` (gRPC). Over HTTP each signal's path (`/v1/traces`, `/v1/logs`, `/v1/metrics`) is added to it. |
| `OTEL_EXPORTER_OTLP_<SIGNAL>_ENDPOINT` | URL | unset | The collector for one signal, `<SIGNAL>` being `TRACES`, `LOGS` or `METRICS`. Turns that signal's export on even without the general endpoint, and is used as given, path included (`http://collector:4318/v1/traces`). |
| `OTEL_EXPORTER_OTLP_PROTOCOL` | `grpc`, `http/protobuf` or `http/json` | `http/protobuf` | How the signals are sent. |
| `OTEL_EXPORTER_OTLP_<SIGNAL>_PROTOCOL` | as above | the general protocol | The protocol for one signal. |
| `OTEL_<SIGNAL>_EXPORTER` | `otlp` or `none` | `otlp` | `none` turns one signal's export off while the others stay on (`OTEL_TRACES_EXPORTER`, `OTEL_LOGS_EXPORTER`, `OTEL_METRICS_EXPORTER`). Any other value also turns it off, with a warning in the log. |
| `OTEL_SDK_DISABLED` | `true` or `false`, case-insensitive | `false` | `true` turns every OTLP export off. Logs to standard output and `/metrics` stay. |
| `OTEL_SERVICE_NAME` | text | `docx-to-pdf` | The `service.name` every signal carries. |
| `OTEL_RESOURCE_ATTRIBUTES` | `key=value` pairs separated by commas | — | Extra attributes on every signal, such as `deployment.environment=prod`. The service adds its own `service.version` and `upstream.version`. |
| `OTEL_EXPORTER_OTLP_HEADERS` | `key=value` pairs separated by commas, values URL-encoded | — | Headers sent with every export, for a collector that wants a token. |
| `OTEL_EXPORTER_OTLP_<SIGNAL>_HEADERS` | as above | the general headers | Headers for one signal's export. |
| `OTEL_EXPORTER_OTLP_TIMEOUT` | milliseconds | `10000` | The longest one export may take. It also bounds the final flush at shutdown. |
| `OTEL_EXPORTER_OTLP_<SIGNAL>_TIMEOUT` | milliseconds | the general timeout | The same, for one signal. |

Compression of the export is not built in: `OTEL_EXPORTER_OTLP_COMPRESSION` is ignored with a warning, and exports go
uncompressed. The OpenTelemetry SDK's other standard variables (batching such as `OTEL_BSP_SCHEDULE_DELAY`, sampling
with `OTEL_TRACES_SAMPLER`, the metric interval `OTEL_METRIC_EXPORT_INTERVAL`) keep the meaning the OpenTelemetry
specification gives them; the service sets none of them itself.

## The system

| Variable | Value | Default | Effect |
| --- | --- | --- | --- |
| `TMPDIR` | a directory path | `/tmp` | Where the service writes while it works: each instance's LibreOffice profile, the uploaded document, the PDF on its way out, and pictures being scaled down. A conversion can hold up to three times the document's size there at once, or 256 MiB when documents are small. The container image sets it to `/tmp`; mount a writable volume there when the root file system is read-only. |

## Settings that are not read

moalhaddar/docx-to-pdf was a Spring Boot application, and its deployments often carry Spring and JVM settings. These
are ignored, so remove them or translate them to the rows above: `JAVA_TOOL_OPTIONS`, every `SPRING_*` (the
multipart size limits become `DOCX_TO_PDF_MAX_DOCUMENT_SIZE`), every `SERVER_TOMCAT_*` (the connection timeout
becomes `DOCX_TO_PDF_IDLE_TIMEOUT`, in a duration rather than milliseconds), every `MANAGEMENT_*`, and
`LOGGING_LEVEL_*` (becomes `RUST_LOG`).
