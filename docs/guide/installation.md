# Installation

How to run the service: with `docker run` to try it, with Docker Compose on one machine, and on Kubernetes with the
Helm chart. Each way ends with the service answering `POST /pdf` on port `8080`; [API reference](api.md) says what
to send it. A test holds this page to the files it shows: the compose files below are the ones in
[`examples/compose/`](../../examples/compose/), a test starts each of them and converts a document through it, and
the `helm` commands install the chart the way the chart's own test does.

> **Keep the service off the public internet.** It has no authentication and no rate limit: anyone who can reach
> its port can make it convert whatever they send, and every conversion costs a CPU core and up to a few GiB of
> memory for as long as it lasts. Run it on a private network, where only your own applications call it, and put
> anything that faces users in front of those applications, not in front of the service.

## Docker

```sh
docker run --rm -p 8080:8080 quay.io/oxidestack/docx-to-pdf:0.1.0
```

The image and the chart are published to quay.io. `ghcr.io/oxidestack/docx-to-pdf` and
`oci://ghcr.io/oxidestack/charts/docx-to-pdf` mirror them with the same digests and signatures, for a network that
cannot reach quay.io.

`-p 8080:8080` publishes the port on every network interface of the machine, so other machines on its network can
call it too. To keep it to the machine itself, bind the port to `127.0.0.1`:

```sh
docker run --rm -p 127.0.0.1:8080:8080 quay.io/oxidestack/docx-to-pdf:0.1.0
```

Once it has started (`/readyz` answers `200`, normally within a few seconds), convert a document:

```sh
curl -F document=@report.docx http://localhost:8080/pdf -o report.pdf
```

The image is built for `linux/amd64` and `linux/arm64`, so it runs natively on an Apple Silicon Mac as on an x86
server. It is made to run locked down — a non-root user, a read-only root file system, no Linux capabilities — and
[Operations](operations.md#the-container) gives the `docker run` line that does so.

## Docker Compose

`examples/compose/compose.yaml` runs the service locked down, on `127.0.0.1:8080`, with the memory limit that one
conversion at a time needs:

```yaml
# docx-to-pdf on this machine only: http://127.0.0.1:8080 (docs/guide/installation.md).
services:
  docx-to-pdf:
    image: quay.io/oxidestack/docx-to-pdf:0.1.0
    ports:
      - "127.0.0.1:8080:8080"
    environment:
      DOCX_TO_PDF_POOL_SIZE: "1"
    # Locked down: a non-root user, a read-only root, no capability; the service writes only /tmp, a volume.
    user: "10001:10001"
    read_only: true
    cap_drop: [ALL]
    security_opt: ["no-new-privileges:true"]
    volumes:
      - /tmp
    # 256 MiB + pool size × (DOCX_TO_PDF_INSTANCE_HARD_MEMORY, 2 GiB by default, + 256 MiB): no document can get
    # the container killed for memory.
    mem_limit: 2560m
    # The shutdown grace (180 s by default) plus the 40 s the service needs after it.
    stop_grace_period: 220s
    restart: unless-stopped
```

```sh
cd examples/compose
docker compose up -d
```

Settings are environment variables, listed in [Configuration](configuration.md). To convert more documents at once,
raise `DOCX_TO_PDF_POOL_SIZE` and the memory limit with it ([Resources](#resources)). `docker compose down -v` stops the
service and removes its `/tmp` volume.

### With telemetry

The service sends traces, logs and metrics over OTLP to the collector `OTEL_EXPORTER_OTLP_ENDPOINT` names. Here an
OpenTelemetry Collector receives them and prints a line for each batch, `examples/compose/telemetry/compose.yaml`:

```yaml
# docx-to-pdf sending its traces, logs and metrics to an OpenTelemetry Collector (docs/guide/installation.md).
services:
  docx-to-pdf:
    image: quay.io/oxidestack/docx-to-pdf:0.1.0
    ports:
      - "127.0.0.1:8080:8080"
    environment:
      DOCX_TO_PDF_POOL_SIZE: "1"
      OTEL_EXPORTER_OTLP_ENDPOINT: http://otel-collector:4318
      OTEL_RESOURCE_ATTRIBUTES: deployment.environment.name=dev
    user: "10001:10001"
    read_only: true
    cap_drop: [ALL]
    security_opt: ["no-new-privileges:true"]
    volumes:
      - /tmp
    mem_limit: 2560m
    stop_grace_period: 220s
    restart: unless-stopped
    depends_on: [otel-collector]

  otel-collector:
    image: otel/opentelemetry-collector-contrib:0.162.0@sha256:39923a8e431bd1f57be82411999d389fcfe40857492e4365456d97a4c1f74be6
    command: ["--config=/etc/otelcol-contrib/config.yaml"]
    volumes:
      - ./otel-collector.yaml:/etc/otelcol-contrib/config.yaml:ro
    restart: unless-stopped
```

and its configuration, `examples/compose/telemetry/otel-collector.yaml`:

```yaml
# Receives OTLP over HTTP on 4318 and prints a line per batch. Replace the debug exporter with your backend's.
receivers:
  otlp:
    protocols:
      http:
        endpoint: 0.0.0.0:4318

processors:
  batch: {}

exporters:
  debug:
    verbosity: basic

service:
  pipelines:
    traces:
      receivers: [otlp]
      processors: [batch]
      exporters: [debug]
    metrics:
      receivers: [otlp]
      processors: [batch]
      exporters: [debug]
    logs:
      receivers: [otlp]
      processors: [batch]
      exporters: [debug]
```

```sh
cd examples/compose/telemetry
docker compose up -d
docker compose logs -f otel-collector
```

Replace the `debug` exporter with your backend's, or point `OTEL_EXPORTER_OTLP_ENDPOINT` at a collector you already
run. Without any collector, `/metrics` still serves the metrics for Prometheus to scrape. What the service reports is
in [Operations](operations.md#telemetry).

## Kubernetes

### With Helm

The chart is published to `oci://quay.io/oxidestack/charts`. With its defaults it runs one pod that converts
one document at a time:

```sh
helm install docx-to-pdf oci://quay.io/oxidestack/charts/docx-to-pdf --version 0.1.0 \
  --namespace docx-to-pdf --create-namespace
```

The values a deployment usually changes, in a file of your own:

```yaml
# values.yaml
replicaCount: 2
# Conversions in parallel per pod: the pod's CPU request, and its memory limit follows.
poolSize: 2
otel:
  endpoint: http://otel-collector.observability:4318
  resourceAttributes: deployment.environment.name=prod
config:
  DOCX_TO_PDF_MAX_DOCUMENT_SIZE: 20MiB
# The NetworkPolicy's egress to the collector: its namespace only (Network, below).
networkPolicy:
  otlpTo: [{namespaceSelector: {matchLabels: {kubernetes.io/metadata.name: observability}}}]
```

```sh
helm upgrade --install docx-to-pdf oci://quay.io/oxidestack/charts/docx-to-pdf --version 0.1.0 \
  --namespace docx-to-pdf --create-namespace --values values.yaml
```

| Value | Default | What it sets |
| --- | --- | --- |
| `replicaCount` | `1` | Pods. |
| `poolSize` | `1` | `DOCX_TO_PDF_POOL_SIZE`, the conversions in parallel per pod. The CPU request is the same number, and the memory request and limit are computed from it ([Resources](#resources)). |
| `instanceHardMemoryMiB` | `2048` | `DOCX_TO_PDF_INSTANCE_HARD_MEMORY`: the memory one conversion may take before it is stopped with `413`. The memory limit is computed from it. |
| `otel.endpoint` | empty: no export | `OTEL_EXPORTER_OTLP_ENDPOINT`, the collector. |
| `config` | `{}` | Any other setting of [Configuration](configuration.md), as `NAME: value`. |
| `tmp.medium` | on disk | `Memory` puts `/tmp` in a tmpfs, whose size `tmp.sizeMiB` is added to the memory limit. |
| `fonts` | none | A volume of fonts you hold a licence for ([Operations](operations.md#mounting-your-own)). |
| `serviceMonitor.enabled` | `false` | A prometheus-operator ServiceMonitor for `/metrics`. |
| `networkPolicy.enabled` | `true` | A NetworkPolicy: ingress to port 8080, egress to DNS and the collector only ([Network](#network-the-service-fetches-nothing)). |

Every value is listed in the chart's README, and the chart refuses a value it does not know. The release's Service is
`docx-to-pdf.docx-to-pdf.svc.cluster.local:8080`, a `ClusterIP`: reachable from inside the cluster only. `helm test
docx-to-pdf --namespace docx-to-pdf` converts a small document through it.

### Without Helm

To deploy with plain manifests — `kubectl apply`, Kustomize, or a GitOps tool that applies files — render the chart
once with the same values and keep the output:

```sh
helm template docx-to-pdf oci://quay.io/oxidestack/charts/docx-to-pdf --version 0.1.0 \
  --namespace docx-to-pdf --values values.yaml --skip-tests > docx-to-pdf.yaml
kubectl create namespace docx-to-pdf
kubectl apply --namespace docx-to-pdf -f docx-to-pdf.yaml
```

The output is a Deployment, a Service, a PodDisruptionBudget and a NetworkPolicy. [Operations](operations.md#kubernetes)
has the same Deployment written out by hand, with the reason for every number in it.

### Network: the service fetches nothing

A document can name content outside itself: a picture linked to a URL or to a file, a remote template, an OLE link.
The service never fetches or reads any of it. A linked picture converts to the copy embedded in the document, or to
nothing, so a conversion opens no connection and waits on no host, whatever the document says. The pod needs no
egress except DNS and the telemetry collector, and the chart's NetworkPolicy, on by default, holds it to that, so that
nothing running in the converter can reach the rest of the cluster. It selects the release's pods and allows:

- ingress on the service port, 8080, only, from anywhere, or from the peers `networkPolicy.ingressFrom` names;
- egress to DNS, UDP and TCP 53, at the cluster DNS pods (`k8s-app: kube-dns` in `kube-system`) or the peers
  `networkPolicy.dnsTo` names;
- egress to the port of `otel.endpoint` (`4318` above), at the peers `networkPolicy.otlpTo` names, or anywhere when it
  names none; with no endpoint, no rule at all.

The values above hold the collector rule to the collector's namespace, `observability`, with `networkPolicy.otlpTo`.

| Value | Default | What it sets |
| --- | --- | --- |
| `networkPolicy.enabled` | `true` | The NetworkPolicy. `false` leaves the pod's network to a policy of your own. |
| `networkPolicy.ingressFrom` | `[]`: any | Who may connect to port 8080, as NetworkPolicy peers. |
| `networkPolicy.dnsTo` | the `kube-dns` pods | Where DNS is. A node-local DNS cache needs its address here, e.g. `[{ipBlock: {cidr: 169.254.20.10/32}}]`. |
| `networkPolicy.otlpTo` | `[]`: any address on the endpoint's port | Where the collector is. |

The policy takes effect only where the cluster's network plugin enforces NetworkPolicy (Calico, Cilium, kind's
kindnet and most managed clusters do); elsewhere it is accepted and does nothing.

## Resources

Each conversion keeps one CPU core busy and LibreOffice's memory grows with the document, so a pod is sized from its
pool size, the number of conversions it runs at once:

| Resource | Per pod | With the defaults (pool size 1) | With the values above (pool size 2) |
| --- | --- | --- | --- |
| CPU | the pool size | 1 core | 2 cores |
| Memory, request = limit | 256 MiB + pool size × (`DOCX_TO_PDF_INSTANCE_HARD_MEMORY` + 256 MiB) | 2560 MiB | 4864 MiB |
| `/tmp` | pool size × the larger of 3 × `DOCX_TO_PDF_MAX_DOCUMENT_SIZE` and 256 MiB | 256 MiB | 512 MiB |

With that memory limit no document can get the container killed for memory: one that would take more is stopped
and answered `413`. The Helm chart computes the CPU request and the memory from `poolSize`; the compose files set
`mem_limit` to the same formula. A typical business report converts in about a second on one core; to measure what
your documents take on your machines, and how many pods a load needs, see [Operations](operations.md#sizing).

## A classic Helm repository

For a tool that installs charts from a repository URL rather than from an OCI registry, every GitHub release carries
the index of the charts released so far, each version downloading from its own release; the URL below always serves
the latest release's:

```sh
helm repo add docx-to-pdf https://github.com/oxidestack/docx-to-pdf/releases/latest/download
helm install docx-to-pdf docx-to-pdf/docx-to-pdf --version 0.1.0 --namespace docx-to-pdf --create-namespace
```

The chart is listed on [Artifact Hub](https://artifacthub.io/) from its quay.io repository. A pre-release (`0.2.0-rc.1`)
is in the index too; `helm search repo --devel` shows it, and nothing installs it unless its version is asked for.

## Verifying a release

Every release is built and published by the repository's release workflow from its tag, and that workflow signs what
it publishes. The image and the chart carry a [cosign](https://docs.sigstore.dev/cosign/) signature with no key to
distribute: the certificate names the workflow and the tag that made it, and the signature is recorded in Sigstore's
public transparency log. To check that an image is the one the release made:

```sh
cosign verify quay.io/oxidestack/docx-to-pdf:0.1.0 \
  --certificate-identity https://github.com/oxidestack/docx-to-pdf/.github/workflows/release.yml@refs/tags/v0.1.0 \
  --certificate-oidc-issuer https://token.actions.githubusercontent.com
cosign verify quay.io/oxidestack/charts/docx-to-pdf:0.1.0 \
  --certificate-identity https://github.com/oxidestack/docx-to-pdf/.github/workflows/release.yml@refs/tags/v0.1.0 \
  --certificate-oidc-issuer https://token.actions.githubusercontent.com
```

The image also carries attestations: its [SLSA build provenance](https://slsa.dev/spec/v1.0/provenance) (which
commit, which workflow run) on the multi-architecture image, and an SPDX SBOM, the Debian packages and the crates it
contains, on each architecture's image. The [GitHub CLI](https://cli.github.com/) checks them:

```sh
gh attestation verify oci://quay.io/oxidestack/docx-to-pdf:0.1.0 --repo oxidestack/docx-to-pdf
gh attestation verify oci://quay.io/oxidestack/charts/docx-to-pdf:0.1.0 --repo oxidestack/docx-to-pdf
```

For an SBOM, name one architecture's image by its digest (`docker buildx imagetools inspect` lists them) and its
predicate type, `--predicate-type https://spdx.dev/Document/v2.3`. The SBOMs are also attached to the GitHub release
as files, in SPDX and CycloneDX, one per architecture, with the chart and the compose examples.

The mirror on ghcr.io carries the same digests with the same signatures and attestations, copied with the image, so
the same commands verify `ghcr.io/oxidestack/docx-to-pdf:0.1.0` and `ghcr.io/oxidestack/charts/docx-to-pdf:0.1.0`
(add `--bundle-from-oci` to `gh attestation verify` to read the attestations from the registry itself).
