# 从 moalhaddar/docx-to-pdf 迁移

<!-- languages:begin -->
[English](../../../guide/migration.md) · [Русский](../../ru/guide/migration.md) · **中文** ·
[Español](../../es/guide/migration.md)
<!-- languages:end -->
<!-- translation of docs/guide/migration.md: dc32e801d4cb -->

本页面向运行或调用 `moalhaddar/docx-to-pdf`（2.1.0-x）并希望迁移到本服务的人。本服务通过相同的接口转换相同的文档，
使用当前版本的 LibreOffice，没有原服务的那些故障模式，并内置 OpenTelemetry。本页说明哪些保持不变、
客户端要改什么、部署要改什么，并在最后列出每一处有意的差异及其理由。有一个测试保证本页与差异列表以及其他指南页面一致，
因此任何差异都不可能在这里没有对应的行。

## 保持不变的部分

- 端口 **8080**。
- `POST /pdf` 和 `POST /docx-to-pdf`，`multipart/form-data` 正文，文档放在名为 **`document`** 的文件部分中。
  该部分的文件名和 `Content-Type` 不起任何作用。
- 成功时为 **`200`**，`Content-Type: application/pdf`，正文就是 PDF。
- 对这两个路径的跨源请求：`Access-Control-Allow-Origin: *`，预检请求的响应带有
  `Access-Control-Allow-Methods: POST`，允许所请求的头，并带有 `Access-Control-Max-Age: 1800`。
- 能够转换的文档：DOCX、DOC、ODT 和 RTF，以及原服务确实能转换的 Word 系列变体：
  DOCM、DOTX、OTT 和扁平 ODT（[接受的格式](api.md#接受的格式)）。
- PDF 的外观：相同的页面、页面尺寸和文字。PDF 并非逐字节相同，因为 LibreOffice 更新了。
- 原服务自带的自由字体，以及常见 Word 字体（如 Calibri、Arial 和 Times New Roman）的相同替代字体
  （[字体](operations.md#字体)）。

只发送文档、读回 PDF、并把其他所有响应都当作失败的客户端，无需任何修改。

## 客户端需要修改的部分

### 状态码

原服务对大多数错误返回 `500`，对一些不是文档的输入还会返回 PDF。本服务对每种情况都返回其专属的状态码，
且每个错误正文都是 problem 文档（[错误](api.md#错误)）。

| 情况 | moalhaddar/docx-to-pdf | 本服务 | 是否重试？ |
| --- | --- | --- | --- |
| 正文不是 multipart、格式错误，或没有名为 `document` 的文件部分 | 500 或 400 | 400 | 否 |
| 未知路径，包括所有 `/actuator` 路径 | 404 | 404 | 否 |
| 已知路径使用其他方法（`GET /pdf`） | 405 | 405 | 否 |
| 两分钟内请求没有到达任何一个字节 | 500 | 408 | 否 |
| 文档超过大小限制（默认 10 MiB） | 500，在上传之后 | 413，在上传其余部分之前 | 否 |
| zip 炸弹，或需要的内存超过一个实例可用内存的文档 | 没有限制：容器先耗尽内存或磁盘 | 413 | 否 |
| 文本、空文件、电子表格、图片、随机字节 | 200，LibreOffice 对它生成的任意 PDF | 415 | 否 |
| LibreOffice 无法打开的文档 | 500 | 422 | 否 |
| 所有实例都忙且超过等待上限、队列已满、正在启动或正在停机 | 无限等待 | 503，带 `Retry-After` | 是，在 `Retry-After` 之后 |
| 转换运行超过转换超时（默认 3 分钟） | 无限等待 | 504 | 最多一次 |
| 其他任何情况 | 500 | 500 | 否 |

需要修改的地方：

- **错误正文。** Spring 的 `{"timestamp","status","error","path"}` 文档和 HTML 的“Whitelabel Error Page”
  都没有了。读取它们的客户端请改读 `status`，或只读 HTTP 状态码。不要匹配 `detail`：
  它是每种情况一句的固定句子，是给人看的（[所有错误](api.md#服务返回的所有错误)）。
- **重试。** 原服务从不拒绝工作，因此客户端唯一的失败就是它自己的读取超时。本服务在饱和时会返回 `503`。
  只重试连接错误的客户端，也应在 `503` 所带的 `Retry-After` 之后重试它；重试一次 `504` 是合理的；
  `4xx` 重试也不会改变。
- **读取超时。** 请把它设为大于等待上限加转换超时，默认为 60 + 180 秒，再留出余量，
  使客户端收到服务的 `503` 或 `504`，而不是自己切断连接。
- **不是文档的输入。** 纯文本、空文件和其他格式现在会得到 `415`。请先把它们转换为 DOCX。
- **被修改的图片。** 当有图片因过大而无法安全转换、被缩小或被替换时，`200` 可能带有
  `Docx-To-Pdf-Pictures: downscaled=<n>, replaced=<n>`（[转换前被修改的图片](api.md#转换前被修改的图片)）。
  想告知其用户的客户端可以读取它；其他客户端可以忽略它。

## 部署需要修改的部分

### 健康检查与指标

| 用途 | moalhaddar/docx-to-pdf | 本服务 |
| --- | --- | --- |
| 存活 | `GET /actuator/health/liveness` | `GET /livez` |
| 就绪，以及等待启动 | `GET /actuator/health/readiness`、`GET /actuator/health` | `GET /readyz` |
| Prometheus 指标 | — | `GET /metrics` |
| 通过 OTLP 输出 trace、日志和指标 | — | 设置 `OTEL_EXPORTER_OTLP_ENDPOINT` |

每个 `/actuator` 路径都返回 `404`，因此请替换对它的每一处引用：Kubernetes 探针、Testcontainers 的等待策略
（`.forPath("/actuator/health")` 改为 `.forPath("/readyz")`）、监控检查、脚本。
[健康检查](operations.md#健康检查)说明每个路径返回什么，以及启动检查应等待多久；
[指标](operations.md#指标)列出了每个指标，[遥测](configuration.md#日志与遥测)列出了 OpenTelemetry 设置。

### 设置

每个设置都是一个环境变量；[配置](configuration.md)列出了全部设置及其默认值。
对于 moalhaddar/docx-to-pdf 的部署，需要改变的只有它原来设置过的那些：

| moalhaddar/docx-to-pdf 的设置 | 在本服务中 |
| --- | --- |
| `SERVER_PORT` | 仍会读取。两者都设置时，以 `DOCX_TO_PDF_PORT` 为准。 |
| `POOL_SIZE`、`pool.size` | 仍会读取，按此顺序。`DOCX_TO_PDF_POOL_SIZE` 优先于两者。 |
| `SPRING_SERVLET_MULTIPART_MAX_FILE_SIZE`、`SPRING_SERVLET_MULTIPART_MAX_REQUEST_SIZE` | 不读取：请设置 `DOCX_TO_PDF_MAX_DOCUMENT_SIZE`。像 `1000MB` 这样的值保持原意。不作修改时，限制为 10 MiB，更大的文档会得到 `413`。 |
| `SERVER_TOMCAT_CONNECTION_TIMEOUT` | 不读取：最接近的是 `DOCX_TO_PDF_IDLE_TIMEOUT`，以时长而不是毫秒表示；`120000` 就是它的默认值 `120s`。 |
| `SERVER_TOMCAT_MAX_HTTP_FORM_POST_SIZE` | 不读取，也没有替代项：没有任何路径接受 URL 编码的表单。 |
| `JAVA_TOOL_OPTIONS` | 不读取：没有 JVM。其中的 `-Dpool.size` 也不会被读取。内存根据 `DOCX_TO_PDF_INSTANCE_HARD_MEMORY` 确定（[容量规划](operations.md#容量规划)）。 |
| `LOGGING_LEVEL_*` | 不读取：改用 `RUST_LOG`。 |
| 其他 `SPRING_*`、`SERVER_TOMCAT_*`、`MANAGEMENT_*` | 不读取。请删除它们。 |

有两个新设置在任何部署中都值得一看：`DOCX_TO_PDF_CONVERSION_TIMEOUT` 和
`DOCX_TO_PDF_POOL_WAIT_TIMEOUT`，它们为原服务无限等待的情况设了上限。

### 内存、磁盘和 CPU

没有了 JVM，容器的内存由 LibreOffice 实例决定。每个实例在超过 `DOCX_TO_PDF_INSTANCE_HARD_MEMORY`
（默认 2 GiB）时会被杀死并替换，其请求以 `413` 响应，因此无论文档如何，
256 MiB + 池大小 × (该限制 + 256 MiB) 的内存限制都不会被突破。为带图片的大文档调高了文档限制的部署，
要同时调高硬内存限制。`/tmp` 必须可写，默认放在磁盘上。[容量规划](operations.md#容量规划)给出了公式，
以及一个测量你自己机器的探测。

### 镜像

- 为 `linux/amd64` 和 `linux/arm64` 构建：在 Arm 机器上，包括 Apple Silicon，它原生运行，而不是在模拟下运行。
- 它以用户 `10001` 运行，没有 passwd 条目，任何其他非 root 用户也可以；根文件系统可以是只读的，
  `/tmp` 是它唯一写入的目录（[容器](operations.md#容器)）。
- PID 1 是 `tini`；`SIGTERM` 会启动优雅停止。请给运行时留出停机宽限期加约 40 秒的时间，
  然后它才杀死进程（[停止](operations.md#停止)）。
- 它不带 HTTP 客户端，因此没有 Docker `HEALTHCHECK`：请使用编排系统的 HTTP 检查。
- 原镜像带有的两种字体 Brown Std 和 GE Dinar Two 采用商业许可证，不在本镜像中：
  使用它们排版的文档会得到替代字体。请挂载你持有许可证的字体文件
  （[挂载你自己的字体](operations.md#挂载你自己的字体)）。

### 网络

除了已设置的 OpenTelemetry collector 之外，服务不会自行打开任何连接。Helm chart 的 NetworkPolicy
默认开启，并把 pod 限制在这个范围内：入站只到端口 8080，出站只到 DNS（UDP 和 TCP 53，目标是集群的
`kube-dns` pod）和 `otel.endpoint` 的端口。使用节点本地 DNS 缓存的集群把 `networkPolicy.dnsTo`
设为它的地址，只能通过特定地址访问的 collector 设置 `networkPolicy.otlpTo`，
而 `networkPolicy.enabled: false` 把 pod 的网络交给你自己的策略
（[网络](installation.md#网络服务不获取任何内容)）。

### 检查清单

1. 把镜像引用替换为本服务的镜像。
2. 把每个 `/actuator/health*` 替换为用于存活检查的 `/livez`，以及用于就绪检查和等待启动的 `/readyz`。
3. 按上表换算设置，并删除不会被读取的设置。
4. 把客户端的读取超时调高到大于等待上限加转换超时，并在 `Retry-After` 之后重试 `503`。
5. 查找解析原服务错误正文、依赖 `500`，或发送文字处理文档以外内容的代码。
6. 把 `OTEL_EXPORTER_OTLP_ENDPOINT` 指向一个 collector，或者抓取 `/metrics`。
7. 用新镜像运行客户端的测试。

## 所有有意的差异

下面的每一处差异都是有意为之：没有沿袭原服务的缺陷、补上它缺少的限制，或者去掉不属于接口的 Spring 细节。
PDF 本身的差异仅在于由更新的 LibreOffice 排版。

| 差异 | moalhaddar/docx-to-pdf | 本服务 | 原因 |
| --- | --- | --- | --- |
| 更新的 LibreOffice | LibreOffice 7.3：某些域中的页码和列表编号为空，一个长浮动表格会让转换挂起，在 Word 的 1.15 倍行距下每页少一行 | 当前版本的 LibreOffice：这些域被填上，该表格不到一秒即可转换，每页多一行；文字相同 | LibreOffice 7.3 已不再维护，而更新的版本能更好地渲染这些文档。 |
| PDF 中的图片 | 每张图片都被解码并降到最多 300 DPI | 按原样嵌入，JPEG 逐字节不变；`DOCX_TO_PDF_PDF_MAX_IMAGE_DPI=300` 会像原服务那样降低分辨率 | 典型报告转换所需的 CPU 只有三分之一，图片也毫无损失；放置分辨率高于 300 DPI 的图片会生成更大的 PDF。 |
| 文档之外的内容 | 链接到 URL 或文件的图片、远程模板或 OLE 链接可能会被获取 | 从不获取或读取：链接的图片会转换为其嵌入的副本，或者什么都不转换 | 文档不能让服务访问其网络中的其他主机，也不能让它等待一个永不响应的主机。 |
| 中文、日文和韩文文本 | 没有相应字体：空白方框或错误的回退字体 | 用 Noto CJK 绘制 | 这些文字在 PDF 中应当可读。 |
| 不是表单上传的请求 | 500，或带有 Spring 错误的 400 | 400 | 这个请求是客户端的错误，而这正是 4xx 所表达的。 |
| 超过大小限制的文档 | 整个上传完成后返回 500 | 413，在读取正文其余部分之前 | 超大的上传是客户端的错误，把它读到结尾只会让服务白白付出代价。 |
| 不是文字处理文档的文件 | 200，LibreOffice 对它生成的任意 PDF，前面带有字节顺序标记的 RTF 会把自己的标记代码当作正文排版出来 | 415 | 一份垃圾内容的 PDF 看起来像是成功；只接受原服务确实能转换的格式。 |
| 损坏的文档 | 500 | 422 | 错在文档，而不在服务。 |
| 需要过多内存的文档 | 没有限制：容器耗尽内存，其中的所有转换都会丢失 | 立即返回 413；实例被替换，其他转换继续进行 | 一份文档不能拖垮整个服务。 |
| zip 炸弹 | 不检查：LibreOffice 在加载时把它解压到磁盘 | 在任何转换之前返回 413 | 几 MB 的文件不能填满数 GB 的磁盘和内存。 |
| 大到无法解码的图片 | 200；解码后超过约 1 GiB 时，图片悄无声息地丢失 | 200，图片被缩小或被占位图替换，并有 `Docx-To-Pdf-Pictures` 头说明此事 | 悄悄丢失内容是缺陷，而拒绝文档会让客户端一无所获。 |
| 永不结束的转换 | 没有响应，实例在它运行期间一直被占用 | 在转换超时后返回 504，实例被替换 | 卡住的文档不能永远占着实例和客户端。 |
| 请求多于实例 | 它们无限等待 | 有界队列；超出队列或超过等待上限时，返回带 `Retry-After` 的 503 | 客户端应当得知服务已饱和，另一个副本可以接手该请求。 |
| 慢速上传 | 上传暂停几毫秒（真实网络就是这样）时返回 500 | 一直等待，无论整个上传需要多久 | 网络中的停顿不是客户端的错误。 |
| 停止发送的客户端 | 连接超时后返回 500 | 两分钟没有一个字节后返回 408，并关闭连接 | 客户端停止了，而这正是 408 所表达的。 |
| 在转换期间断开连接的客户端 | 转换继续运行并占用实例 | 立即停止转换并释放实例 | 没有人在等那份 PDF。 |
| 错误正文 | Spring 的 JSON 文档，或给浏览器的 HTML 页面 | `application/problem+json`，其 `detail` 从不提及转换器 | HTTP 错误的标准格式，不暴露框架的内部细节。 |
| 健康检查与指标 | `/actuator` 下的 Spring Actuator；即使唯一的实例卡在转换中，健康状态仍报告为 up | `/livez`、`/readyz` 和 `/metrics`；没有实例能接收转换时，`/readyz` 为 `503` | 就绪状态应当说明现在能否处理请求，让流量去往能处理的副本。 |
| 平台探测 | 无 | 在单独端口上的 `POST /probe`，默认关闭，为容量规划测量机器 | 规划部署容量需要来自其运行机器的数字。 |
| 转换之外的跨源请求 | 对其他路径或其他方法的预检请求返回 403 `Invalid CORS request`，以及 Spring 的 `Allow` 头 | 两个转换路径之外不做跨源处理；预检请求只列出 `POST` | 浏览器会自行执行跨源规则；那个 403 和那些头是 Spring 的。 |
| 启动失败的实例 | 服务启动失败 | 重新尝试启动，最多三次，期间 `/readyz` 为 `503` | 暂时性故障不应让服务陷入重启循环。 |
| 池大于 CPU 数 | 当池大于 JVM 看到的 CPU 数时，启动可能永久挂起 | 无论池多大，每个实例都能启动 | 永远完不成的启动是缺陷。 |
| Spring、Tomcat 和 JVM 设置 | 读取 | 不读取；它们的替代项见[设置](#设置) | 本服务中没有 Spring、Tomcat 或 JVM。 |
