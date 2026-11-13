# 配置

<!-- languages:begin -->
[English](../../../guide/configuration.md) · [Русский](../../ru/guide/configuration.md) · **中文** ·
[Español](../../es/guide/configuration.md)
<!-- languages:end -->
<!-- translation of docs/guide/configuration.md: 5325aabd7af2 -->

每个设置都是一个环境变量，在服务启动时读取一次。本页列出了全部设置；有一个测试会把本页的表格与代码读取的变量进行比对，
因此任何设置都不可能在这里没有对应的行。

## 如何读取值

- **未设置与空值**是一回事：只包含空格的变量视为未设置，此时使用默认值。每个值的首尾空格都会被去掉。
- **服务无法使用的值会让它在启动时停止**，并在标准错误上输出一行，写明变量名和问题所在，退出码不为零。
  遇到错误的值时，它从不回退到默认值。
- **时长**需要带单位：`500ms`、`30s`、`2m`、`1h`。必须是整数，不能有小数，中间不能有空格；单独的 `30`
  会被拒绝。
- **大小**是纯字节数（`1048576`），或带单位的整数，单位不区分大小写：`B`、`KiB`、`MiB`、`GiB`。
  `KB`、`MB` 和 `GB` 也被接受，且按**二进制**计算，与 Spring 的设置相同，因此从 Spring 应用复制来的值保持原意：
  `10MB` 是 10 485 760 字节，与 `10MiB` 相同。
- **端口**是 1 到 65535 的整数。有一个针对 Kubernetes 的例外：名为 `docx-to-pdf` 的 Service 会让
  Kubernetes 向其命名空间中的每个 pod 注入 `DOCX_TO_PDF_PORT=tcp://<cluster IP>:<port>`（即
  “service link”），名为 `docx-to-pdf-probe` 的 Service 则对 `DOCX_TO_PDF_PROBE_PORT` 做同样的事。
  这两个变量中这种形式的值（`tcp://`、`udp://` 或 `sctp://` 后跟 IP 地址和端口）不算设置：它会被忽略，
  并在日志中输出一条写明变量名的警告，就像该变量未设置一样。其他任何不是端口的值仍然会让启动停止。
  要使用其他端口，请显式设置它，或在 pod 上设置 `enableServiceLinks: false`。

## 服务器

| 变量 | 取值 | 默认值 | 作用 |
| --- | --- | --- | --- |
| `DOCX_TO_PDF_PORT` | 端口 | `8080` | 服务在所有网络接口上监听的端口，用于转换端点、健康检查和指标。这里的 Kubernetes service link（`tcp://<ip>:<port>`）会被忽略并输出警告，端口回退到 `SERVER_PORT`，否则为 `8080`。 |
| `SERVER_PORT` | 端口 | — | 同一个设置，使用 moalhaddar/docx-to-pdf 的名称。仅在 `DOCX_TO_PDF_PORT` 未设置时读取。 |
| `DOCX_TO_PDF_PROBE_PORT` | 端口，不能与服务端口相同 | 未设置：无探测 | 在单独的端口上开启平台探测 `POST /probe`。探测测量这台机器的转换速度，用于规划部署容量；运行期间它占用所有 LibreOffice 实例，因此转换会得到 503，`/readyz` 报告未就绪。这就是它从不与转换端口共用的原因：不要让客户端访问这个端口。这里的 Kubernetes service link 会被忽略并输出警告，探测保持关闭。 |
| `DOCX_TO_PDF_IDLE_TIMEOUT` | 大于 0 的时长 | `120s` | 请求在没有收到其头部或正文任何一个字节的情况下最长可以持续多久。超过后客户端得到 408，连接被关闭。持续发送的慢速上传无论总共需要多久都会被等待；转换运行期间或其 PDF 发送期间不计时。没有进行中请求的 keep-alive 连接在同样的时间后被关闭，不作响应。 |
| `DOCX_TO_PDF_SHUTDOWN_GRACE` | 时长，允许 `0s` | `DOCX_TO_PDF_CONVERSION_TIMEOUT` 的值（`180s`） | 收到 `SIGTERM` 后，已在进行中的请求最多还能用多长时间完成。超过后仍在运行的请求得到 503；仍在发送的 PDF 会被截断。默认值让信号到来前刚开始的转换能够运行到它自己的时限。 |

**停机最多需要**：宽限期加上约 2 秒用于 HTTP，然后最多 5 秒用于停止 LibreOffice 实例，
然后在导出遥测数据时，再进行一次以 exporter 超时为上限的最终刷新。请给容器运行时留出比这更长的时间，
然后它才杀死进程：Kubernetes 的 `terminationGracePeriodSeconds` 设为宽限期加约 40 秒（默认值下为 `220`），
`docker stop -t` 也用同样的数字。

## LibreOffice 实例池

服务使用一个固定大小的 LibreOffice 进程池（“实例”）进行转换，每个实例同一时间只做一次转换。
发现所有实例都忙的请求会在队列中等待。

| 变量 | 取值 | 默认值 | 作用 |
| --- | --- | --- | --- |
| `DOCX_TO_PDF_POOL_SIZE` | 整数，1 或更大 | `1` | 运行多少个实例，也就是多少份文档可以并行转换。每个实例都是一个独立的 LibreOffice 进程，有自己的内存；容器的规格见 `DOCX_TO_PDF_INSTANCE_HARD_MEMORY`。它也限制了同时缩小图片的文档数量（`DOCX_TO_PDF_PICTURE_MAX_BITMAP`），这一步在文档获得实例之前运行。 |
| `POOL_SIZE` | 整数，1 或更大 | — | 池大小，使用 moalhaddar/docx-to-pdf 的名称。仅在 `DOCX_TO_PDF_POOL_SIZE` 未设置时读取。 |
| `pool.size` | 整数，1 或更大 | — | moalhaddar/docx-to-pdf 对池大小的另一种写法。仅在 `DOCX_TO_PDF_POOL_SIZE` 和 `POOL_SIZE` 都未设置时读取。 |
| `DOCX_TO_PDF_POOL_MAX_WAITING` | 整数，0 或更大 | `16` | 同时可以有多少个请求等待实例。下一个请求会立即得到 503，而不是加入队列。`0`：没有请求等待；所有实例都忙时，立即响应 503。 |
| `DOCX_TO_PDF_POOL_WAIT_TIMEOUT` | 时长，允许 `0s` | `60s` | 请求在队列中等待空闲实例的最长时间，也是有图片需要缩小的文档等待轮到它的最长时间。超过后返回 503。请把客户端的读取超时设为大于此值加 `DOCX_TO_PDF_CONVERSION_TIMEOUT`，否则客户端会放弃服务仍会响应的请求。 |
| `DOCX_TO_PDF_INSTANCE_START_TIMEOUT` | 大于 0 的时长 | `30s` | 实例从启动到就绪最多可以用多长时间。超时的实例会被杀死，并在暂停后重新尝试启动。服务启动时，连续三次失败的实例会让服务报错停止。实例通常在远不到一秒的时间内启动。 |
| `DOCX_TO_PDF_CONVERSION_TIMEOUT` | 大于 0 的时长 | `180s` | 一次转换最长可以运行多久。超过后实例被杀死并替换，客户端得到 504。默认值约为在默认大小限制内测得的最慢文档的三倍（一份含 1000 个表格的文档，在一个 CPU 上约需一分钟），因此在更慢或更忙的机器上也能转换完成；一份典型报告约一秒即可转换完成。 |
| `DOCX_TO_PDF_INSTANCE_MAX_CONVERSIONS` | 整数，0 或更大 | `200` | 实例完成这么多次转换后会被一个新实例替换；`0` 表示从不因此替换。实例在数千次转换中没有表现出增长，因此这只是一种保险；一次替换约需三分之一秒。 |
| `DOCX_TO_PDF_INSTANCE_MAX_MEMORY` | 大小，允许 `0` | `1GiB` | 转换后常驻内存高于此值的实例，会在接受下一次转换之前被替换；`0` 表示不设此上限。典型报告会让实例保持在 450 到 650 MiB；非常大的文档可能让它永久停留在 1 GiB 以上，这个设置就是用来处理这种情况的。 |
| `DOCX_TO_PDF_INSTANCE_HARD_MEMORY` | 大小，允许 `0`；除非两者之一为 `0`，否则必须大于 `DOCX_TO_PDF_INSTANCE_MAX_MEMORY` | `2GiB` | 实例转换期间，每 100 ms 读取一次其常驻内存；超过此值时实例立即被杀死并替换，请求得到 413。`0` 表示不设硬限制。原本可能拖垮整个容器的文档，这样只会损失一个请求。如果 DOCX 或 ODT 中的图片（按缩小之后的大小计算）总和超过此值减去 192 MiB（带透明度的图片按其位图的二点五倍计算），在解码任何图片之前就会得到同样的 413。请据此确定容器的内存：**256 MiB + 池大小 × (此值 + 256 MiB)**，默认值下一个实例为 2.5 GiB。 |

## 请求可携带内容的限制

| 变量 | 取值 | 默认值 | 作用 |
| --- | --- | --- | --- |
| `DOCX_TO_PDF_MAX_DOCUMENT_SIZE` | 大于 0 的大小 | `10MiB` | 接受的最大文档。超过时，客户端在正文仍在到达时就会得到 413，因此超大的上传不会被读到结尾。整个请求可以比它大 64 KiB，用于 multipart 封装和其他表单字段；声明的 `Content-Length` 超过该值时，在读取任何正文之前就会得到 413。为图片很多的文档调高此值，还需要为每个实例提供更多内存：LibreOffice 在转换时会把每张图片保持为解码状态，对于照片约为文档大小的 12 倍。 |
| `DOCX_TO_PDF_MAX_INFLATED_SIZE` | 大小；`0` 表示无限制，否则不能小于 `DOCX_TO_PDF_MAX_DOCUMENT_SIZE` | `256MiB`，或在 3 × `DOCX_TO_PDF_MAX_DOCUMENT_SIZE` 更大时取后者 | DOCX 或 ODT 是一个 ZIP 压缩包。当其各部分解压后的总和超过此值时，请求在任何转换开始之前就会得到 413。各部分会被实际解压以计算大小；压缩包声明的大小不被信任。这能拦住“zip 炸弹”，即一个能膨胀到数 GB 的小文件。默认大小限制内的所有普通文档都远低于默认值。 |

## 图片

| 变量 | 取值 | 默认值 | 作用 |
| --- | --- | --- | --- |
| `DOCX_TO_PDF_PICTURE_MAX_BITMAP` | 大于 0 的大小 | `256MiB` | DOCX 或 ODT 中解码后位图（宽 × 高 × 每像素 1、3 或 4 字节）大于此值的 PNG 或 JPEG，会在转换前被修改：按能使其低于此值的最小的 2 的幂缩小；无法安全解码时，则用占位图替换。响应的 `Docx-To-Pdf-Pictures` 头说明修改了什么。LibreOffice 本身会悄悄丢弃解码后超过 1 073 741 823 字节的图片；更高的值等同于这个上限。 |
| `DOCX_TO_PDF_PDF_MAX_IMAGE_DPI` | 整数，1 或更大 | 未设置：图片保持原样 | 在文档中以高于此值的分辨率放置的图片，在 PDF 中会被降到此分辨率。未设置时，每张图片都按原样嵌入（JPEG 逐字节不变）：典型报告转换所需的 CPU 约为三分之一，但图片放置分辨率高于 300 DPI 的文档会生成更大的 PDF。`300` 重现 moalhaddar/docx-to-pdf 生成的结果。 |

## 日志与遥测

日志总是输出到标准输出：每行一个 JSON 对象，或者在标准输出是终端时输出人类可读的文本。
每个 JSON 行都带有其所属请求的 `trace_id` 和 `span_id`。Prometheus 指标总是在 `GET /metrics` 提供。
设置了 endpoint 之后，trace、日志和指标还会通过 OpenTelemetry 协议 OTLP 导出；没有 endpoint 时不导出任何内容。

| 变量 | 取值 | 默认值 | 作用 |
| --- | --- | --- | --- |
| `RUST_LOG` | 一个过滤器：级别（`error`、`warn`、`info`、`debug`、`trace`），或目标与级别，如 `docx_to_pdf=debug,info` | `info` | 哪些日志行会到达标准输出。它不改变通过 OTLP 导出的内容，后者为 `info` 及以上。 |
| `DOCX_TO_PDF_ACCESS_LOG` | `true` 或 `false`，不区分大小写 | `true` | 每个请求结束时输出一行 `info` 日志，位于该请求之内，因此带有它的 `trace_id` 和 `span_id`：方法、路由、状态码、耗时、请求体与响应体的大小，以及请求失败时的 `error.type`（见[运维](operations.md#日志)）。健康检查和指标抓取（`/livez`、`/readyz`、`/metrics`）以 `debug` 级别记录，因此除非 `RUST_LOG` 用 `docx_to_pdf::access_log=debug` 要求，否则不会出现。`false` 不记录任何请求。 |
| `OTEL_EXPORTER_OTLP_ENDPOINT` | URL | 未设置：不导出 | 接收全部三种信号的 collector，例如 `http://otel-collector:4318`（HTTP）或 `http://otel-collector:4317`（gRPC）。通过 HTTP 时，会在其后加上每种信号的路径（`/v1/traces`、`/v1/logs`、`/v1/metrics`）。 |
| `OTEL_EXPORTER_OTLP_<SIGNAL>_ENDPOINT` | URL | 未设置 | 某一种信号的 collector，`<SIGNAL>` 为 `TRACES`、`LOGS` 或 `METRICS`。即使没有通用 endpoint，也会开启该信号的导出，并且按原样使用，包括路径（`http://collector:4318/v1/traces`）。 |
| `OTEL_EXPORTER_OTLP_PROTOCOL` | `grpc`、`http/protobuf` 或 `http/json` | `http/protobuf` | 信号的发送方式。 |
| `OTEL_EXPORTER_OTLP_<SIGNAL>_PROTOCOL` | 同上 | 通用协议 | 某一种信号的协议。 |
| `OTEL_<SIGNAL>_EXPORTER` | `otlp` 或 `none` | `otlp` | `none` 关闭某一种信号的导出，其他信号保持开启（`OTEL_TRACES_EXPORTER`、`OTEL_LOGS_EXPORTER`、`OTEL_METRICS_EXPORTER`）。任何其他值也会关闭它，并在日志中输出警告。 |
| `OTEL_SDK_DISABLED` | `true` 或 `false`，不区分大小写 | `false` | `true` 关闭所有 OTLP 导出。输出到标准输出的日志和 `/metrics` 保留。 |
| `OTEL_SERVICE_NAME` | 文本 | `docx-to-pdf` | 每种信号携带的 `service.name`。 |
| `OTEL_RESOURCE_ATTRIBUTES` | 以逗号分隔的 `key=value` 对 | — | 附加在每种信号上的额外属性，例如 `deployment.environment=prod`。服务会加上自己的 `service.version` 和 `upstream.version`。 |
| `OTEL_EXPORTER_OTLP_HEADERS` | 以逗号分隔的 `key=value` 对，值需 URL 编码 | — | 每次导出都发送的请求头，用于需要令牌的 collector。 |
| `OTEL_EXPORTER_OTLP_<SIGNAL>_HEADERS` | 同上 | 通用请求头 | 某一种信号导出时的请求头。 |
| `OTEL_EXPORTER_OTLP_TIMEOUT` | 毫秒 | `10000` | 一次导出最长可用的时间。它也限制停机时的最终刷新。 |
| `OTEL_EXPORTER_OTLP_<SIGNAL>_TIMEOUT` | 毫秒 | 通用超时 | 同上，针对某一种信号。 |

导出不内置压缩：`OTEL_EXPORTER_OTLP_COMPRESSION` 会被忽略并输出警告，导出内容不经压缩。
OpenTelemetry SDK 的其他标准变量（批处理，如 `OTEL_BSP_SCHEDULE_DELAY`；采样，如 `OTEL_TRACES_SAMPLER`；
指标间隔 `OTEL_METRIC_EXPORT_INTERVAL`）保持 OpenTelemetry 规范赋予它们的含义；服务本身不设置其中任何一个。

## 系统

| 变量 | 取值 | 默认值 | 作用 |
| --- | --- | --- | --- |
| `TMPDIR` | 目录路径 | `/tmp` | 服务工作时写入的位置：每个实例的 LibreOffice 配置文件目录（profile）、上传的文档、正在输出的 PDF，以及正在缩小的图片。一次转换在那里同时最多可能占用文档大小的三倍，文档较小时为 256 MiB。容器镜像把它设为 `/tmp`；根文件系统为只读时，请在那里挂载一个可写卷。 |

## 不会读取的设置

moalhaddar/docx-to-pdf 是一个 Spring Boot 应用，它的部署中常带有 Spring 和 JVM 设置。这些设置会被忽略，
因此请删除它们，或把它们换算成上面的行：`JAVA_TOOL_OPTIONS`、所有 `SPRING_*`（multipart 大小限制变为
`DOCX_TO_PDF_MAX_DOCUMENT_SIZE`）、所有 `SERVER_TOMCAT_*`（连接超时变为 `DOCX_TO_PDF_IDLE_TIMEOUT`，
以时长而不是毫秒表示）、所有 `MANAGEMENT_*`，以及 `LOGGING_LEVEL_*`（变为 `RUST_LOG`）。
