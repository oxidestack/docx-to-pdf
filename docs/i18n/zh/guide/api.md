# API 参考

<!-- languages:begin -->
[English](../../../guide/api.md) · [Русский](../../ru/guide/api.md) · **中文** ·
[Español](../../es/guide/api.md)
<!-- languages:end -->
<!-- translation of docs/guide/api.md: f98dcbb15dbb -->

服务每个请求把一份办公文档转换为 PDF。本页列出它响应的每个路径、请求携带的内容、它可能给出的每种响应，
以及每种错误。有一个测试会把错误表格与代码可能给出的响应进行比对，并把下面每个 `curl` 示例对服务重放，
将其响应与所示的响应进行比较，因此本页不会与服务脱节。

示例假定服务运行在 `localhost:8080`，即其默认端口。响应按 `curl -i` 打印的样子展示，只保留相关的响应头；
写在 `<angle brackets>`（尖括号）中的值会变化。

## 路径

| 方法和路径 | 作用 |
| --- | --- |
| `POST /pdf` | 把请求中的文档转换为 PDF。 |
| `POST /docx-to-pdf` | 同样的转换，用第二个名称；两者可以互换。 |
| `GET /livez` | 存活：只要进程在提供 HTTP 服务就返回 `200`。 |
| `GET /readyz` | 就绪：服务能够转换后返回 `200`，在此之前以及从停机开始时返回 `503`。 |
| `GET /metrics` | Prometheus 指标，采用文本展示格式。 |
| `POST /probe` | 平台探测，在单独的端口上，且仅在开启时可用（见[平台探测](#平台探测)）。 |

每个响应 `GET` 的路径也响应 `HEAD`。其他任何路径都返回 `404`；已知路径使用其他方法时返回 `405`，
并带有一个 `Allow` 头，列出该路径接受的方法。

## 转换文档

```sh
curl -sS -D - -o report.pdf -F document=@report.docx http://localhost:8080/pdf
```

```http
HTTP/1.1 200 OK
content-type: application/pdf
content-length: <bytes>
```

请求是 `multipart/form-data`，文档放在一个**名为 `document` 的文件部分**中：即其 `Content-Disposition`
带有 `filename`（或 `filename*`）的部分。`curl -F document=@<file>` 发送的正是这样的部分。

- **只转换第一个名为 `document` 的文件部分。** 之后的同名部分以及所有其他字段都会被读过并忽略。
  名为 `document` 的普通表单字段（没有文件名的字段）不是文档。
- **文件名和该部分的 `Content-Type` 不起任何作用。** 格式由文档的字节决定，因此名为 `scan.pdf` 的 DOCX
  能够转换，而名为 `letter.docx` 的图片会被拒绝。任何语言的文件名都被接受，且永远不会进入文件系统。
- **正文可以用 `Content-Length` 或分块方式发送。** 声明的 `Content-Length` 超过限制的请求，
  会在读取任何正文之前以 `413` 拒绝，因此发送 `Expect: 100-continue` 的客户端会得到 `413` 而不是
  `100 Continue`，根本不会上传文件。
- **文档默认最大可为 10 MiB**
  （[`DOCX_TO_PDF_MAX_DOCUMENT_SIZE`](configuration.md#请求可携带内容的限制)），整个请求可以再多 64 KiB，
  用于 multipart 封装和其他字段。

对已转换文档的响应是 `200`，带有 `Content-Type: application/pdf` 和 `Content-Length`，正文就是 PDF。
请求中的其他任何内容都不会改变 PDF：没有任何选项。

### 接受的格式

| 格式 | 如何识别 |
| --- | --- |
| DOCX（Word 2007 及以后）、DOCM（启用宏；宏从不运行）、DOTX（Word 模板） | 一个 ZIP 压缩包，其 `[Content_Types].xml` 声明了该格式的 Word 主文档类型。 |
| ODT（OpenDocument 文本）、OTT（其模板） | 一个 ZIP 压缩包，其 `mimetype` 条目为 `application/vnd.oasis.opendocument.text` 或 `…text-template`。 |
| 扁平 ODT（`.fodt`，单个 XML 文件形式的 OpenDocument 文本） | 一个 XML 文档，其根元素为 `office:document`，带有 `office:mimetype="application/vnd.oasis.opendocument.text"`，且该属性在前 4000 字节内结束。 |
| DOC（Word 97-2003） | 一个复合文件（OLE），其根目录包含名为 `WordDocument` 的流。 |
| RTF | 字节以 `{\rtf` 开头。前面带有 UTF-8 字节顺序标记（BOM）的 RTF 会被拒绝。 |

其他一切都会在任何转换开始之前得到 `415`：电子表格、演示文稿、PDF、图片、纯文本、HTML、空文件。
只有标识格式的结构必须完好：一个 `[Content_Types].xml` 幸存下来的截断 DOCX，或一个文本流已损坏的 DOC，
都会被接受，由 LibreOffice 决定能否打开它。无法打开时，响应为 `422`。

### 转换前被修改的图片

LibreOffice 会悄悄丢弃解码后位图超过约 1 GiB 的图片，在 PDF 中留下一个空框。为防止这种情况，
DOCX、DOCM、DOTX、ODT 或 OTT 中解码后位图大于
[`DOCX_TO_PDF_PICTURE_MAX_BITMAP`](configuration.md#图片)（默认 256 MiB）的 PNG 或 JPEG 会在转换前被缩小，
无法安全解码时则用占位图替换。发生这种情况时，`200` 响应会带有一个说明此事的头：

```http
Docx-To-Pdf-Pictures: downscaled=1, replaced=0
```

没有图片被修改时，没有这个头。

在解码任何图片之前，服务会把 LibreOffice 为文档中的 PNG 和 JPEG 图片需要占用的内存加起来，
每张图片都按缩小之后的样子计算，带透明度的图片按其位图的二点五倍计算。当总和超过
[`DOCX_TO_PDF_INSTANCE_HARD_MEMORY`](configuration.md#libreoffice-实例池) 留出的余量（该限制减去
192 MiB）时，响应立即是内存 `413`，而不必先花一分钟缩小图片，
之后转换却仍然撑不过去。
需要缩小图片的文档轮流进行：同时最多
[`DOCX_TO_PDF_POOL_SIZE`](configuration.md#libreoffice-实例池) 份，其余的等待。

### 从浏览器调用

两个转换路径响应来自任何源的跨源请求：对带有 `Origin` 头的请求的响应带有
`Access-Control-Allow-Origin: *`，并向页面暴露 `Docx-To-Pdf-Pictures`。预检请求以 `200` 响应：

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

预检请求要求的头无论是什么都会被允许，浏览器可以把响应缓存 30 分钟。无论预检请求询问的是哪种方法，
响应总是只列出 `POST`。其他路径没有跨源头。

## 错误

每个错误都是一份 problem 文档（[RFC 9457](https://www.rfc-editor.org/rfc/rfc9457)），
`Content-Type: application/problem+json`，并且恰好有四个字段：

| 字段 | 值 |
| --- | --- |
| `type` | 总是 `about:blank`：状态码说明了错误的类型。 |
| `title` | 状态码的原因短语，例如 `Unsupported Media Type`。 |
| `status` | 状态码，以数字表示。 |
| `detail` | 针对该情况的固定句子，见下表。它从不包含文件名、路径、文档内容或堆栈跟踪，因此可以放心展示或记录。 |

```sh
curl -sS -i -F document=@photo.png http://localhost:8080/pdf
```

```http
HTTP/1.1 415 Unsupported Media Type
content-type: application/problem+json

{"type":"about:blank","title":"Unsupported Media Type","status":415,"detail":"the document is not DOCX, DOC, ODT or RTF"}
```

客户端应根据状态码做决定。`detail` 告诉人们是哪种情况。

### 服务返回的所有错误

| 状态码 | `detail` | 何时 |
| --- | --- | --- |
| 400 | `the body is not multipart/form-data with a boundary` | 请求没有 `Content-Type`、是其他类型，或者是没有 boundary 的 multipart 类型。 |
| 400 | `the multipart body is malformed` | multipart 正文无法解析，或在结束 boundary 之前就结束了。 |
| 400 | `the body has no file part named "document"` | 没有任何部分是名为 `document` 的文件部分。 |
| 404 | `no such path; the service converts at POST /pdf and POST /docx-to-pdf` | 路径不是[路径](#路径)中列出的路径之一。 |
| 405 | `this path does not take this method; see Allow` | 路径存在，但不接受这种方法；`Allow` 列出它接受的方法。对转换路径发送 `HEAD` 会得到这个状态码，且没有正文。 |
| 408 | `no part of the request arrived within the idle limit` | 在 [`DOCX_TO_PDF_IDLE_TIMEOUT`](configuration.md#服务器)（默认 2 分钟）内，请求的头部或正文没有到达任何一个字节。响应带有 `Connection: close`，连接被关闭。 |
| 413 | `the document or the request is over the size limit` | 文档大于 [`DOCX_TO_PDF_MAX_DOCUMENT_SIZE`](configuration.md#请求可携带内容的限制)，或请求大于该值加 64 KiB。一旦超过限制立即响应：正文的其余部分不会被读取。 |
| 413 | `the document's compressed parts inflate past the size limit` | DOCX、DOCM、DOTX、ODT 或 OTT 的各部分解压后总和超过 [`DOCX_TO_PDF_MAX_INFLATED_SIZE`](configuration.md#请求可携带内容的限制)：即“zip 炸弹”。在任何转换开始之前响应。 |
| 413 | `the document needs more memory to convert than the service allows` | 转换占用的内存超过 [`DOCX_TO_PDF_INSTANCE_HARD_MEMORY`](configuration.md#libreoffice-实例池) 而被终止；或者在任何转换之前，文档图片的总和超过了该限制留出的余量（[转换前被修改的图片](#转换前被修改的图片)）。用同一文档重试很可能会碰到同样的限制。 |
| 415 | `the document is not DOCX, DOC, ODT or RTF` | 文档不属于[接受的格式](#接受的格式)，或为空。 |
| 422 | `the document could not be loaded` | 格式被接受，但 LibreOffice 无法打开该文档：它损坏到无法读过去的程度。 |
| 500 | `the service failed to answer this request` | 其他任何情况：LibreOffice 实例在转换期间崩溃，并在自动重试时再次崩溃；LibreOffice 无法写出 PDF；或服务无法把文档写入其临时目录。服务会记录原因。 |
| 503 | `the service is not ready to convert documents yet` | 服务刚刚启动，其 LibreOffice 实例尚未就绪。`Retry-After: 30`。 |
| 503 | `no instance came free within the wait limit` | 在 [`DOCX_TO_PDF_POOL_WAIT_TIMEOUT`](configuration.md#libreoffice-实例池) 期间所有实例一直忙碌；或者，对于有图片需要缩小的文档，在这整段时间里有 `DOCX_TO_PDF_POOL_SIZE` 份其他文档在缩小它们的图片。`Retry-After: 10`。 |
| 503 | `too many requests are already waiting for an instance` | 已有 [`DOCX_TO_PDF_POOL_MAX_WAITING`](configuration.md#libreoffice-实例池) 个请求在排队。立即响应。`Retry-After: 10`。 |
| 503 | `the service is shutting down` | 服务收到了 `SIGTERM`。尚未到达实例的请求得到 `Retry-After: 10`；在停机宽限期结束时仍在运行的请求得到 `Retry-After: 1`。另一个副本可以接手。 |
| 503 | `the service is measuring its platform; another replica can convert meanwhile` | 平台探测占用了所有实例。`Retry-After: 10`。 |
| 504 | `the conversion did not finish within the conversion timeout` | 转换运行时间超过 [`DOCX_TO_PDF_CONVERSION_TIMEOUT`](configuration.md#libreoffice-实例池)（默认 3 分钟），被终止。 |

每个 `503` 都带有 `Retry-After`，单位为整秒。当 LibreOffice 实例在转换中崩溃时，服务会自行重试一次转换，
因此 `500` 已经是重试过的结果。用同一文档重试时，`422`、`413` 和 `415` 不会改变；`503` 和 `504` 可能会。

转换路径的错误带有与其 `200` 相同的跨源头，因此浏览器中的页面可以读取它们。

### 示例

文件字段使用了其他名称的表单：

```sh
curl -sS -i -F file=@report.docx http://localhost:8080/pdf
```

```http
HTTP/1.1 400 Bad Request
content-type: application/problem+json

{"type":"about:blank","title":"Bad Request","status":400,"detail":"the body has no file part named \"document\""}
```

超过大小限制的文档（这里是 11 MiB，默认限制为 10 MiB）：

```sh
curl -sS -i -F document=@scan.docx http://localhost:8080/pdf
```

```http
HTTP/1.1 413 Payload Too Large
content-type: application/problem+json

{"type":"about:blank","title":"Payload Too Large","status":413,"detail":"the document or the request is over the size limit"}
```

一份几百 KiB 的 DOCX，其各部分解压后为 257 MiB，超过了 256 MiB 的默认限制：

```sh
curl -sS -i -F document=@bomb.docx http://localhost:8080/pdf
```

```http
HTTP/1.1 413 Payload Too Large
content-type: application/problem+json

{"type":"about:blank","title":"Payload Too Large","status":413,"detail":"the document's compressed parts inflate past the size limit"}
```

一份格式被接受、但 LibreOffice 无法打开的文档：

```sh
curl -sS -i -F document=@damaged.rtf http://localhost:8080/docx-to-pdf
```

```http
HTTP/1.1 422 Unprocessable Entity
content-type: application/problem+json

{"type":"about:blank","title":"Unprocessable Entity","status":422,"detail":"the document could not be loaded"}
```

用 `GET` 调用转换路径：

```sh
curl -sS -i http://localhost:8080/pdf
```

```http
HTTP/1.1 405 Method Not Allowed
allow: POST
content-type: application/problem+json

{"type":"about:blank","title":"Method Not Allowed","status":405,"detail":"this path does not take this method; see Allow"}
```

不存在的路径：

```sh
curl -sS -i http://localhost:8080/actuator/health
```

```http
HTTP/1.1 404 Not Found
content-type: application/problem+json

{"type":"about:blank","title":"Not Found","status":404,"detail":"no such path; the service converts at POST /pdf and POST /docx-to-pdf"}
```

## 健康检查

```sh
curl -sS -i http://localhost:8080/livez
```

```http
HTTP/1.1 200 OK
content-type: text/plain; charset=utf-8

ok
```

只要进程在提供 HTTP 服务，无论 LibreOffice 处于什么状态，`/livez` 都返回 `200 ok`。把它用作存活检查：
失败意味着进程应当重启。

```sh
curl -sS -i http://localhost:8080/readyz
```

```http
HTTP/1.1 200 OK
content-type: text/plain; charset=utf-8

ok
```

LibreOffice 实例启动后，`/readyz` 返回 `200 ok`；在此之前、平台探测运行期间，以及从服务收到 `SIGTERM`
的那一刻起，它返回 `503`，正文为纯文本 `not ready`。把它用作就绪检查，使负载均衡器只把转换发送给能够接收的副本。
这两种响应都是纯文本，而不是 problem 文档。如何把它们接入容器平台，以及启动检查应允许多长时间，
见[运维：健康检查](operations.md#健康检查)。

## 指标

```sh
curl -sS -i http://localhost:8080/metrics
```

```http
HTTP/1.1 200 OK
content-type: text/plain; version=0.0.4; charset=utf-8

<the metrics>
```

`/metrics` 以 Prometheus 文本格式提供服务的指标：按路径和状态码统计的 HTTP 请求、按结果统计的转换及其耗时、
文档和 PDF 的大小、实例池以及进程。它始终开启，无需任何设置。同样的指标连同 trace 和日志，
也可以通过 OpenTelemetry 导出；见[日志与遥测](configuration.md#日志与遥测)。每个指标、它的 Prometheus
名称及其含义，列在[运维：指标](operations.md#指标)中。

## 平台探测

`POST /probe` 测量这台机器的转换速度，用于规划部署容量。它只在设置了
[`DOCX_TO_PDF_PROBE_PORT`](configuration.md#服务器) 时存在，且只在该端口上，从不在转换端口上：
运行期间它占用所有 LibreOffice 实例，因此转换会得到 `503`，`/readyz` 报告未就绪。它需要一两分钟。
其报告的含义以及如何把它换算成部署的规模，见[运维：测量平台](operations.md#测量平台容量探测)。

请求是 `multipart/form-data`，带有 1 到 16 个名为 `document` 的文件部分，即用于测量的文档，
每份都在文档大小限制之内。可选的查询参数：

| 参数 | 默认值 | 含义 |
| --- | --- | --- |
| `rounds` | `5` | 每份文档逐一转换多少次（1 到 100）。 |
| `busy` | 120 + 3 × 池大小 | 在所有实例都忙的情况下运行多少次转换（最多 10000）。 |
| `disk_mib` | `1024` | 为测量磁盘写入多少 MiB（最多 65536）；`0` 跳过。 |
| `cpus` | 池大小 | 响应中的公式按多少个 CPU 计算（1 到 1024）。 |
| `name` | CPU 型号和数量 | 响应中平台的名称。 |

响应是 `200`，带有一份 JSON 报告。它的错误是 problem 文档，同上；除了转换路径中适用于它的那些错误外，
探测还有以下错误：

| 状态码 | `detail` | 何时 |
| --- | --- | --- |
| 400 | `rounds, busy, cpus and disk_mib are whole numbers within their bounds` | 某个参数不是整数，或超过了其上限。 |
| 400 | `the probe takes rounds, busy, disk_mib, cpus and name` | 查询中有其他参数。 |
| 413 | `the document or the request is over the size limit` | 某份文档超过大小限制，或请求携带了超过 16 份文档。 |
| 422 | `a probe document could not be loaded` | LibreOffice 无法打开其中一份文档。 |
| 503 | `a probe is already running` | 另一个探测占用着实例。`Retry-After: 30`。 |
| 503 | `the conversions under way did not end in time` | 探测在占用实例之前，会等待已在运行的转换完成；它们没有在转换超时加实例启动超时的时间内完成。`Retry-After: 30`。 |
| 503 | `the service is shutting down` | 服务收到了 `SIGTERM`。`Retry-After: 30`。 |
