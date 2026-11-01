# 安装

<!-- languages:begin -->
[English](../../../guide/installation.md) · [Русский](../../ru/guide/installation.md) · **中文** ·
[Español](../../es/guide/installation.md)
<!-- languages:end -->
<!-- translation of docs/guide/installation.md: fb96eac62937 -->

如何运行本服务：用 `docker run` 试用，在单台机器上用 Docker Compose 运行，以及用 Helm chart 在
Kubernetes 上运行。无论哪种方式，最终都是服务在端口 `8080` 上响应 `POST /pdf`；要发送什么，见
[API 参考](api.md)。有一个测试保证本页与其中展示的文件一致：下面的 compose 文件就是
[`examples/compose/`](../../../../examples/compose/) 中的文件，测试会逐一启动它们并通过它们转换一份文档；
`helm` 命令安装 chart 的方式与 chart 自己的测试相同。

> **不要让服务暴露在公网上。** 它没有身份认证，也没有速率限制：任何能访问其端口的人都能让它转换任何发来的内容，
> 而每次转换在其持续期间都要占用一个 CPU 核心和最多数 GiB 内存。请在私有网络中运行它，只让你自己的应用调用它；
> 面向用户的一切都放在这些应用之前，而不是放在服务之前。

## Docker

```sh
docker run --rm -p 8080:8080 quay.io/oxidestack/docx-to-pdf:1.2.1
```

镜像和 chart 发布在 quay.io。`ghcr.io/oxidestack/docx-to-pdf` 和
`oci://ghcr.io/oxidestack/charts/docx-to-pdf` 是它们的镜像站（mirror），摘要和签名相同，供无法访问 quay.io
的网络使用。

`-p 8080:8080` 会在机器的每个网络接口上发布该端口，因此同一网络中的其他机器也能调用它。
若只允许本机访问，请把端口绑定到 `127.0.0.1`：

```sh
docker run --rm -p 127.0.0.1:8080:8080 quay.io/oxidestack/docx-to-pdf:1.2.1
```

启动完成后（`/readyz` 返回 `200`，通常在几秒之内），转换一份文档：

```sh
curl -F document=@report.docx http://localhost:8080/pdf -o report.pdf
```

镜像为 `linux/amd64` 和 `linux/arm64` 构建，因此在 Apple Silicon Mac 上与在 x86 服务器上一样原生运行。
它被设计为以锁定方式运行——非 root 用户、只读根文件系统、没有 Linux capabilities——
[运维](operations.md#容器)给出了这样运行的 `docker run` 命令。

## Docker Compose

`examples/compose/compose.yaml` 以锁定方式在 `127.0.0.1:8080` 上运行服务，并设置一次一个转换所需的内存限制：

```yaml
# docx-to-pdf on this machine only: http://127.0.0.1:8080 (docs/guide/installation.md).
services:
  docx-to-pdf:
    image: quay.io/oxidestack/docx-to-pdf:1.2.1
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

设置项都是环境变量，列在[配置](configuration.md)中。要同时转换更多文档，请调高 `DOCX_TO_PDF_POOL_SIZE`，
并相应调高内存限制（[资源](#资源)）。`docker compose down -v` 会停止服务并删除其 `/tmp` 卷。

### 启用遥测

服务通过 OTLP 把 trace、日志和指标发送到 `OTEL_EXPORTER_OTLP_ENDPOINT` 指定的 collector。
这里由一个 OpenTelemetry Collector 接收它们，并为每个批次打印一行，`examples/compose/telemetry/compose.yaml`：

```yaml
# docx-to-pdf sending its traces, logs and metrics to an OpenTelemetry Collector (docs/guide/installation.md).
services:
  docx-to-pdf:
    image: quay.io/oxidestack/docx-to-pdf:1.2.1
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

以及它的配置，`examples/compose/telemetry/otel-collector.yaml`：

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

把 `debug` exporter 换成你的后端的 exporter，或者把 `OTEL_EXPORTER_OTLP_ENDPOINT` 指向你已经在运行的 collector。
没有任何 collector 时，`/metrics` 仍会提供指标供 Prometheus 抓取。服务报告哪些内容，见[运维](operations.md#遥测)。

## Kubernetes

### 使用 Helm

chart 发布在 `oci://quay.io/oxidestack/charts`。使用默认值时，它运行一个 pod，一次转换一份文档：

```sh
helm install docx-to-pdf oci://quay.io/oxidestack/charts/docx-to-pdf --version 1.2.1 \
  --namespace docx-to-pdf --create-namespace
```

部署中通常需要修改的 values，写在你自己的文件中：

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
helm upgrade --install docx-to-pdf oci://quay.io/oxidestack/charts/docx-to-pdf --version 1.2.1 \
  --namespace docx-to-pdf --create-namespace --values values.yaml
```

| 值 | 默认值 | 设置的内容 |
| --- | --- | --- |
| `replicaCount` | `1` | pod 数量。 |
| `poolSize` | `1` | `DOCX_TO_PDF_POOL_SIZE`，每个 pod 并行的转换数。CPU request 与之相同，内存 request 和 limit 由它计算得出（[资源](#资源)）。 |
| `instanceHardMemoryMiB` | `2048` | `DOCX_TO_PDF_INSTANCE_HARD_MEMORY`：一次转换在被以 `413` 终止之前可以占用的内存。内存 limit 由它计算得出。 |
| `otel.endpoint` | 空：不导出 | `OTEL_EXPORTER_OTLP_ENDPOINT`，即 collector。 |
| `config` | `{}` | [配置](configuration.md)中的任何其他设置，写作 `NAME: value`。 |
| `tmp.medium` | 磁盘 | `Memory` 把 `/tmp` 放在 tmpfs 中，其大小 `tmp.sizeMiB` 会加到内存 limit 上。 |
| `fonts` | 无 | 一个存放你持有许可证的字体的卷（[运维](operations.md#挂载你自己的字体)）。 |
| `serviceMonitor.enabled` | `false` | 一个针对 `/metrics` 的 prometheus-operator ServiceMonitor。 |
| `networkPolicy.enabled` | `true` | 一个 NetworkPolicy：入站只到端口 8080，出站只到 DNS 和 collector（[网络](#网络服务不获取任何内容)）。 |

每个值都列在 chart 的 README 中，chart 会拒绝它不认识的值。该 release 的 Service 是
`docx-to-pdf.docx-to-pdf.svc.cluster.local:8080`，类型为 `ClusterIP`：只能从集群内部访问。`helm test
docx-to-pdf --namespace docx-to-pdf` 会通过它转换一份小文档。

### 不使用 Helm

若要用普通清单部署——`kubectl apply`、Kustomize，或应用文件的 GitOps 工具——请用相同的 values
渲染一次 chart 并保存输出：

```sh
helm template docx-to-pdf oci://quay.io/oxidestack/charts/docx-to-pdf --version 1.2.1 \
  --namespace docx-to-pdf --values values.yaml --skip-tests > docx-to-pdf.yaml
kubectl create namespace docx-to-pdf
kubectl apply --namespace docx-to-pdf -f docx-to-pdf.yaml
```

输出包含一个 Deployment、一个 Service、一个 PodDisruptionBudget 和一个 NetworkPolicy。
[运维](operations.md#kubernetes)中有手写的同一个 Deployment，并说明了其中每个数字的理由。

### 网络：服务不获取任何内容

文档可以引用其自身之外的内容：链接到 URL 或文件的图片、远程模板、OLE 链接。服务从不获取或读取其中任何一项。
链接的图片会转换为文档中嵌入的副本，或者什么都不转换，因此无论文档写了什么，转换都不会打开任何连接，
也不会等待任何主机。pod 除了 DNS 和遥测 collector 之外不需要任何出站访问，而 chart 的 NetworkPolicy
（默认开启）把它限制在这个范围内，使转换器中运行的任何东西都无法访问集群的其余部分。
它选中该 release 的 pod，并允许：

- 入站只到服务端口 8080，来源不限，或者只来自 `networkPolicy.ingressFrom` 指定的对端；
- 出站到 DNS，UDP 和 TCP 53，目标是集群 DNS pod（`kube-system` 中的 `k8s-app: kube-dns`）或
  `networkPolicy.dnsTo` 指定的对端；
- 出站到 `otel.endpoint` 的端口（上例中为 `4318`），目标是 `networkPolicy.otlpTo` 指定的对端，未指定时不限目标；
  没有 endpoint 时，则完全没有这条规则。

上面的 values 用 `networkPolicy.otlpTo` 把 collector 规则限制在 collector 所在的命名空间 `observability`。

| 值 | 默认值 | 设置的内容 |
| --- | --- | --- |
| `networkPolicy.enabled` | `true` | NetworkPolicy。`false` 把 pod 的网络交给你自己的策略。 |
| `networkPolicy.ingressFrom` | `[]`：任意 | 谁可以连接端口 8080，以 NetworkPolicy 对端的形式给出。 |
| `networkPolicy.dnsTo` | `kube-dns` pod | DNS 所在之处。节点本地 DNS 缓存需要在这里填写它的地址，例如 `[{ipBlock: {cidr: 169.254.20.10/32}}]`。 |
| `networkPolicy.otlpTo` | `[]`：endpoint 端口上的任意地址 | collector 所在之处。 |

只有在集群的网络插件实施 NetworkPolicy 时（Calico、Cilium、kind 的 kindnet 以及大多数托管集群都会），
该策略才会生效；在其他地方，它会被接受但不起任何作用。

## 资源

每次转换让一个 CPU 核心保持繁忙，而 LibreOffice 的内存随文档增长，因此 pod 的规格由其池大小决定，
即它同时运行的转换数：

| 资源 | 每个 pod | 使用默认值（池大小 1） | 使用上面的 values（池大小 2） |
| --- | --- | --- | --- |
| CPU | 池大小 | 1 核 | 2 核 |
| 内存，request = limit | 256 MiB + 池大小 × (`DOCX_TO_PDF_INSTANCE_HARD_MEMORY` + 256 MiB) | 2560 MiB | 4864 MiB |
| `/tmp` | 池大小 × 3 × `DOCX_TO_PDF_MAX_DOCUMENT_SIZE` 与 256 MiB 中的较大者 | 256 MiB | 512 MiB |

有了这样的内存限制，任何文档都无法让容器因内存而被杀死：需要更多内存的文档会被终止，并以 `413` 响应。
Helm chart 根据 `poolSize` 计算 CPU request 和内存；compose 文件把 `mem_limit` 设为同一公式的结果。
一份典型的业务报告在一个核心上大约一秒即可转换完成；要测量你的文档在你的机器上需要多少资源，
以及某个负载需要多少个 pod，见[运维](operations.md#容量规划)。

## 传统 Helm 仓库

对于从仓库 URL 而不是从 OCI registry 安装 chart 的工具，每个 GitHub release 都附带迄今为止发布的所有 chart
的索引，每个版本都从它自己的 release 下载；下面的 URL 始终提供最新 release 的索引：

```sh
helm repo add docx-to-pdf https://github.com/oxidestack/docx-to-pdf/releases/latest/download
helm install docx-to-pdf docx-to-pdf/docx-to-pdf --version 1.2.1 --namespace docx-to-pdf --create-namespace
```

该 chart 以其 quay.io 仓库登记在 [Artifact Hub](https://artifacthub.io/) 上。预发布版本（`0.2.0-rc.1`）
也在索引中；`helm search repo --devel` 会显示它，除非明确要求该版本，否则任何操作都不会安装它。

## 验证版本

每个版本都由仓库的 release 工作流从其标签构建并发布，该工作流会对它发布的内容签名。
镜像和 chart 带有 [cosign](https://docs.sigstore.dev/cosign/) 签名，无需分发任何密钥：
证书写明了生成它的工作流和标签，签名记录在 Sigstore 的公开透明日志中。要检查某个镜像是否就是该版本生成的镜像：

```sh
cosign verify quay.io/oxidestack/docx-to-pdf:1.2.1 \
  --certificate-identity https://github.com/oxidestack/docx-to-pdf/.github/workflows/release.yml@refs/tags/v1.2.1 \
  --certificate-oidc-issuer https://token.actions.githubusercontent.com
cosign verify quay.io/oxidestack/charts/docx-to-pdf:1.2.1 \
  --certificate-identity https://github.com/oxidestack/docx-to-pdf/.github/workflows/release.yml@refs/tags/v1.2.1 \
  --certificate-oidc-issuer https://token.actions.githubusercontent.com
```

镜像还带有证明（attestation）：多架构镜像上有其 [SLSA 构建来源证明](https://slsa.dev/spec/v1.0/provenance)
（哪个提交、哪次工作流运行），每个架构的镜像上有一份 SPDX SBOM，列出其中包含的 Debian 软件包和 crate。
[GitHub CLI](https://cli.github.com/) 可以检查它们：

```sh
gh attestation verify oci://quay.io/oxidestack/docx-to-pdf:1.2.1 --repo oxidestack/docx-to-pdf
gh attestation verify oci://quay.io/oxidestack/charts/docx-to-pdf:1.2.1 --repo oxidestack/docx-to-pdf
```

对于 SBOM，请用摘要指定某一个架构的镜像（`docker buildx imagetools inspect` 会列出它们），并指定其
predicate 类型 `--predicate-type https://spdx.dev/Document/v2.3`。SBOM 也以文件形式附在 GitHub release 上，
有 SPDX 和 CycloneDX 两种格式，每个架构一份，与 chart 和 compose 示例放在一起。

附在 GitHub release 上的每个文件都以同样的方式签名，其 Sigstore bundle 作为 `<file>.sigstore.json`
附在旁边，`docx-to-pdf-1.2.1.intoto.jsonl` 是所有这些文件的构建来源证明。这些 bundle 采用 Sigstore
当前的 bundle 格式，cosign 3 起 `cosign verify-blob` 默认读取该格式。cosign 2 只有在命令中加上
`--new-bundle-format` 时才能读取它们；不加的话，它会返回 `bundle does not contain cert for
verification`。下载文件及其 bundle，然后用下面任一或两个命令检查：

```sh
cosign verify-blob docx-to-pdf-1.2.1.tgz --bundle docx-to-pdf-1.2.1.tgz.sigstore.json \
  --certificate-identity https://github.com/oxidestack/docx-to-pdf/.github/workflows/release.yml@refs/tags/v1.2.1 \
  --certificate-oidc-issuer https://token.actions.githubusercontent.com
gh attestation verify docx-to-pdf-1.2.1.tgz --repo oxidestack/docx-to-pdf
```

SBOM、compose 示例和 Helm 仓库的 `index.yaml` 也是如此。第一个版本 1.0.0 的文件是在 release
开始为其签名之前附上的，因此没有 bundle：这两个命令从 1.1.0 起适用。

ghcr.io 上的镜像站带有相同的摘要，以及随镜像一起复制的相同签名和证明，因此同样的命令也能验证
`ghcr.io/oxidestack/docx-to-pdf:1.2.1` 和 `ghcr.io/oxidestack/charts/docx-to-pdf:1.2.1`
（给 `gh attestation verify` 加上 `--bundle-from-oci`，即可从 registry 本身读取证明）。
