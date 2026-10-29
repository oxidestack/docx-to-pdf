# Operations

How to run the service in a container or on Kubernetes, watch it, bound what a document may cost, size a
deployment and give it fonts. Every setting named here is an environment variable described in
[Configuration](configuration.md); this page says how they fit together. A test holds the page to the code: the
metric names below are the ones the service registers, and the Kubernetes example sets only variables the service
reads, to values it accepts.

## The container

The image runs one process, the service, under `tini`, so `SIGTERM` reaches it and nothing is left behind when a
LibreOffice process is killed. It listens on port `8080` and is built for `linux/amd64` and `linux/arm64`.

It is made to run locked down, and is tested that way:

- **as user `10001`**, group `10001`, which has no entry in `/etc/passwd`; any other non-root user works too;
- **with a read-only root file system.** The only directory the service writes is `/tmp` (the image sets `TMPDIR`,
  `HOME` and `XDG_CACHE_HOME` under it): each instance's LibreOffice profile, the uploaded document, the PDF on its
  way out, pictures being scaled down, and the font cache for fonts you mount. Mount a writable volume there;
- **with every Linux capability dropped** and no privilege escalation. No file in the image keeps a setuid or setgid
  bit, so there is nothing to escalate to;
- **without the network beyond its own port**: it calls no other service, except an OpenTelemetry collector when you
  set one. The Helm chart's NetworkPolicy holds the pod to that
  ([Installation](installation.md#network-the-service-fetches-nothing)).

```sh
docker run --rm -p 8080:8080 \
  --user 10001:10001 --read-only --cap-drop ALL --security-opt no-new-privileges \
  --mount type=volume,dst=/tmp \
  quay.io/oxidestack/docx-to-pdf:1.1.0
```

### `/tmp`: on disk or in memory

`/tmp` on disk (a volume, or an `emptyDir` on Kubernetes) is the recommended choice: it costs no memory. A tmpfs
(`--tmpfs /tmp:rw,mode=1777,size=512m`, or an `emptyDir` with `medium: Memory`) keeps conversions off the disk, but
everything written there counts against the container's memory limit, so add its size to the limit.

How much a conversion writes there: up to three times the document's size at once (the document, its PDF and
LibreOffice's own temporary files), or about 256 MiB for a long layout, a small document of many pages whose PDF
grows large. So give `/tmp` at least **pool size × the larger of 3 × `DOCX_TO_PDF_MAX_DOCUMENT_SIZE` and 256 MiB**,
plus well under a megabyte per instance for its profile.

## Health checks

| Path | Answers | Use it as |
| --- | --- | --- |
| `GET /livez` | `200 ok` as soon as the service listens, whatever its instances are doing. | The liveness check. It fails only if the process stops answering HTTP at all. |
| `GET /readyz` | `200 ok` while at least one LibreOffice instance can take a conversion; `503 not ready` before the first instance is up, while none can, while the sizing probe runs (below), and from the moment shutdown begins. | The readiness check, and the startup check. |

The service listens before its instances have started, so `/livez` answers at once and `/readyz` turns `200` once the
first instance is ready, normally within a second or two. An instance that cannot start is tried again; at start-up,
three failures in a row stop the service with an error. A startup check should therefore allow more than three times
`DOCX_TO_PDF_INSTANCE_START_TIMEOUT` (`30s`) plus a few seconds: the example below allows 120 seconds.

The image carries no HTTP client, so it has no Docker `HEALTHCHECK`; let the orchestrator call the two paths.

## Stopping

On `SIGTERM` (or `SIGINT`) the service:

1. turns `/readyz` to `503` and stops accepting new connections;
2. lets the requests already in flight finish, for at most `DOCX_TO_PDF_SHUTDOWN_GRACE` (by default the conversion
   timeout, `180s`, so a conversion that started just before the signal can run to its own limit). A request still
   running when the grace ends gets `503`; a PDF still being sent is cut;
3. stops its LibreOffice instances, up to 5 seconds;
4. flushes the telemetry it exports, bounded by the exporter's timeout, and exits with code 0.

Give the runtime longer than that before it kills the process: the grace plus about 40 seconds. On Kubernetes, also
let the pod sleep a few seconds before the signal (`preStop`), so the Service and any load balancer stop sending it
requests before it stops taking them: `terminationGracePeriodSeconds` is then that sleep + the grace + 40.

## Kubernetes

A complete example: two conversions in parallel per pod, the security context above, `/tmp` on disk, the three
checks, a graceful stop and OTLP export. The project's Helm chart, `charts/docx-to-pdf`, renders the same thing from
a handful of values and computes the resources for you. [Installation](installation.md#kubernetes) installs it.

```yaml
apiVersion: apps/v1
kind: Deployment
metadata:
  name: docx-to-pdf
spec:
  replicas: 2
  selector:
    matchLabels:
      app: docx-to-pdf
  template:
    metadata:
      labels:
        app: docx-to-pdf
    spec:
      automountServiceAccountToken: false
      enableServiceLinks: false
      securityContext:
        runAsNonRoot: true
        runAsUser: 10001
        runAsGroup: 10001
        fsGroup: 10001
        seccompProfile:
          type: RuntimeDefault
      terminationGracePeriodSeconds: 225
      containers:
        - name: docx-to-pdf
          image: quay.io/oxidestack/docx-to-pdf:1.1.0
          securityContext:
            allowPrivilegeEscalation: false
            readOnlyRootFilesystem: true
            capabilities:
              drop: [ALL]
          ports:
            - name: http
              containerPort: 8080
          env:
            - name: DOCX_TO_PDF_PORT
              value: "8080"
            - name: DOCX_TO_PDF_POOL_SIZE
              value: "2"
            - name: DOCX_TO_PDF_INSTANCE_HARD_MEMORY
              value: "2GiB"
            - name: DOCX_TO_PDF_SHUTDOWN_GRACE
              value: "180s"
            - name: OTEL_EXPORTER_OTLP_ENDPOINT
              value: "http://otel-collector.observability:4318"
            - name: OTEL_RESOURCE_ATTRIBUTES
              value: "deployment.environment.name=prod"
          startupProbe:
            httpGet: {path: /readyz, port: http}
            periodSeconds: 2
            failureThreshold: 60
          livenessProbe:
            httpGet: {path: /livez, port: http}
            periodSeconds: 10
            timeoutSeconds: 3
            failureThreshold: 3
          readinessProbe:
            httpGet: {path: /readyz, port: http}
            periodSeconds: 5
            timeoutSeconds: 3
            failureThreshold: 2
          lifecycle:
            preStop:
              sleep:
                seconds: 5
          resources:
            requests:
              cpu: "2"
              memory: 4864Mi
            limits:
              memory: 4864Mi
          volumeMounts:
            - name: tmp
              mountPath: /tmp
      volumes:
        - name: tmp
          emptyDir:
            sizeLimit: 512Mi
---
apiVersion: v1
kind: Service
metadata:
  name: docx-to-pdf
spec:
  selector:
    app: docx-to-pdf
  ports:
    - name: http
      port: 8080
      targetPort: http
```

Why each number is what it is:

- **`DOCX_TO_PDF_POOL_SIZE` = the CPU request.** One conversion keeps one core busy; more instances than cores add no
  throughput and make each conversion slower.
- **Memory: 256 MiB + pool size × (`DOCX_TO_PDF_INSTANCE_HARD_MEMORY` + 256 MiB)**, here 256 + 2 × 2304 = 4864 MiB. The
  service kills an instance whose memory passes the hard limit while it converts (that request gets `413`), checking
  every 100 ms; 256 MiB per instance covers the growth between two checks, and 256 MiB covers the service itself.
  With this limit no document can get the pod killed for memory. Request as much as the limit, so the scheduler
  places the pod where that memory exists. A tmpfs `/tmp` adds its size here.
- **`/tmp`: 2 × max(3 × 10 MiB, 256 MiB) = 512 MiB** with the default 10 MiB document limit.
- **`terminationGracePeriodSeconds`: 5 + 180 + 40 = 225**, the `preStop` sleep, the shutdown grace and the time the
  service needs after it.
- **`enableServiceLinks: false`.** A Service named `docx-to-pdf` makes Kubernetes set `DOCX_TO_PDF_PORT` to
  `tcp://<ip>:8080` in every pod of the namespace. The service ignores a value of that form with a warning, but
  turning service links off, and setting the port explicitly, keeps the setting yours.
- **No service account token**: the service never calls the Kubernetes API.

A `PodDisruptionBudget` with `maxUnavailable: 1` lets a node drain one pod at a time.

## Telemetry

### Logs

One JSON object per line on standard output (human-readable text when standard output is a terminal), filtered by
`RUST_LOG`, `info` by default. Each line inside a request carries its `trace_id` and `span_id`, so a log line leads to
its trace. A setting the service ignores, such as a Kubernetes service link, is a `warn` line at start.

### Traces

When an OTLP endpoint is set (`OTEL_EXPORTER_OTLP_ENDPOINT`, see [Configuration](configuration.md#logs-and-telemetry)),
every request is a trace. Its root span is named after the method and the route, such as `POST /pdf`; under it,
`document.sniff` (recognising the format), `pool.acquire` (waiting for an instance), and `backend.load` and
`backend.export` (LibreOffice loading the document and writing the PDF). Starting an instance is its own span,
`instance.start`. A W3C `traceparent` header on the request makes the trace a child of the caller's.

An OTLP endpoint is ASCII: write an internationalised hostname in punycode (`http://xn--bcher-kva.example:4318`, not
`http://bücher.example:4318`; `idn2 bücher.example` prints it) and percent-encode the path. An endpoint with any
other character exports nothing for its signals and says so in a `warn` line at start, naming the variable.

### Metrics

Metrics are served in the Prometheus text format at `GET /metrics` on the service's port, always, and are exported
over OTLP too when an endpoint is set. The names follow the OpenTelemetry conventions; Prometheus spells them with
underscores, the unit as a suffix and `_total` on counters, and an attribute such as `http.route` becomes the label
`http_route`. A `target_info` series carries `service.name`, `service.version` and the version of
moalhaddar/docx-to-pdf the service is compatible with (`upstream.version`).

| OpenTelemetry name | Prometheus name | Kind | Attributes | What it tells you |
| --- | --- | --- | --- | --- |
| `http.server.request.duration` | `http_server_request_duration_seconds` | histogram, seconds | `http.request.method`, `http.route`, `http.response.status_code`, `error.type` | How long each request took, from its arrival to the last byte of the answer. `http.route` is the path pattern (`/pdf`, `/readyz`), never the raw path. |
| `http.server.active_requests` | `http_server_active_requests` | gauge | `http.request.method`, `http.route` | Requests in flight now. |
| `docx_to_pdf.document.size` | `docx_to_pdf_document_size_bytes` | histogram, bytes | `format` | The size of each document received, by the format it was recognised as. |
| `docx_to_pdf.conversions` | `docx_to_pdf_conversions_total` | counter | `outcome`, `retried` | Conversions, by how they ended: `success`, `unsupported` (415), `not_loaded` (422), `timeout` (504), `memory_limit` (413), `dead_instance` (500), `cancelled` (the client left) or `error` (500). `retried` is `true` when the first instance died and the document was converted again on another. |
| `docx_to_pdf.conversion.duration` | `docx_to_pdf_conversion_duration_seconds` | histogram, seconds | `outcome` | LibreOffice's own time for a conversion, loading plus export, without the upload, the queue or the download. |
| `docx_to_pdf.pdf.size` | `docx_to_pdf_pdf_size_bytes` | histogram, bytes | — | The size of each PDF answered. |
| `docx_to_pdf.pictures` | `docx_to_pdf_pictures_total` | counter | `outcome` | Pictures changed before conversion because they were too large to decode safely: `downscaled`, `replaced` by a placeholder, or `failed` (the document was converted as it was). |
| `docx_to_pdf.pool.size` | `docx_to_pdf_pool_size` | gauge | — | The configured number of LibreOffice instances. |
| `docx_to_pdf.pool.instances` | `docx_to_pdf_pool_instances` | gauge | `state` | Instances now `idle`, `busy` or `starting`. `idle` at 0 for long means requests are queueing: see the wait below. |
| `docx_to_pdf.pool.wait.duration` | `docx_to_pdf_pool_wait_duration_seconds` | histogram, seconds | `outcome` | How long a request waited for a free instance; `acquired`, or `rejected` with a 503. Waits growing toward `DOCX_TO_PDF_POOL_WAIT_TIMEOUT` mean the pod needs more instances, or more pods are needed. |
| `docx_to_pdf.instance.starts` | `docx_to_pdf_instance_starts_total` | counter | `reason` | Instance starts, by why: `initial`, `recycle` (after `DOCX_TO_PDF_INSTANCE_MAX_CONVERSIONS` or above `DOCX_TO_PDF_INSTANCE_MAX_MEMORY`), `crash`, `timeout`, `memory_limit`, `cancelled` (its conversion was abandoned) or `failed` (the previous start did not succeed). Anything but `initial` and `recycle` rising steadily is worth a look. |
| `docx_to_pdf.instance.start.duration` | `docx_to_pdf_instance_start_duration_seconds` | histogram, seconds | — | From starting an instance to its being ready. |
| `docx_to_pdf.instance.memory.usage` | `docx_to_pdf_instance_memory_usage_bytes` | gauge, bytes | `instance` | Each instance's resident memory now, by its slot in the pool. |
| `docx_to_pdf.instance.memory.peak` | `docx_to_pdf_instance_memory_peak_bytes` | histogram, bytes | — | The highest resident memory of the instance during each conversion. Compare it with `DOCX_TO_PDF_INSTANCE_HARD_MEMORY` to see how close your documents come to the limit. |
| `process.cpu.time` | `process_cpu_time_seconds_total` | counter, seconds | `cpu.mode` | CPU the service process itself has used, `user` and `system`; the instances are separate processes and not counted here. |
| `process.memory.usage` | `process_memory_usage_bytes` | gauge, bytes | — | The service process's own resident memory. |
| `process.unix.file_descriptor.count` | `process_unix_file_descriptor_count` | gauge | — | Files and sockets the service process holds open. |

To scrape `/metrics` with the Prometheus operator, point a `ServiceMonitor` at the Service's `http` port and the path
`/metrics`; the Helm chart has one behind `serviceMonitor.enabled`.

## Heavy documents: what is refused and what is bounded

A converter that accepts documents from others must survive any of them. The service bounds every cost a document
can have, and each bound ends in a definite answer rather than a stuck or dead pod. All answers that are not a PDF
are `application/problem+json` with a fixed `detail` sentence.

| A document that… | Is stopped by | The client gets |
| --- | --- | --- |
| is larger than allowed | `DOCX_TO_PDF_MAX_DOCUMENT_SIZE` (10 MiB): checked while the body arrives, and against `Content-Length` before any of it is read | `413`, without the service reading the rest of the upload |
| is a small archive that expands enormously (a "zip bomb") | `DOCX_TO_PDF_MAX_INFLATED_SIZE` (256 MiB): the DOCX or ODT is actually decompressed and counted before conversion | `413` |
| holds a picture too large to decode in memory | `DOCX_TO_PDF_PICTURE_MAX_BITMAP` (256 MiB decoded): the picture is scaled down, or replaced by a placeholder | a PDF, with a `Docx-To-Pdf-Pictures` header saying how many pictures were changed |
| needs more memory to lay out than allowed | `DOCX_TO_PDF_INSTANCE_HARD_MEMORY` (2 GiB per instance): the instance is killed and replaced | `413` |
| takes too long to convert | `DOCX_TO_PDF_CONVERSION_TIMEOUT` (180 s): the instance is killed and replaced | `504` |
| is not a format the service converts | the format is recognised from the content, not the name | `415` |
| is in a supported format but LibreOffice cannot open it | — | `422` |
| crashes LibreOffice | the document is tried once more on another instance | a PDF if the second try succeeds, else `500` |
| arrives too slowly, or stops arriving | `DOCX_TO_PDF_IDLE_TIMEOUT` (120 s without a byte) | `408` |

And when there are more requests than instances:

| When… | Bounded by | The client gets |
| --- | --- | --- |
| every instance is busy | requests wait in a queue, first come first served | the PDF, later |
| the queue is full | `DOCX_TO_PDF_POOL_MAX_WAITING` (16) | `503` at once, with `Retry-After: 10` |
| a request waited too long | `DOCX_TO_PDF_POOL_WAIT_TIMEOUT` (60 s) | `503`, with `Retry-After: 10` |

A client that retries a `503` after `Retry-After` and sets its read timeout above the wait timeout plus the conversion
timeout (240 seconds with the defaults) never gives up on a request the service would still answer.

An instance is also replaced, between conversions and without failing anything, after
`DOCX_TO_PDF_INSTANCE_MAX_CONVERSIONS` conversions (200) or when its memory stays above
`DOCX_TO_PDF_INSTANCE_MAX_MEMORY` (1 GiB) after one, so a document that leaves LibreOffice large does not make every
later conversion pay for it. A replacement takes about a third of a second.

After each conversion, before it answers, an instance hands the memory LibreOffice freed back to the system
(`malloc_trim`), so its resident memory between conversions is its working set, not what the largest recent document
left behind. On a soak of typical reports this keeps one instance's container about 50 MiB lower, for a few
milliseconds a conversion; it does not lower the peak during a conversion, which the memory limits below are sized
from. There is no setting for it.

## Sizing

### A starting point

| Resource | Formula | With the defaults |
| --- | --- | --- |
| CPU | the pool size, one core per instance | 1 |
| Memory (request = limit) | 256 MiB + pool size × (`DOCX_TO_PDF_INSTANCE_HARD_MEMORY` + 256 MiB), plus a tmpfs `/tmp` | 2560 MiB for one instance |
| `/tmp` | pool size × max(3 × `DOCX_TO_PDF_MAX_DOCUMENT_SIZE`, 256 MiB) | 256 MiB for one instance |

A typical business report converts in about a second on one core, so one instance handles roughly 60 such documents
a minute. Documents differ by orders of magnitude, though, and so do machines: measure yours.

### Measuring a platform: the sizing probe

The service can measure how fast the machine it runs on converts, under the limits it actually has. Set
`DOCX_TO_PDF_PROBE_PORT` (for instance to `9090`) and the service listens there for one endpoint, `POST /probe`.
Keep that port private: never expose it through a Service. While a probe runs it holds every instance, so
conversions get `503` and `/readyz` is not ready, and Kubernetes routes traffic to the other pods. Probe one pod
that takes no traffic, or one you can spare for a minute or two.

```sh
kubectl port-forward pod/docx-to-pdf-7c9d8-abcde 9090:9090
curl -F document=@report-1.docx -F document=@report-2.docx \
  'http://localhost:9090/probe?cpus=2' -o platform.json
```

Send documents that look like your traffic: up to 16, each up to 64 MiB (or `DOCX_TO_PDF_MAX_DOCUMENT_SIZE`, if that
is larger). The probe converts them, server-side, without the network in the way:

1. once on each instance, to warm up;
2. one at a time, `rounds` times over (default 5): the mean is **`s1`**, a conversion's time with nothing else
   running;
3. on every instance at once, `busy` conversions in all (default 120 + 3 × pool size): how much slower a conversion
   runs with every core converting, as a ratio to `s1`, is the **contention**;
4. 1 GiB of direct writes under `/tmp` (`disk_mib`, `0` to skip; skipped on a tmpfs): the disk's sustained **MB/s**.

Then it gives the instances back and answers with JSON: those three numbers, and every sizing formula worked out for
this pod (`cpus`, default the pool size): the pool size, the throughput of a pod in conversions per minute, a
conversion's time alone and with every instance busy, the memory limit, the `/tmp` size, timeouts that fit, the
response time at a given request rate, and the number of pods for a rate and a response-time target. Each formula
comes with the reason for it. The optional `name` parameter labels the platform in the answer.

A second probe while one runs gets `503` ("a probe is already running"), as does one sent while the service is
starting or stopping.

### Turning the measurement into a plan: `bench.py size`

The answer is also the input of the sizing calculator in the repository, `tools/bench/bench.py size`, which needs
only Python's standard library. Ask it what a given machine can do, or what it takes to meet a target:

```sh
# What 4 vCPU and 8 GiB give.
python3 tools/bench/bench.py size hardware --platform platform.json --cpus 4 --memory 8

# Pods for 60 requests a minute with 95 % of them answered within 5 seconds.
python3 tools/bench/bench.py size target --platform platform.json --latency 5 --quantile 0.95 --rate 60/min
```

`size hardware` answers with the pool the machine allows (by its cores and by its memory), the throughput at
saturation, a conversion's mean and tail time, the memory limit, the `/tmp` size, and the settings to start the
service with, such as:

```text
4 vCPU, 8 GiB, disk 219.3 MB/s:
  pool 3 (CPU allows 4, memory 3; bound by memory)
  throughput 1.78/s = 107/min at saturation
  a conversion at saturation: mean 1.7 s, p95 2.2 s, p99 2.4 s
  memory limit 7.00 GiB (request = limit, worst case)
  /tmp 866 MiB (worst case), on disk
  DOCX_TO_PDF_POOL_SIZE=3
  ...
```

`size target` lists the shapes of pod (vCPU, pool, memory) that meet the target and how many of each.

Without `--platform` the calculator uses a reference platform measured by the project; `--s1` and `--contention`
override the platform's numbers by hand.

## Fonts

A PDF can only look like the document if the fonts the document names are available; where one is missing,
LibreOffice substitutes another, and the text reflows. The image carries only fonts whose licences allow it to be
shared:

- **Carlito** and **Caladea**, drawn to the same widths as Calibri and Cambria, and **Liberation** Sans, Serif, Mono
  and Sans Narrow, with the widths of Arial, Times New Roman, Courier New and Arial Narrow: a document set in those
  Microsoft fonts keeps its line and page breaks;
- **DejaVu**, **Linux Libertine**, **Gentium** and **OpenSymbol** (LibreOffice's symbol font);
- **Noto**: the core and extra sets, Mono, UI, Chinese, Japanese and Korean (CJK), and colour emoji, so text in
  most of the world's scripts renders rather than falling back to empty boxes;
- the three files moalhaddar/docx-to-pdf shipped, **Droid Kufi** (regular and bold) and **Cantarell** Extra Bold, so
  documents made for that service look the same.

### Mounting your own

The directory `/usr/share/fonts/custom` is empty in the image and is searched for fonts. Mount the fonts you hold a
licence for there, read-only, for example Microsoft's own fonts where the substitutes above are not close enough:

```sh
docker run --rm -p 8080:8080 --user 10001:10001 --read-only --cap-drop ALL \
  --mount type=volume,dst=/tmp \
  -v "$PWD/fonts:/usr/share/fonts/custom:ro" \
  quay.io/oxidestack/docx-to-pdf:1.1.0
```

On Kubernetes, mount a Secret, a ConfigMap or any volume at the same path (the Helm chart's `fonts` value does it).
Fonts are read when an instance starts: restart the pod after changing them. The first instance after a change
writes the font cache for the mounted directory under `/tmp`, which takes a moment at start-up and nothing after.
