# Troubleshooting

What a client or an operator meets when something goes wrong, what it means, and what to change. The first part has
one entry for every error the service answers, headed by its status and its `detail`, so you can search this page
for the sentence in the answer you got. The second part covers problems that are not an error answer: a pod killed
for memory, pictures changed, wrong fonts, slow conversions. Each entry names the setting to change, the metric to
watch or the log line to look for. A test holds the page to the service: every error of the
[API reference](api.md#every-error-the-service-answers) has an entry here, and every setting, metric and log line
named below exists.

**Where to look.** Settings are in [Configuration](configuration.md); metrics are served at `GET /metrics` and listed
in [Operations: metrics](operations.md#metrics). Logs are one JSON object per line on standard output; a log line is
quoted here as its message field, such as `"message":"the conversion exceeded its timeout"`, so you can search the
logs for it. Every line written while a request was served carries that request's `trace_id`.

A Prometheus counter by answer: `http_server_request_duration_seconds_count` with the label
`http_response_status_code` counts every request by its status, and `docx_to_pdf_conversions_total` counts
conversions by how they ended.

## Errors

### 400 `the body is not multipart/form-data with a boundary`

The request is not a form upload. The document must be sent as `multipart/form-data`, in a file part named
`document`; a raw body (`curl --data-binary @report.docx`), JSON with the document in base64, or a hand-written
`Content-Type: multipart/form-data` header without its `boundary=` all get this. With `curl`, use
`-F document=@report.docx` and let it write the header; in code, let the HTTP library build the multipart body rather
than setting `Content-Type` yourself. Count them with
`http_server_request_duration_seconds_count{http_response_status_code="400"}`.

### 400 `the multipart body is malformed`

The body says it is multipart but cannot be parsed: a boundary that does not match the header's, or a body cut off
before its closing boundary, typically a client or proxy that gave up mid-upload. Look at the client's library and
at anything between it and the service that rewrites bodies. It shows in
`http_server_request_duration_seconds_count{http_response_status_code="400"}`.

### 400 `the body has no file part named "document"`

The form has no file part named `document`. Either the field has another name (`file`, `upload`), or it is a plain
form field without a file name: `curl -F document=<report.docx` (with `<`) sends the file's content as text and is
refused, `curl -F document=@report.docx` (with `@`) sends a file. Rename the field, or make the part a file part. It
shows in `http_server_request_duration_seconds_count{http_response_status_code="400"}`.

### 404 `no such path; the service converts at POST /pdf and POST /docx-to-pdf`

The path does not exist. The service answers only the paths listed in [the API reference](api.md#paths): a client
or health check still pointing at another path, such as a Spring Boot `/actuator/health`, gets `404`. Use `/livez`
and `/readyz` for health checks. Count them with
`http_server_request_duration_seconds_count{http_response_status_code="404"}`.

### 405 `this path does not take this method; see Allow`

The path exists but not with this method: a `GET` to `/pdf`, a `POST` to `/readyz`. The `Allow` header of the answer
names the methods the path takes. A `HEAD` to a conversion path gets this status too, which matters for a load
balancer health check configured with `HEAD /pdf`: point it at `/readyz` instead. It shows in
`http_server_request_duration_seconds_count{http_response_status_code="405"}`.

### 408 `no part of the request arrived within the idle limit`

The client started a request and then sent nothing, not even one byte, for
[`DOCX_TO_PDF_IDLE_TIMEOUT`](configuration.md#server) (2 minutes by default). A slow upload that keeps sending is
never cut; this is a client that stalled or a connection that died on the way. The connection is closed after the
answer. The service logs `"message":"closing a connection idle past the limit"`. If legitimate clients stall that
long (a mobile network, an upload that pauses while the client reads the file from slow storage), raise
`DOCX_TO_PDF_IDLE_TIMEOUT`.

### 413 `the document or the request is over the size limit`

The document is larger than [`DOCX_TO_PDF_MAX_DOCUMENT_SIZE`](configuration.md#limits-on-what-a-request-may-carry)
(10 MiB by default), or the whole request is larger than that plus 64 KiB. Raise the limit if such documents are
expected; `docx_to_pdf_document_size_bytes` shows the sizes you actually receive. A document with many photographs
needs more memory per instance as it grows (see the next-but-one entry), so raise
`DOCX_TO_PDF_INSTANCE_HARD_MEMORY` with it if those documents then get the memory `413`.

A `413` whose body is not a problem document (HTML, or `Content-Type` other than `application/problem+json`) comes
from a proxy in front of the service, not from the service: raise that proxy's own body limit, such as nginx's
`client_max_body_size`, which is 1 MiB by default.

The platform probe answers the same `413` when a request carries more than 16 documents.

### 413 `the document's compressed parts inflate past the size limit`

A DOCX or ODT is a ZIP archive, and this one's parts, decompressed, add up to more than
[`DOCX_TO_PDF_MAX_INFLATED_SIZE`](configuration.md#limits-on-what-a-request-may-carry) (256 MiB by default). That is
what a "zip bomb" looks like, and no ordinary document within the default size limit comes near it. The service logs
`"message":"the document's parts inflate past the limit; refused before conversion"` with the sizes it found. If a
genuine document is refused, raise `DOCX_TO_PDF_MAX_INFLATED_SIZE`; it cannot be set below the document size limit.

### 413 `the document needs more memory to convert than the service allows`

LibreOffice's memory passed [`DOCX_TO_PDF_INSTANCE_HARD_MEMORY`](configuration.md#the-pool-of-libreoffice-instances)
(2 GiB by default) while it converted this document, so its instance was killed and replaced, and the other
conversions went on. The request is not retried: the same document would kill the next instance too. The logs show
`"message":"the instance passed the hard memory limit during a conversion; killing it, the request fails"` and
`"message":"the conversion passed the hard memory limit"`, with the memory the instance had reached; the metric is
`docx_to_pdf_conversions_total{outcome="memory_limit"}`.

The same answer comes before any conversion when a DOCX or ODT's pictures, each as it will be after any downscale,
add up to more than the hard limit leaves (the limit less 192 MiB; a picture with transparency counts two and a half
times its bitmap): no instance is touched and the answer takes milliseconds. The log shows
`"message":"the document's pictures, after the planned downscale, need more memory than the hard limit leaves; refused before any is decoded"` <!-- markdownlint-disable-line MD013 --><!-- a log message, quoted whole -->
with `held_bytes`, `budget_bytes` and the number of pictures. The remedy is the same: raise the hard limit, or lower
[`DOCX_TO_PDF_PICTURE_MAX_BITMAP`](configuration.md#pictures) so that each oversized picture is downscaled further.

What takes the memory is mostly pictures: LibreOffice keeps every picture decoded while it converts, about 12 times
the document's size for photographs. To convert such documents, raise `DOCX_TO_PDF_INSTANCE_HARD_MEMORY`, and the
container's memory limit with it, by the formula in [Operations: Kubernetes](operations.md#kubernetes):
256 MiB + pool size × (the hard limit + 256 MiB). `docx_to_pdf_instance_memory_peak_bytes` shows how close your
documents come to the limit.

### 415 `the document is not DOCX, DOC, ODT or RTF`

The document's bytes are not one of [the formats accepted](api.md#the-formats-accepted). The file name and the
part's `Content-Type` are ignored, so renaming a file does not change the answer. The usual cases:

- a spreadsheet, a presentation, a PDF or a picture;
- an empty file, often a client that sent the part before the file was written;
- a "Word" file exported by a web application that is really HTML or plain text with a `.doc` name;
- a DOCX protected by a password to open: Word stores it encrypted inside a compound file that holds no Word
  document stream;
- an RTF that starts with a UTF-8 byte-order mark.

Open the file in a word processor and save it again as DOCX. Count them with
`docx_to_pdf_conversions_total{outcome="unsupported"}`.

### 422 `the document could not be loaded`

The format is one the service accepts, but LibreOffice cannot open this document: it is truncated, damaged, or
encrypted (a Word 97-2003 document with a password to open). The service logs
`"message":"the backend could not load the document"` with LibreOffice's own error in the `backend_error` field. A
retry gives the same answer. If Word opens the file, save it again from Word as DOCX and convert that. Count them with
`docx_to_pdf_conversions_total{outcome="not_loaded"}`.

### 500 `the service failed to answer this request`

Something failed that is not the document's fault, or not only. The log line at level `ERROR` with the request's
`trace_id` says which:

- `"message":"the conversion got no answer"`: the LibreOffice instance died during the conversion, and again on the
  automatic retry on another instance (`docx_to_pdf_conversions_total{outcome="dead_instance"}`). Each death also
  logs `"message":"the instance died during a conversion; retrying once on another instance"`. A document that kills
  LibreOffice twice will most likely do it again; if many documents do, look at the instances' memory
  (`docx_to_pdf_instance_memory_usage_bytes`) and at the kernel's out-of-memory messages.
- `"message":"the backend could not export the PDF"`: LibreOffice opened the document but could not write its PDF.
- `"message":"the spooled document could not be read back"` or
  `"message":"the document's pictures could not be checked"`: the service could not use its temporary directory.
  Check that [`TMPDIR`](configuration.md#the-system) (`/tmp` in the image) is writable and not full; on a read-only
  root file system it needs a volume of its own (see [Operations: `/tmp`](operations.md#tmp-on-disk-or-in-memory)).

### 503 `the service is not ready to convert documents yet`

The service has just started and none of its LibreOffice instances is up yet. The answer carries `Retry-After: 30`.
A readiness check on `/readyz` keeps traffic away until the first instance is up, normally within a second or two of
the start; if requests get this answer, the load balancer is sending traffic before `/readyz` is `200`. If the
service stays in this state, see [The service does not start](#the-service-does-not-start-or-never-becomes-ready);
`docx_to_pdf_pool_instances{state="starting"}` shows the instances still starting.

### 503 `no instance came free within the wait limit`

Every instance stayed busy for [`DOCX_TO_PDF_POOL_WAIT_TIMEOUT`](configuration.md#the-pool-of-libreoffice-instances)
(60 seconds by default) while this request waited in the queue. The service is short of capacity for the load it is
getting: `docx_to_pdf_pool_instances{state="idle"}` stays at 0 and `docx_to_pdf_pool_wait_duration_seconds` grows
toward the wait limit; `docx_to_pdf_pool_wait_duration_seconds_count{outcome="rejected"}` counts these answers. Add
replicas, or give each more CPUs and a matching `DOCX_TO_PDF_POOL_SIZE` (one instance per core; more instances than
cores make every conversion slower and add nothing). [Operations: sizing](operations.md#sizing) works the numbers
out. The answer carries `Retry-After: 10`, and a client that retries after it gets its PDF once the burst is over.

A document with a picture to downscale gets the same answer when, for the whole wait, as many other documents as
`DOCX_TO_PDF_POOL_SIZE` were downscaling theirs: that step runs before a document takes an instance and has the same
bound. The log then shows `"message":"no turn of the pictures step came free within the wait limit"`.

### 503 `too many requests are already waiting for an instance`

[`DOCX_TO_PDF_POOL_MAX_WAITING`](configuration.md#the-pool-of-libreoffice-instances) requests (16 by default) were
already queued, so this one was refused at once instead of joining the queue. The cause and the remedy are those of
the previous entry: too little capacity for the load. Raising `DOCX_TO_PDF_POOL_MAX_WAITING` only lengthens the
queue, and each queued request waits longer. `docx_to_pdf_pool_wait_duration_seconds_count{outcome="rejected"}`
counts these answers too. `Retry-After: 10`.

### 503 `the service is shutting down`

The service received `SIGTERM` (a rolling update, a scale-down, a node drain) and is no longer taking conversions.
A request that had not yet reached an instance gets `Retry-After: 10`; one still converting when
[`DOCX_TO_PDF_SHUTDOWN_GRACE`](configuration.md#server) ran out gets `Retry-After: 1`, and the service logs
`"message":"the grace period ran out; cutting the requests still in flight short"`. A client that retries reaches
another replica.

Requests that arrive after the signal mean the load balancer still routes to the pod: on Kubernetes, add the
`preStop` sleep of [Operations: stopping](operations.md#stopping). Requests cut at the end of the grace mean the grace
is shorter than your conversions: leave `DOCX_TO_PDF_SHUTDOWN_GRACE` at its default, the conversion timeout, and give
the pod a `terminationGracePeriodSeconds` of the grace plus about 40 seconds. The platform probe answers the same
`503`, with `Retry-After: 30`, when it is sent during a shutdown.

### 503 `the service is measuring its platform; another replica can convert meanwhile`

Someone is running the platform probe on this replica, and the probe holds every instance for a minute or two. The
service logs `"message":"the pool is reserved for the platform probe; draining it"` when it starts and
`"message":"the platform probe has given the pool back"` when it ends. Probe a replica that takes no traffic, and keep
[`DOCX_TO_PDF_PROBE_PORT`](configuration.md#server) away from clients; unset it when you are not measuring.

### 504 `the conversion did not finish within the conversion timeout`

LibreOffice was still converting after
[`DOCX_TO_PDF_CONVERSION_TIMEOUT`](configuration.md#the-pool-of-libreoffice-instances) (3 minutes by default), so its
instance was killed and replaced. The logs show
`"message":"the conversion did not end within the timeout; killing its instance"` and
`"message":"the conversion exceeded its timeout"`; the metric is `docx_to_pdf_conversions_total{outcome="timeout"}`.

The default is about three times the slowest document measured within the default size limit, so a `504` on an
ordinary document usually means the machine is slower than it should be: more instances than CPU cores
(`DOCX_TO_PDF_POOL_SIZE` above the CPU limit, or a CPU limit that throttles the pod), so each conversion gets a
fraction of a core. Compare `docx_to_pdf_conversion_duration_seconds` with the time the same document takes on an
idle replica. For documents that are genuinely slow (hundreds of tables, thousands of pages), raise
`DOCX_TO_PDF_CONVERSION_TIMEOUT`, and the clients' read timeouts above it.

## Errors of the platform probe

The probe, `POST /probe` on [`DOCX_TO_PDF_PROBE_PORT`](configuration.md#server), also answers the size `413` and the
shutting-down `503` above. How to use it is in
[Operations: measuring a platform](operations.md#measuring-a-platform-the-sizing-probe).

### 400 `rounds, busy, cpus and disk_mib are whole numbers within their bounds`

A query parameter is not a whole number, or is above its bound: `rounds` up to 100, `busy` up to 10000, `disk_mib` up
to 65536, `cpus` from 1 to 1024. See [the probe's parameters](api.md#the-platform-probe). The probe listens only when
`DOCX_TO_PDF_PROBE_PORT` is set.

### 400 `the probe takes rounds, busy, disk_mib, cpus and name`

The query has a parameter the probe does not know, often a misspelling such as `cpu=4` for `cpus=4`. The probe listens
only when `DOCX_TO_PDF_PROBE_PORT` is set.

### 422 `a probe document could not be loaded`

LibreOffice cannot open one of the documents sent to the probe; the probe measures nothing until every document
converts. Convert each document through `POST /pdf` first: the one that gets
[`422`](#422-the-document-could-not-be-loaded) there is the one to replace. The service logs
`"message":"the backend could not load the document"` for it.

### 503 `a probe is already running`

Another probe holds the instances of this replica. Wait for it to answer (a minute or two) and send yours after; the
answer carries `Retry-After: 30`. The service logs `"message":"the platform probe has given the pool back"` when the
running one ends.

### 503 `the conversions under way did not end in time`

Before it takes the instances, the probe waits for the conversions already running to finish. They did not finish
within [`DOCX_TO_PDF_CONVERSION_TIMEOUT`](configuration.md#the-pool-of-libreoffice-instances) plus
`DOCX_TO_PDF_INSTANCE_START_TIMEOUT`. The replica is busy with traffic: probe one that takes none.

## Other problems

### The service does not start, or never becomes ready

**It exits at once with one line on standard error**, `docx-to-pdf:` and a space, followed by the variable and what is
wrong with its value: a setting holds a value the service cannot use (`DOCX_TO_PDF_IDLE_TIMEOUT=30` without a unit, for
instance). Fix the value; [Configuration](configuration.md#how-values-are-read) says what each one takes.

**It logs `"message":"cannot listen"` and exits**: the port in `DOCX_TO_PDF_PORT` is taken, or below 1024 for a
non-root user.

**It logs `"message":"instance start failed"` or `"message":"instance start timed out; killed"`**, then
`"message":"retrying a failed first start"`, and after three failures in a row `"message":"the pool could not start"`
and exits: LibreOffice cannot start. Most often its temporary directory is not writable: with a read-only root file
system, `/tmp` needs a volume (see [Operations: the container](operations.md#the-container)). On a very slow or
heavily loaded machine an instance may need longer than `DOCX_TO_PDF_INSTANCE_START_TIMEOUT` (30 seconds); one
normally starts in well under a second.

**`/readyz` stays `503` while the process runs**: the instances are still starting or keep failing to.
`docx_to_pdf_pool_instances{state="starting"}` and `docx_to_pdf_instance_starts_total{reason="failed"}` show which.
A Kubernetes startup check must allow more than three times `DOCX_TO_PDF_INSTANCE_START_TIMEOUT`; see
[Operations: health checks](operations.md#health-checks).

### The service listens on another port than the one set, or the probe does not start

A Kubernetes Service named `docx-to-pdf` (or `docx-to-pdf-probe`) in the same namespace makes Kubernetes set
`DOCX_TO_PDF_PORT` (or `DOCX_TO_PDF_PROBE_PORT`) to `tcp://<ip>:<port>` in every pod. The service ignores a value of
that form with a warning at start, as though the variable were unset. Set the port explicitly and set
`enableServiceLinks: false` on the pod; see [How values are read](configuration.md#how-values-are-read).

### The client gives up before the answer

A client whose read timeout is shorter than the time a request can legitimately take abandons requests the service
would still answer; the service then logs
`"message":"the client went away before the answer; the conversion is cancelled"` and counts
`docx_to_pdf_conversions_total{outcome="cancelled"}`. A request can wait up to
`DOCX_TO_PDF_POOL_WAIT_TIMEOUT` for an instance and then convert for up to `DOCX_TO_PDF_CONVERSION_TIMEOUT`: set the
client's read timeout, and that of every proxy on the way, above the sum (240 seconds with the defaults), or lower
the two settings.

### The pod is killed for memory (OOMKilled)

The service bounds each instance's memory with `DOCX_TO_PDF_INSTANCE_HARD_MEMORY`, so a container sized by the formula
256 MiB + pool size × (`DOCX_TO_PDF_INSTANCE_HARD_MEMORY` + 256 MiB) is not killed for a document's sake. When it is
killed anyway, one of these is true:

- the memory limit is below the formula, often because `DOCX_TO_PDF_POOL_SIZE` was raised without it;
- `DOCX_TO_PDF_INSTANCE_HARD_MEMORY` is `0`, which turns the bound off;
- `/tmp` is a tmpfs (`emptyDir` with `medium: Memory`): what is written there counts against the container's memory,
  so its size must be added to the limit, or `/tmp` moved to disk.

`docx_to_pdf_instance_memory_usage_bytes` shows each instance's memory and `process_memory_usage_bytes` the service
process's own. See [Operations: Kubernetes](operations.md#kubernetes) for the worked numbers.

### Instances are replaced often

Every replacement is counted in `docx_to_pdf_instance_starts_total` with its reason. `recycle` is routine: an instance
is replaced after `DOCX_TO_PDF_INSTANCE_MAX_CONVERSIONS` conversions, or when its memory stays above
`DOCX_TO_PDF_INSTANCE_MAX_MEMORY` after one (logged as
`"message":"the instance is above the memory ceiling; recycling it"`), and costs about a third of a second.
`memory_limit` and `timeout` follow a `413` or a `504` (above). `crash` means LibreOffice died; the request was retried,
and a `500` follows only when the retry dies too. A steady stream of `crash` on ordinary documents is worth reporting,
with the log lines of one occurrence.

### A picture in the PDF is smaller, blurred, or a grey placeholder

The answer then carries a header such as `Docx-To-Pdf-Pictures: downscaled=1, replaced=0`. A PNG or JPEG whose decoded
bitmap is larger than [`DOCX_TO_PDF_PICTURE_MAX_BITMAP`](configuration.md#pictures) (256 MiB by default, about 90
megapixels of RGB colour) is scaled down before conversion, because LibreOffice would otherwise drop it from the PDF.
The service logs
`"message":"a picture's decoded bitmap passes the threshold; downscaled before conversion"` with the picture's size
and the factor, and `docx_to_pdf_pictures_total{outcome="downscaled"}` counts them.

A picture that cannot be scaled down safely is replaced by a placeholder instead, logged as
`"message":"a picture's decoded bitmap passes the threshold; replaced by a placeholder before conversion"` with the
reason in its `why` field (an interlaced PNG, a damaged picture, a JPEG coding the service does not decode) and
counted as `docx_to_pdf_pictures_total{outcome="replaced"}`. Re-save such a picture in an ordinary format at a
reasonable size (a picture of 20000 × 20000 pixels is far beyond what a page prints) and put it back in the document.

Raising `DOCX_TO_PDF_PICTURE_MAX_BITMAP` keeps larger pictures as they are, but LibreOffice holds each one decoded
while it converts, so raise `DOCX_TO_PDF_INSTANCE_HARD_MEMORY` with it. Above about 1 GiB LibreOffice drops a picture
whatever the setting.

### A picture is missing from the PDF, its frame empty

The service checks pictures only in the PNG and JPEG files of a DOCX, DOCM, DOTX, ODT or OTT. A very large picture in
a DOC, an RTF or a flat ODT, or in another picture format, reaches LibreOffice as it is, and LibreOffice drops one
whose decoded bitmap is above about 1 GiB. Save the document as DOCX, or the picture smaller. When the picture check
itself fails, the document is converted unchanged; the service logs
`"message":"the pictures step panicked; the document is converted as it was received"` and counts
`docx_to_pdf_pictures_total{outcome="failed"}`.

### The PDF is larger than expected

By default every picture is embedded as it is in the document, a JPEG byte for byte, so a photograph placed small on
the page keeps its full resolution. Set [`DOCX_TO_PDF_PDF_MAX_IMAGE_DPI`](configuration.md#pictures) to `300` to reduce
pictures above 300 DPI as placed, as moalhaddar/docx-to-pdf did; it costs CPU on documents with pictures.
`docx_to_pdf_pdf_size_bytes` shows the sizes of the PDFs you answer.

### Text in the PDF is in another font, reflows, or shows empty boxes

The PDF uses the fonts the document names when they are available, and a substitute when they are not. The image
carries substitutes with the same widths for Calibri, Cambria, Arial, Times New Roman, Courier New and Arial Narrow,
so those keep their line and page breaks; any other missing font is replaced by one of different widths, and the text
reflows. Empty boxes, or a glyph from another script's font, mean no available font has the character.

To see which fonts a PDF uses, run `pdffonts` (from Poppler) on it; to see what a font name resolves to inside the
image:

```sh
docker run --rm --entrypoint fc-match quay.io/oxidestack/docx-to-pdf:1.1.0 'Segoe UI'
```

The fix is to give the service the font: mount the font files you hold a licence for at `/usr/share/fonts/custom`,
as described in [Operations: mounting your own](operations.md#mounting-your-own), and restart the service; fonts are
read when an instance starts.

### The first conversion is slower than the next ones

Each LibreOffice instance loads part of its code on its first conversion rather than at start, so the first document
an instance converts takes noticeably longer than the same document after it; a fresh instance pays it again, after
every replacement (`docx_to_pdf_instance_starts_total{reason="recycle"}`, every `DOCX_TO_PDF_INSTANCE_MAX_CONVERSIONS`
conversions by default). With fonts mounted, the first instance after a change also builds their font cache when it
starts. Measure with `docx_to_pdf_conversion_duration_seconds`, which is LibreOffice's own time, and leave the first
conversion of each instance out of a benchmark. If the first request after a deployment must be fast, send one small
document per instance once `/readyz` turns `200`.

### Every conversion is slow

A typical business report converts in about a second on one core. When everything is slower:

- **More instances than cores.** `DOCX_TO_PDF_POOL_SIZE` above the CPUs the container may use, or a CPU limit that
  throttles it, makes each conversion share a core. Keep the pool size equal to the CPU request.
- **Time spent waiting, not converting.** Compare the request time, `http_server_request_duration_seconds`, with
  `docx_to_pdf_conversion_duration_seconds` and `docx_to_pdf_pool_wait_duration_seconds`: a large gap is the queue
  (more capacity needed) or the upload and download (the network).
- **Pictures reduced in the PDF.** `DOCX_TO_PDF_PDF_MAX_IMAGE_DPI` makes LibreOffice re-encode pictures, which costs
  CPU; unset, pictures are copied as they are.

To know what a machine can do, measure it with the platform probe; see
[Operations: sizing](operations.md#sizing).
