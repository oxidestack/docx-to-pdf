# 运维

<!-- languages:begin -->
[English](../../../guide/operations.md) · [Русский](../../ru/guide/operations.md) · **中文** ·
[Español](../../es/guide/operations.md)
<!-- languages:end -->
<!-- translation of docs/guide/operations.md: c560ba149348 -->

如何在容器中或 Kubernetes 上运行服务、监控它、限制一份文档可能造成的开销、规划部署容量以及为它提供字体。
这里提到的每个设置都是一个环境变量，在[配置](configuration.md)中有说明；本页说明它们如何配合。
有一个测试保证本页与代码一致：下面的指标名称就是服务注册的名称，Kubernetes 示例只设置服务会读取的变量，
且取值都是服务接受的值。

## 容器

镜像只运行一个进程，即服务本身，由 `tini` 托管，因此 `SIGTERM` 能够到达它，LibreOffice 进程被杀死时也不会留下残余。
它监听端口 `8080`，为 `linux/amd64` 和 `linux/arm64` 构建。

它被设计为以锁定方式运行，并按这种方式进行测试：

- **以用户 `10001`**、组 `10001` 运行，该用户在 `/etc/passwd` 中没有条目；任何其他非 root 用户也可以；
- **使用只读根文件系统。** 服务唯一写入的目录是 `/tmp`（镜像把 `TMPDIR`、`HOME` 和 `XDG_CACHE_HOME`
  设在其下）：每个实例的 LibreOffice 配置文件目录（profile）、上传的文档、正在输出的 PDF、
  正在缩小的图片，以及你挂载的字体的字体缓存。请在那里挂载一个可写卷；
- **丢弃所有 Linux capabilities**，并禁止权限提升。镜像中没有任何文件保留 setuid 或 setgid 位，
  因此没有任何可以提升到的权限；
- **除了自己的端口之外不使用网络**：它不调用任何其他服务，除非你设置了 OpenTelemetry collector。
  Helm chart 的 NetworkPolicy 把 pod 限制在这个范围内
  （[安装](installation.md#网络服务不获取任何内容)）。

```sh
docker run --rm -p 8080:8080 \
  --user 10001:10001 --read-only --cap-drop ALL --security-opt no-new-privileges \
  --mount type=volume,dst=/tmp \
  quay.io/oxidestack/docx-to-pdf:1.2.0
```

### `/tmp`：放在磁盘上还是内存中

把 `/tmp` 放在磁盘上（一个卷，或 Kubernetes 上的 `emptyDir`）是推荐的选择：它不占用内存。tmpfs
（`--tmpfs /tmp:rw,mode=1777,size=512m`，或 `medium: Memory` 的 `emptyDir`）让转换不落盘，
但写入其中的一切都计入容器的内存限制，因此要把它的大小加到限制上。

一次转换在那里写入多少：同时最多为文档大小的三倍（文档、它的 PDF 以及 LibreOffice 自己的临时文件），
对于长版面——一份页数很多、PDF 会变得很大的小文档——约为 256 MiB。因此请为 `/tmp` 至少提供
**池大小 × 3 × `DOCX_TO_PDF_MAX_DOCUMENT_SIZE` 与 256 MiB 中的较大者**，再为每个实例的配置文件目录加上远不到 1 MB 的空间。

## 健康检查

| 路径 | 响应 | 用作 |
| --- | --- | --- |
| `GET /livez` | 服务一开始监听就返回 `200 ok`，无论其实例在做什么。 | 存活检查。只有当进程完全不再响应 HTTP 时才会失败。 |
| `GET /readyz` | 至少有一个 LibreOffice 实例能接收转换时返回 `200 ok`；在第一个实例就绪之前、没有实例能接收时、容量探测运行期间（见下文），以及从停机开始的那一刻起，返回 `503 not ready`。 | 就绪检查，以及启动检查。 |

服务在其实例启动之前就开始监听，因此 `/livez` 立即响应，而 `/readyz` 在第一个实例就绪后变为 `200`，
通常在一两秒之内。无法启动的实例会被重试；启动阶段连续三次失败会让服务报错停止。
因此启动检查应允许多于三倍 `DOCX_TO_PDF_INSTANCE_START_TIMEOUT`（`30s`）再加几秒的时间：
下面的示例允许 120 秒。

镜像中没有 HTTP 客户端，因此没有 Docker `HEALTHCHECK`；请让编排系统调用这两个路径。

## 停止

收到 `SIGTERM`（或 `SIGINT`）时，服务会：

1. 把 `/readyz` 变为 `503`，并停止接受新连接；
2. 让已在进行中的请求完成，最多 `DOCX_TO_PDF_SHUTDOWN_GRACE`（默认为转换超时 `180s`，
   使信号到来前刚开始的转换能够运行到它自己的时限）。宽限期结束时仍在运行的请求得到 `503`；
   仍在发送的 PDF 会被截断；
3. 停止其 LibreOffice 实例，最多 5 秒；
4. 刷新它导出的遥测数据，以 exporter 的超时为上限，然后以退出码 0 退出。

请给运行时留出比这更长的时间，然后它才杀死进程：宽限期加约 40 秒。在 Kubernetes 上，
还要让 pod 在信号到来之前先休眠几秒（`preStop`），使 Service 和任何负载均衡器在它停止接收请求之前就不再向它发送请求：
此时 `terminationGracePeriodSeconds` 为该休眠 + 宽限期 + 40。

## Kubernetes

一个完整的示例：每个 pod 并行两个转换、上述的安全上下文、`/tmp` 放在磁盘上、三种检查、优雅停止和 OTLP 导出。
项目的 Helm chart `charts/docx-to-pdf` 用少量 values 渲染出同样的内容，并为你计算资源。
[安装](installation.md#kubernetes)说明如何安装它。

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
          image: quay.io/oxidestack/docx-to-pdf:1.2.0
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

每个数字为什么是这样：

- **`DOCX_TO_PDF_POOL_SIZE` = CPU request。** 一次转换让一个核心保持繁忙；实例多于核心不会增加吞吐量，
  反而会让每次转换变慢。
- **内存：256 MiB + 池大小 × (`DOCX_TO_PDF_INSTANCE_HARD_MEMORY` + 256 MiB)**，这里为 256 + 2 × 2304 = 4864 MiB。
  服务会杀死转换期间内存超过硬限制的实例（该请求得到 `413`），每 100 ms 检查一次；每个实例 256 MiB
  覆盖两次检查之间的增长，另外 256 MiB 覆盖服务本身。有了这个限制，任何文档都无法让 pod 因内存而被杀死。
  request 设得与 limit 一样多，使调度器把 pod 放在确实有这么多内存的地方。tmpfs 形式的 `/tmp` 要把它的大小加在这里。
- **`/tmp`：2 × max(3 × 10 MiB, 256 MiB) = 512 MiB**，使用默认的 10 MiB 文档限制。
- **`terminationGracePeriodSeconds`：5 + 180 + 40 = 225**，即 `preStop` 休眠、停机宽限期，以及服务在其后所需的时间。
- **`enableServiceLinks: false`。** 名为 `docx-to-pdf` 的 Service 会让 Kubernetes 在命名空间的每个 pod 中把
  `DOCX_TO_PDF_PORT` 设为 `tcp://<ip>:8080`。服务会忽略这种形式的值并输出警告，但关闭 service link
  并显式设置端口，可以让这个设置由你掌控。
- **没有 service account 令牌**：服务从不调用 Kubernetes API。

一个 `maxUnavailable: 1` 的 `PodDisruptionBudget` 让节点每次只驱逐一个 pod。

## 遥测

### 日志

在标准输出上每行一个 JSON 对象（标准输出是终端时输出人类可读的文本），由 `RUST_LOG` 过滤，默认为 `info`。
请求内的每一行都带有该请求的 `trace_id` 和 `span_id`，因此一条日志可以引向它的 trace。
服务忽略的设置，例如 Kubernetes service link，会在启动时输出一条 `warn` 日志。

### 链路追踪

设置了 OTLP endpoint 时（`OTEL_EXPORTER_OTLP_ENDPOINT`，见[配置](configuration.md#日志与遥测)），
每个请求都是一条 trace。它的根 span 以方法和路由命名，例如 `POST /pdf`；其下有 `document.sniff`（识别格式）、
`pool.acquire`（等待实例），以及 `backend.load` 和 `backend.export`（LibreOffice 加载文档和写出 PDF）。
启动实例有它自己的 span，`instance.start`。请求上的 W3C `traceparent` 头会让该 trace 成为调用方 trace 的子级。

OTLP endpoint 只能是 ASCII：国际化主机名请用 punycode 书写（`http://xn--bcher-kva.example:4318`，而不是
`http://bücher.example:4318`；`idn2 bücher.example` 会输出它），路径请做百分号编码。
含有任何其他字符的 endpoint 不会为其信号导出任何内容，并在启动时用一条写明变量名的 `warn` 日志说明这一点。

### 指标

指标始终以 Prometheus 文本格式在服务端口的 `GET /metrics` 上提供，设置了 endpoint 时还会通过 OTLP 导出。
名称遵循 OpenTelemetry 约定；Prometheus 用下划线书写它们，以单位作为后缀，计数器带 `_total`，
而像 `http.route` 这样的属性会变成标签 `http_route`。一个 `target_info` 序列带有 `service.name`、
`service.version`，以及服务所兼容的 moalhaddar/docx-to-pdf 版本（`upstream.version`）。

| OpenTelemetry 名称 | Prometheus 名称 | 类型 | 属性 | 它告诉你什么 |
| --- | --- | --- | --- | --- |
| `http.server.request.duration` | `http_server_request_duration_seconds` | histogram，秒 | `http.request.method`、`http.route`、`http.response.status_code`、`error.type` | 每个请求从到达到响应最后一个字节所花的时间。`http.route` 是路径模式（`/pdf`、`/readyz`），从不是原始路径。 |
| `http.server.active_requests` | `http_server_active_requests` | gauge | `http.request.method`、`http.route` | 当前进行中的请求。 |
| `docx_to_pdf.document.size` | `docx_to_pdf_document_size_bytes` | histogram，字节 | `format` | 收到的每份文档的大小，按识别出的格式区分。 |
| `docx_to_pdf.conversions` | `docx_to_pdf_conversions_total` | counter | `outcome`、`retried` | 转换，按结束方式区分：`success`、`unsupported`（415）、`not_loaded`（422）、`timeout`（504）、`memory_limit`（413）、`dead_instance`（500）、`cancelled`（客户端离开）或 `error`（500）。当第一个实例崩溃、文档在另一个实例上重新转换时，`retried` 为 `true`。 |
| `docx_to_pdf.conversion.duration` | `docx_to_pdf_conversion_duration_seconds` | histogram，秒 | `outcome` | LibreOffice 自身的转换时间，加载加导出，不含上传、排队或下载。 |
| `docx_to_pdf.pdf.size` | `docx_to_pdf_pdf_size_bytes` | histogram，字节 | — | 每个响应的 PDF 的大小。 |
| `docx_to_pdf.pictures` | `docx_to_pdf_pictures_total` | counter | `outcome` | 因过大而无法安全解码、在转换前被修改的图片：`downscaled`（缩小）、`replaced`（被占位图替换），或 `failed`（文档按原样转换）。 |
| `docx_to_pdf.pool.size` | `docx_to_pdf_pool_size` | gauge | — | 配置的 LibreOffice 实例数。 |
| `docx_to_pdf.pool.instances` | `docx_to_pdf_pool_instances` | gauge | `state` | 当前处于 `idle`、`busy` 或 `starting` 状态的实例。`idle` 长时间为 0 说明请求在排队：见下面的等待时间。 |
| `docx_to_pdf.pool.wait.duration` | `docx_to_pdf_pool_wait_duration_seconds` | histogram，秒 | `outcome` | 请求等待空闲实例的时间；`acquired`，或以 503 `rejected`。等待时间逐渐接近 `DOCX_TO_PDF_POOL_WAIT_TIMEOUT` 说明 pod 需要更多实例，或者需要更多 pod。 |
| `docx_to_pdf.instance.starts` | `docx_to_pdf_instance_starts_total` | counter | `reason` | 实例启动，按原因区分：`initial`、`recycle`（达到 `DOCX_TO_PDF_INSTANCE_MAX_CONVERSIONS` 后或超过 `DOCX_TO_PDF_INSTANCE_MAX_MEMORY` 时）、`crash`、`timeout`、`memory_limit`、`cancelled`（其转换被放弃）或 `failed`（上一次启动未成功）。除 `initial` 和 `recycle` 之外的任何一项持续上升都值得一查。 |
| `docx_to_pdf.instance.start.duration` | `docx_to_pdf_instance_start_duration_seconds` | histogram，秒 | — | 从启动实例到它就绪的时间。 |
| `docx_to_pdf.instance.memory.usage` | `docx_to_pdf_instance_memory_usage_bytes` | gauge，字节 | `instance` | 每个实例当前的常驻内存，按其在池中的槽位区分。 |
| `docx_to_pdf.instance.memory.peak` | `docx_to_pdf_instance_memory_peak_bytes` | histogram，字节 | — | 每次转换期间实例的最高常驻内存。把它与 `DOCX_TO_PDF_INSTANCE_HARD_MEMORY` 比较，可以看出你的文档离限制有多近。 |
| `process.cpu.time` | `process_cpu_time_seconds_total` | counter，秒 | `cpu.mode` | 服务进程本身使用的 CPU，`user` 和 `system`；实例是独立的进程，不计入这里。 |
| `process.memory.usage` | `process_memory_usage_bytes` | gauge，字节 | — | 服务进程自身的常驻内存。 |
| `process.unix.file_descriptor.count` | `process_unix_file_descriptor_count` | gauge | — | 服务进程打开的文件和套接字。 |

要用 Prometheus operator 抓取 `/metrics`，请把一个 `ServiceMonitor` 指向 Service 的 `http` 端口和路径
`/metrics`；Helm chart 中有一个，由 `serviceMonitor.enabled` 控制。

## 重型文档：拒绝什么，限制什么

接受他人文档的转换器必须能扛住其中任何一份。服务对一份文档可能造成的每一种开销都设了界限，
每个界限最终都给出明确的响应，而不是一个卡住或崩溃的 pod。所有不是 PDF 的响应都是
`application/problem+json`，带有固定的 `detail` 句子。

| 一份文档…… | 由什么拦住 | 客户端得到 |
| --- | --- | --- |
| 大于允许的大小 | `DOCX_TO_PDF_MAX_DOCUMENT_SIZE`（10 MiB）：在正文到达时检查，并在读取任何正文之前对照 `Content-Length` 检查 | `413`，服务不会读取上传的其余部分 |
| 是一个会极度膨胀的小压缩包（“zip 炸弹”） | `DOCX_TO_PDF_MAX_INFLATED_SIZE`（256 MiB）：在转换前实际解压 DOCX 或 ODT 并计算大小 | `413` |
| 含有一张大到无法在内存中解码的图片 | `DOCX_TO_PDF_PICTURE_MAX_BITMAP`（解码后 256 MiB）：图片被缩小，或被占位图替换 | 一份 PDF，带有 `Docx-To-Pdf-Pictures` 头，说明有多少张图片被修改 |
| 排版所需的内存超过允许值 | `DOCX_TO_PDF_INSTANCE_HARD_MEMORY`（每个实例 2 GiB）：实例被杀死并替换 | `413` |
| 转换时间过长 | `DOCX_TO_PDF_CONVERSION_TIMEOUT`（180 s）：实例被杀死并替换 | `504` |
| 不是服务转换的格式 | 格式根据内容而不是名称识别 | `415` |
| 格式受支持，但 LibreOffice 无法打开 | — | `422` |
| 让 LibreOffice 崩溃 | 文档会在另一个实例上再试一次 | 第二次成功则得到 PDF，否则得到 `500` |
| 到达太慢，或不再到达 | `DOCX_TO_PDF_IDLE_TIMEOUT`（120 s 没有一个字节） | `408` |

而当请求多于实例时：

| 当…… | 由什么限制 | 客户端得到 |
| --- | --- | --- |
| 所有实例都忙 | 请求在队列中等待，先到先服务 | PDF，稍后 |
| 队列已满 | `DOCX_TO_PDF_POOL_MAX_WAITING`（16） | 立即得到 `503`，带有 `Retry-After: 10` |
| 请求等待太久 | `DOCX_TO_PDF_POOL_WAIT_TIMEOUT`（60 s） | `503`，带有 `Retry-After: 10` |

在 `Retry-After` 之后重试 `503`、并把读取超时设为大于等待超时加转换超时（默认值下为 240 秒）的客户端，
永远不会放弃服务仍会响应的请求。

在完成 `DOCX_TO_PDF_INSTANCE_MAX_CONVERSIONS` 次转换（200）后，或者某次转换后内存仍高于
`DOCX_TO_PDF_INSTANCE_MAX_MEMORY`（1 GiB）时，实例也会在两次转换之间被替换，且不会导致任何失败，
因此一份让 LibreOffice 变得庞大的文档不会让之后的每次转换都为它买单。一次替换约需三分之一秒。

每次转换之后、给出响应之前，实例会把 LibreOffice 释放的内存交还给系统（`malloc_trim`），
因此它在两次转换之间的常驻内存就是其工作集，而不是最近最大的那份文档留下的内存。
在典型报告的长时间运行测试（soak）中，这让单实例容器的内存降低约 50 MiB，每次转换只多花几毫秒；
它不会降低转换期间的峰值，而下面的内存限制正是按峰值确定的。这一行为没有对应的设置。

## 容量规划

### 起步配置

| 资源 | 公式 | 使用默认值 |
| --- | --- | --- |
| CPU | 池大小，每个实例一个核心 | 1 |
| 内存（request = limit） | 256 MiB + 池大小 × (`DOCX_TO_PDF_INSTANCE_HARD_MEMORY` + 256 MiB)，再加上 tmpfs 形式的 `/tmp` | 一个实例 2560 MiB |
| `/tmp` | 池大小 × max(3 × `DOCX_TO_PDF_MAX_DOCUMENT_SIZE`, 256 MiB) | 一个实例 256 MiB |

一份典型的业务报告在一个核心上约一秒即可转换完成，因此一个实例每分钟大约能处理 60 份这样的文档。
不过文档之间相差几个数量级，机器也是如此：请测量你自己的。

### 测量平台：容量探测

服务可以在其实际拥有的限制下，测量它所在机器的转换速度。设置 `DOCX_TO_PDF_PROBE_PORT`（例如设为 `9090`），
服务就会在那里监听一个端点 `POST /probe`。请保持该端口私有：永远不要通过 Service 暴露它。
探测运行期间它占用所有实例，因此转换会得到 `503`，`/readyz` 为未就绪，Kubernetes 会把流量路由到其他 pod。
请探测一个不接收流量的 pod，或者一个你可以腾出一两分钟的 pod。

```sh
kubectl port-forward pod/docx-to-pdf-7c9d8-abcde 9090:9090
curl -F document=@report-1.docx -F document=@report-2.docx \
  'http://localhost:9090/probe?cpus=2' -o platform.json
```

发送与你的流量相似的文档：最多 16 份，每份最大 64 MiB（如果 `DOCX_TO_PDF_MAX_DOCUMENT_SIZE` 更大，则以它为准）。
探测在服务端转换它们，不受网络影响：

1. 在每个实例上各转换一次，用于预热；
2. 逐一转换，重复 `rounds` 轮（默认 5）：平均值为 **`s1`**，即没有其他任务运行时一次转换的时间；
3. 在所有实例上同时转换，总共 `busy` 次（默认 120 + 3 × 池大小）：在每个核心都在转换时，
   一次转换慢了多少，以与 `s1` 的比值表示，即**争用系数（contention）**；
4. 在 `/tmp` 下直接写入 1 GiB（`disk_mib`，`0` 表示跳过；在 tmpfs 上会跳过）：磁盘的持续写入速度 **MB/s**。

然后它交还实例，并以 JSON 响应：上述三个数字，以及为这个 pod（`cpus`，默认为池大小）算好的每个容量公式：
池大小、pod 以每分钟转换次数计的吞吐量、一次转换单独运行和所有实例都忙时的时间、内存限制、`/tmp` 大小、
合适的超时、给定请求速率下的响应时间，以及满足某个速率和响应时间目标所需的 pod 数。
每个公式都附有其理由。可选的 `name` 参数在响应中为平台命名。

在一个探测运行期间发送的第二个探测会得到 `503`（“a probe is already running”），在服务启动或停止期间发送的探测也是如此。

### 把测量结果变成计划：`bench.py size`

该响应也是仓库中容量计算器 `tools/bench/bench.py size` 的输入，它只需要 Python 标准库。
可以问它某台机器能做什么，或者要达到某个目标需要什么：

```sh
# What 4 vCPU and 8 GiB give.
python3 tools/bench/bench.py size hardware --platform platform.json --cpus 4 --memory 8

# Pods for 60 requests a minute with 95 % of them answered within 5 seconds.
python3 tools/bench/bench.py size target --platform platform.json --latency 5 --quantile 0.95 --rate 60/min
```

`size hardware` 会给出这台机器允许的池大小（分别按其核心数和内存计算）、饱和时的吞吐量、
一次转换的平均时间和尾部时间、内存限制、`/tmp` 大小，以及启动服务时要用的设置，例如：

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

`size target` 列出满足目标的 pod 规格（vCPU、池大小、内存），以及每种规格需要多少个。

不带 `--platform` 时，计算器使用项目测量的参考平台；`--s1` 和 `--contention` 可以手动覆盖平台的数字。

## 字体

只有当文档指定的字体可用时，PDF 才能与文档看起来一样；缺少某种字体时，LibreOffice 会用另一种字体替代，
文字随之重新排版。镜像只带有许可证允许再分发的字体：

- **Carlito** 和 **Caladea**，字宽设计得与 Calibri 和 Cambria 相同；**Liberation** Sans、Serif、Mono
  和 Sans Narrow，字宽与 Arial、Times New Roman、Courier New 和 Arial Narrow 相同：
  使用这些 Microsoft 字体排版的文档能保持其换行和分页；
- **DejaVu**、**Linux Libertine**、**Gentium** 和 **OpenSymbol**（LibreOffice 的符号字体）；
- **Noto**：核心集和扩展集、Mono、UI、中文、日文和韩文（CJK）字体，以及彩色 emoji，
  因此世界上大多数文字都能正常显示，而不会退化成空白方框；
- moalhaddar/docx-to-pdf 自带的三个文件，**Droid Kufi**（常规和粗体）和 **Cantarell** Extra Bold，
  使为那个服务制作的文档看起来一样。

### 挂载你自己的字体

目录 `/usr/share/fonts/custom` 在镜像中是空的，并且会被搜索字体。请把你持有许可证的字体以只读方式挂载到那里，
例如在上述替代字体不够接近时使用 Microsoft 自己的字体：

```sh
docker run --rm -p 8080:8080 --user 10001:10001 --read-only --cap-drop ALL \
  --mount type=volume,dst=/tmp \
  -v "$PWD/fonts:/usr/share/fonts/custom:ro" \
  quay.io/oxidestack/docx-to-pdf:1.2.0
```

在 Kubernetes 上，把 Secret、ConfigMap 或任何卷挂载到同一路径（Helm chart 的 `fonts` 值就是这样做的）。
字体在实例启动时读取：修改后请重启 pod。修改后的第一个实例会在 `/tmp` 下为挂载的目录写入字体缓存，
这在启动时需要一点时间，之后不再有开销。
