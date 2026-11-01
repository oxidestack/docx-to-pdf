# 故障排查

<!-- languages:begin -->
[English](../../../guide/troubleshooting.md) · [Русский](../../ru/guide/troubleshooting.md) · **中文** ·
[Español](../../es/guide/troubleshooting.md)
<!-- languages:end -->
<!-- translation of docs/guide/troubleshooting.md: 00faa880a539 -->

出现问题时客户端或运维人员会遇到什么、它意味着什么，以及应该改什么。第一部分为服务返回的每种错误各列一个条目，
标题是其状态码和 `detail`，因此你可以在本页中搜索你收到的响应里的那句话。第二部分涵盖不是错误响应的问题：
pod 因内存被杀死、图片被修改、字体不对、转换缓慢。每个条目都写明要修改的设置、要观察的指标或要查找的日志。
有一个测试保证本页与服务一致：[API 参考](api.md#服务返回的所有错误)中的每种错误在这里都有条目，
下文提到的每个设置、指标和日志都确实存在。

**在哪里查看。** 设置见[配置](configuration.md)；指标在 `GET /metrics` 提供，并列在
[运维：指标](operations.md#指标)中。日志在标准输出上每行一个 JSON 对象；本页以其 message 字段引用日志，
例如 `"message":"the conversion exceeded its timeout"`，因此你可以在日志中搜索它。
处理某个请求期间写下的每一行日志都带有该请求的 `trace_id`。

按响应统计的 Prometheus 计数器：带标签 `http_response_status_code` 的
`http_server_request_duration_seconds_count` 按状态码统计每个请求，`docx_to_pdf_conversions_total`
按结束方式统计转换。

## 错误

### 400 `the body is not multipart/form-data with a boundary`

请求不是表单上传。文档必须以 `multipart/form-data` 发送，放在名为 `document` 的文件部分中；
原始正文（`curl --data-binary @report.docx`）、把文档以 base64 放在 JSON 中，或手写的不带 `boundary=`
的 `Content-Type: multipart/form-data` 头，都会得到这个错误。使用 `curl` 时，请用
`-F document=@report.docx`，让它来写这个头；在代码中，请让 HTTP 库构建 multipart 正文，而不是自己设置
`Content-Type`。用以下指标统计：
`http_server_request_duration_seconds_count{http_response_status_code="400"}`。

### 400 `the multipart body is malformed`

正文声称是 multipart，却无法解析：boundary 与请求头中的不一致，或正文在结束 boundary 之前被截断，
通常是客户端或代理在上传中途放弃了。请检查客户端的库，以及它与服务之间任何会改写正文的组件。它体现在
`http_server_request_duration_seconds_count{http_response_status_code="400"}` 中。

### 400 `the body has no file part named "document"`

表单中没有名为 `document` 的文件部分。要么字段用了其他名称（`file`、`upload`），要么它是一个没有文件名的普通表单字段：
`curl -F document=<report.docx`（用 `<`）把文件内容作为文本发送，会被拒绝；`curl -F document=@report.docx`
（用 `@`）发送的是文件。请重命名字段，或把该部分改为文件部分。它体现在
`http_server_request_duration_seconds_count{http_response_status_code="400"}` 中。

### 404 `no such path; the service converts at POST /pdf and POST /docx-to-pdf`

路径不存在。服务只响应 [API 参考](api.md#路径)中列出的路径：仍指向其他路径的客户端或健康检查，
例如 Spring Boot 的 `/actuator/health`，会得到 `404`。健康检查请使用 `/livez` 和 `/readyz`。用以下指标统计：
`http_server_request_duration_seconds_count{http_response_status_code="404"}`。

### 405 `this path does not take this method; see Allow`

路径存在，但不接受这种方法：对 `/pdf` 发送 `GET`，对 `/readyz` 发送 `POST`。响应的 `Allow`
头列出该路径接受的方法。对转换路径发送 `HEAD` 也会得到这个状态码，这对配置为 `HEAD /pdf`
的负载均衡器健康检查很重要：请改为指向 `/readyz`。它体现在
`http_server_request_duration_seconds_count{http_response_status_code="405"}` 中。

### 408 `no part of the request arrived within the idle limit`

客户端开始了一个请求，然后在 [`DOCX_TO_PDF_IDLE_TIMEOUT`](configuration.md#服务器)（默认 2 分钟）内什么都没发送，
连一个字节都没有。持续发送的慢速上传从不会被切断；这是一个停滞的客户端，或一条在途中断开的连接。
响应之后连接被关闭。服务会记录 `"message":"closing a connection idle past the limit"`。
如果正常的客户端会停滞这么久（移动网络、客户端从慢速存储读取文件时暂停的上传），请调高
`DOCX_TO_PDF_IDLE_TIMEOUT`。

### 413 `the document or the request is over the size limit`

文档大于 [`DOCX_TO_PDF_MAX_DOCUMENT_SIZE`](configuration.md#请求可携带内容的限制)（默认 10 MiB），
或者整个请求大于该值加 64 KiB。如果预期会有这样的文档，请调高该限制；`docx_to_pdf_document_size_bytes`
显示你实际收到的大小。照片很多的文档随着变大，每个实例需要的内存也更多（见下下个条目），
因此如果这些文档随后得到内存 `413`，请同时调高 `DOCX_TO_PDF_INSTANCE_HARD_MEMORY`。

正文不是 problem 文档的 `413`（HTML，或 `Content-Type` 不是 `application/problem+json`）来自服务前面的代理，
而不是服务本身：请调高该代理自己的正文限制，例如 nginx 的 `client_max_body_size`，其默认值为 1 MiB。

当请求携带超过 16 份文档时，平台探测也会返回同样的 `413`。

### 413 `the document's compressed parts inflate past the size limit`

DOCX 或 ODT 是一个 ZIP 压缩包，而这一份的各部分解压后总和超过了
[`DOCX_TO_PDF_MAX_INFLATED_SIZE`](configuration.md#请求可携带内容的限制)（默认 256 MiB）。
“zip 炸弹”就是这个样子，默认大小限制内的普通文档都远远达不到这个值。服务会记录
`"message":"the document's parts inflate past the limit; refused before conversion"`，并附上它发现的大小。
如果正常文档被拒绝，请调高 `DOCX_TO_PDF_MAX_INFLATED_SIZE`；它不能设得低于文档大小限制。

### 413 `the document needs more memory to convert than the service allows`

LibreOffice 在转换这份文档时内存超过了
[`DOCX_TO_PDF_INSTANCE_HARD_MEMORY`](configuration.md#libreoffice-实例池)（默认 2 GiB），
因此它的实例被杀死并替换，其他转换继续进行。该请求不会重试：同一份文档也会杀死下一个实例。日志中会出现
`"message":"the instance passed the hard memory limit during a conversion; killing it, the request fails"` 和
`"message":"the conversion passed the hard memory limit"`，并附上实例达到的内存；对应指标为
`docx_to_pdf_conversions_total{outcome="memory_limit"}`。

当 DOCX 或 ODT 的图片（每张都按缩小之后的样子计算）总和超过硬限制留出的余量时（该限制减去 192 MiB；
带透明度的图片按其位图的二点五倍计算），在任何转换之前就会得到同样的响应：不会动用任何实例，响应只需几毫秒。日志中会出现
`"message":"the document's pictures, after the planned downscale, need more memory than the hard limit leaves; refused before any is decoded"` <!-- markdownlint-disable-line MD013 --><!-- a log message, quoted whole -->
并附上 `held_bytes`、`budget_bytes` 和图片数量。处理办法相同：调高硬限制，或调低
[`DOCX_TO_PDF_PICTURE_MAX_BITMAP`](configuration.md#图片)，让每张超大的图片缩得更小。

占用内存的主要是图片：LibreOffice 在转换时会把每张图片保持为解码状态，对于照片约为文档大小的 12 倍。
要转换这样的文档，请调高 `DOCX_TO_PDF_INSTANCE_HARD_MEMORY`，并按[运维：Kubernetes](operations.md#kubernetes)
中的公式同时调高容器的内存限制：256 MiB + 池大小 × (硬限制 + 256 MiB)。
`docx_to_pdf_instance_memory_peak_bytes` 显示你的文档离限制有多近。

### 415 `the document is not DOCX, DOC, ODT or RTF`

文档的字节不属于[接受的格式](api.md#接受的格式)。文件名和该部分的 `Content-Type` 会被忽略，
因此重命名文件不会改变响应。常见情况：

- 电子表格、演示文稿、PDF 或图片；
- 空文件，常见于客户端在文件写完之前就发送了该部分；
- 由 Web 应用导出的“Word”文件，实际上是带 `.doc` 名称的 HTML 或纯文本；
- 设置了打开密码的 DOCX：Word 把它加密存放在一个不包含 Word 文档流的复合文件中；
- 以 UTF-8 字节顺序标记（BOM）开头的 RTF。

请在文字处理软件中打开该文件，再另存为 DOCX。用以下指标统计：
`docx_to_pdf_conversions_total{outcome="unsupported"}`。

### 422 `the document could not be loaded`

格式是服务接受的，但 LibreOffice 无法打开这份文档：它被截断、损坏或加密了（设置了打开密码的
Word 97-2003 文档）。服务会记录 `"message":"the backend could not load the document"`，
并在 `backend_error` 字段中附上 LibreOffice 自己的错误。重试会得到同样的响应。如果 Word 能打开该文件，
请用 Word 把它另存为 DOCX，再转换那一份。用以下指标统计：
`docx_to_pdf_conversions_total{outcome="not_loaded"}`。

### 500 `the service failed to answer this request`

出现了不是（或不仅是）文档造成的故障。带有该请求 `trace_id` 的 `ERROR` 级别日志会说明是哪一种：

- `"message":"the conversion got no answer"`：LibreOffice 实例在转换期间崩溃，并在另一个实例上自动重试时再次崩溃
  （`docx_to_pdf_conversions_total{outcome="dead_instance"}`）。每次崩溃还会记录
  `"message":"the instance died during a conversion; retrying once on another instance"`。
  让 LibreOffice 崩溃两次的文档很可能还会再让它崩溃；如果许多文档都如此，请查看实例的内存
  （`docx_to_pdf_instance_memory_usage_bytes`）以及内核的内存不足（OOM）消息。
- `"message":"the backend could not export the PDF"`：LibreOffice 打开了文档，但无法写出它的 PDF。
- `"message":"the spooled document could not be read back"` 或
  `"message":"the document's pictures could not be checked"`：服务无法使用其临时目录。
  请检查 [`TMPDIR`](configuration.md#系统)（镜像中为 `/tmp`）是否可写、是否已满；在只读根文件系统上，
  它需要一个自己的卷（见[运维：`/tmp`](operations.md#tmp放在磁盘上还是内存中)）。

### 503 `the service is not ready to convert documents yet`

服务刚刚启动，它的 LibreOffice 实例还没有一个就绪。响应带有 `Retry-After: 30`。
`/readyz` 上的就绪检查会让流量在第一个实例就绪之前远离它，通常在启动后一两秒之内；
如果请求得到了这个响应，说明负载均衡器在 `/readyz` 变为 `200` 之前就在发送流量。
如果服务一直处于这种状态，见[服务无法启动](#服务无法启动或始终未就绪)；
`docx_to_pdf_pool_instances{state="starting"}` 显示仍在启动的实例。

### 503 `no instance came free within the wait limit`

在这个请求于队列中等待期间，所有实例在
[`DOCX_TO_PDF_POOL_WAIT_TIMEOUT`](configuration.md#libreoffice-实例池)（默认 60 秒）内一直忙碌。
服务的容量不足以应对它所承受的负载：`docx_to_pdf_pool_instances{state="idle"}` 一直为 0，
`docx_to_pdf_pool_wait_duration_seconds` 逐渐增长到等待上限；
`docx_to_pdf_pool_wait_duration_seconds_count{outcome="rejected"}` 统计这些响应。
请增加副本，或给每个副本更多 CPU 以及相应的 `DOCX_TO_PDF_POOL_SIZE`（每个核心一个实例；
实例多于核心会让每次转换变慢，而且没有任何增益）。[运维：容量规划](operations.md#容量规划)给出了具体计算。
响应带有 `Retry-After: 10`，在它之后重试的客户端会在突发过去后得到它的 PDF。

当在整个等待期间都有 `DOCX_TO_PDF_POOL_SIZE` 份其他文档在缩小它们的图片时，有图片需要缩小的文档也会得到同样的响应：
这一步在文档获得实例之前运行，并有同样的上限。此时日志中会出现
`"message":"no turn of the pictures step came free within the wait limit"`。

### 503 `too many requests are already waiting for an instance`

已经有 [`DOCX_TO_PDF_POOL_MAX_WAITING`](configuration.md#libreoffice-实例池) 个请求（默认 16 个）在排队，
因此这个请求被立即拒绝，而不是加入队列。原因和处理办法与上一个条目相同：容量不足以应对负载。
调高 `DOCX_TO_PDF_POOL_MAX_WAITING` 只会拉长队列，每个排队的请求都要等得更久。
`docx_to_pdf_pool_wait_duration_seconds_count{outcome="rejected"}` 也统计这些响应。`Retry-After: 10`。

### 503 `the service is shutting down`

服务收到了 `SIGTERM`（滚动更新、缩容、节点驱逐），不再接收转换。尚未到达实例的请求得到 `Retry-After: 10`；
在 [`DOCX_TO_PDF_SHUTDOWN_GRACE`](configuration.md#服务器) 用完时仍在转换的请求得到 `Retry-After: 1`，
服务会记录 `"message":"the grace period ran out; cutting the requests still in flight short"`。
重试的客户端会到达另一个副本。

信号之后仍有请求到达，说明负载均衡器仍在把流量路由到该 pod：在 Kubernetes 上，请加上
[运维：停止](operations.md#停止)中的 `preStop` 休眠。在宽限期结束时被切断的请求，说明宽限期比你的转换短：
请让 `DOCX_TO_PDF_SHUTDOWN_GRACE` 保持其默认值，即转换超时，并给 pod 设置宽限期加约 40 秒的
`terminationGracePeriodSeconds`。平台探测在停机期间被调用时，也会返回同样的 `503`，带有 `Retry-After: 30`。

### 503 `the service is measuring its platform; another replica can convert meanwhile`

有人正在这个副本上运行平台探测，探测会占用所有实例一两分钟。服务在探测开始时记录
`"message":"the pool is reserved for the platform probe; draining it"`，结束时记录
`"message":"the platform probe has given the pool back"`。请探测不接收流量的副本，
并让 [`DOCX_TO_PDF_PROBE_PORT`](configuration.md#服务器) 远离客户端；不测量时请取消设置它。

### 504 `the conversion did not finish within the conversion timeout`

在 [`DOCX_TO_PDF_CONVERSION_TIMEOUT`](configuration.md#libreoffice-实例池)（默认 3 分钟）之后，
LibreOffice 仍在转换，因此它的实例被杀死并替换。日志中会出现
`"message":"the conversion did not end within the timeout; killing its instance"` 和
`"message":"the conversion exceeded its timeout"`；对应指标为 `docx_to_pdf_conversions_total{outcome="timeout"}`。

默认值约为在默认大小限制内测得的最慢文档的三倍，因此普通文档出现 `504`，通常意味着机器比应有的慢：
实例多于 CPU 核心（`DOCX_TO_PDF_POOL_SIZE` 高于 CPU limit，或 CPU limit 对 pod 进行了节流），
于是每次转换只得到一个核心的一部分。请把 `docx_to_pdf_conversion_duration_seconds` 与同一文档在空闲副本上所需的时间进行比较。
对于确实很慢的文档（数百个表格、数千页），请调高 `DOCX_TO_PDF_CONVERSION_TIMEOUT`，并把客户端的读取超时调得比它更高。

## 平台探测的错误

探测，即 [`DOCX_TO_PDF_PROBE_PORT`](configuration.md#服务器) 上的 `POST /probe`，也会返回上面的大小 `413`
和停机 `503`。如何使用它，见[运维：测量平台](operations.md#测量平台容量探测)。

### 400 `rounds, busy, cpus and disk_mib are whole numbers within their bounds`

某个查询参数不是整数，或超过了其上限：`rounds` 最多 100，`busy` 最多 10000，`disk_mib` 最多 65536，
`cpus` 从 1 到 1024。见[探测的参数](api.md#平台探测)。探测只在设置了 `DOCX_TO_PDF_PROBE_PORT` 时监听。

### 400 `the probe takes rounds, busy, disk_mib, cpus and name`

查询中有一个探测不认识的参数，常见的是拼写错误，例如把 `cpus=4` 写成 `cpu=4`。
探测只在设置了 `DOCX_TO_PDF_PROBE_PORT` 时监听。

### 422 `a probe document could not be loaded`

LibreOffice 无法打开发送给探测的某份文档；在每份文档都能转换之前，探测不会测量任何东西。
请先把每份文档通过 `POST /pdf` 转换一遍：在那里得到 [`422`](#422-the-document-could-not-be-loaded)
的那份就是要替换的文档。服务会为它记录 `"message":"the backend could not load the document"`。

### 503 `a probe is already running`

另一个探测占用着这个副本的实例。请等它响应（一两分钟）之后再发送你的探测；响应带有 `Retry-After: 30`。
正在运行的探测结束时，服务会记录 `"message":"the platform probe has given the pool back"`。

### 503 `the conversions under way did not end in time`

探测在占用实例之前，会等待已在运行的转换完成。它们没有在
[`DOCX_TO_PDF_CONVERSION_TIMEOUT`](configuration.md#libreoffice-实例池) 加
`DOCX_TO_PDF_INSTANCE_START_TIMEOUT` 的时间内完成。这个副本正忙于处理流量：请探测一个不接收流量的副本。

## 其他问题

### 服务无法启动，或始终未就绪

**它立即退出，并在标准错误上输出一行**：`docx-to-pdf:` 和一个空格，后跟变量名及其值的问题：
某个设置的值是服务无法使用的（例如不带单位的 `DOCX_TO_PDF_IDLE_TIMEOUT=30`）。
请修正该值；[配置](configuration.md#如何读取值)说明了每个设置接受什么。

**它记录 `"message":"cannot listen"` 并退出**：`DOCX_TO_PDF_PORT` 中的端口已被占用，
或者对非 root 用户而言低于 1024。

**它记录 `"message":"instance start failed"` 或 `"message":"instance start timed out; killed"`**，然后记录
`"message":"retrying a failed first start"`，连续三次失败后记录 `"message":"the pool could not start"`
并退出：LibreOffice 无法启动。最常见的原因是它的临时目录不可写：在只读根文件系统上，`/tmp` 需要一个卷
（见[运维：容器](operations.md#容器)）。在非常慢或负载很重的机器上，实例可能需要比
`DOCX_TO_PDF_INSTANCE_START_TIMEOUT`（30 秒）更长的时间；实例通常在远不到一秒的时间内启动。

**进程在运行，但 `/readyz` 一直是 `503`**：实例仍在启动，或者一直启动失败。
`docx_to_pdf_pool_instances{state="starting"}` 和 `docx_to_pdf_instance_starts_total{reason="failed"}` 会显示是哪一种。
Kubernetes 的启动检查必须允许多于三倍 `DOCX_TO_PDF_INSTANCE_START_TIMEOUT` 的时间；见
[运维：健康检查](operations.md#健康检查)。

### 服务监听的端口与设置的不同，或探测没有启动

同一命名空间中名为 `docx-to-pdf`（或 `docx-to-pdf-probe`）的 Kubernetes Service，会让 Kubernetes
在每个 pod 中把 `DOCX_TO_PDF_PORT`（或 `DOCX_TO_PDF_PROBE_PORT`）设为 `tcp://<ip>:<port>`。
服务会忽略这种形式的值，并在启动时输出警告，就像该变量未设置一样。请显式设置端口，并在 pod 上设置
`enableServiceLinks: false`；见[如何读取值](configuration.md#如何读取值)。

### 客户端在响应到来之前放弃

读取超时短于请求正常所需时间的客户端，会放弃服务仍会响应的请求；此时服务会记录
`"message":"the client went away before the answer; the conversion is cancelled"`，并计入
`docx_to_pdf_conversions_total{outcome="cancelled"}`。一个请求最多可能等待
`DOCX_TO_PDF_POOL_WAIT_TIMEOUT` 来获得实例，然后最多转换 `DOCX_TO_PDF_CONVERSION_TIMEOUT`：
请把客户端以及途经的每个代理的读取超时设为大于两者之和（默认值下为 240 秒），或者调低这两个设置。

### pod 因内存被杀死（OOMKilled）

服务用 `DOCX_TO_PDF_INSTANCE_HARD_MEMORY` 限制每个实例的内存，因此按公式
256 MiB + 池大小 × (`DOCX_TO_PDF_INSTANCE_HARD_MEMORY` + 256 MiB) 确定规格的容器，不会因为某份文档而被杀死。
如果它仍然被杀死了，那么以下情况之一成立：

- 内存限制低于公式的结果，常见原因是调高了 `DOCX_TO_PDF_POOL_SIZE`，却没有相应调高内存限制；
- `DOCX_TO_PDF_INSTANCE_HARD_MEMORY` 为 `0`，这会关闭该限制；
- `/tmp` 是 tmpfs（`medium: Memory` 的 `emptyDir`）：写入其中的内容计入容器的内存，
  因此必须把它的大小加到限制上，或把 `/tmp` 移到磁盘上。

`docx_to_pdf_instance_memory_usage_bytes` 显示每个实例的内存，`process_memory_usage_bytes` 显示服务进程自身的内存。
具体计算见[运维：Kubernetes](operations.md#kubernetes)。

### 实例被频繁替换

每次替换都按其原因计入 `docx_to_pdf_instance_starts_total`。`recycle` 是例行的：实例在完成
`DOCX_TO_PDF_INSTANCE_MAX_CONVERSIONS` 次转换后，或者某次转换后内存仍高于 `DOCX_TO_PDF_INSTANCE_MAX_MEMORY`
时被替换（日志为 `"message":"the instance is above the memory ceiling; recycling it"`），约需三分之一秒。
`memory_limit` 和 `timeout` 伴随 `413` 或 `504` 出现（见上文）。`crash` 表示 LibreOffice 崩溃了；
请求已经重试，只有重试也崩溃时才会出现 `500`。普通文档上持续不断的 `crash` 值得报告，请附上一次发生时的日志。

### PDF 中的图片变小、变模糊，或变成灰色占位图

此时响应会带有一个头，例如 `Docx-To-Pdf-Pictures: downscaled=1, replaced=0`。
解码后位图大于 [`DOCX_TO_PDF_PICTURE_MAX_BITMAP`](configuration.md#图片)（默认 256 MiB，约 9000 万像素的
RGB 彩色）的 PNG 或 JPEG 会在转换前被缩小，否则 LibreOffice 会把它从 PDF 中丢弃。服务会记录
`"message":"a picture's decoded bitmap passes the threshold; downscaled before conversion"`，
并附上图片的大小和缩放倍数，`docx_to_pdf_pictures_total{outcome="downscaled"}` 会统计这些图片。

无法安全缩小的图片会改为被占位图替换，日志为
`"message":"a picture's decoded bitmap passes the threshold; replaced by a placeholder before conversion"`，
原因写在 `why` 字段中（隔行扫描的 PNG、损坏的图片、服务不解码的 JPEG 编码方式），
并计入 `docx_to_pdf_pictures_total{outcome="replaced"}`。请把这样的图片以普通格式、合理的尺寸重新保存
（20000 × 20000 像素的图片远远超出一页能打印的程度），再放回文档中。

调高 `DOCX_TO_PDF_PICTURE_MAX_BITMAP` 会让更大的图片保持原样，但 LibreOffice 在转换时会把每张图片保持为解码状态，
因此请同时调高 `DOCX_TO_PDF_INSTANCE_HARD_MEMORY`。超过约 1 GiB 时，无论如何设置，LibreOffice 都会丢弃图片。

### PDF 中缺少图片，只剩空框

服务只检查 DOCX、DOCM、DOTX、ODT 或 OTT 中的 PNG 和 JPEG 文件。DOC、RTF 或扁平 ODT 中的超大图片，
或其他图片格式的超大图片，会原样交给 LibreOffice，而 LibreOffice 会丢弃解码后位图超过约 1 GiB 的图片。
请把文档另存为 DOCX，或者把图片缩小。当图片检查本身失败时，文档会原样转换；服务会记录
`"message":"the pictures step panicked; the document is converted as it was received"`，并计入
`docx_to_pdf_pictures_total{outcome="failed"}`。

### PDF 比预期的大

默认情况下，每张图片都按它在文档中的样子嵌入，JPEG 逐字节不变，因此以小尺寸放在页面上的照片会保留其完整分辨率。
把 [`DOCX_TO_PDF_PDF_MAX_IMAGE_DPI`](configuration.md#图片) 设为 `300`，即可像 moalhaddar/docx-to-pdf
那样把放置分辨率高于 300 DPI 的图片降下来；对带图片的文档，这会多花 CPU。
`docx_to_pdf_pdf_size_bytes` 显示你响应的 PDF 的大小。

### PDF 中的文字字体不对、重新排版，或显示为方框

文档指定的字体可用时，PDF 使用这些字体；不可用时使用替代字体。镜像带有与 Calibri、Cambria、Arial、
Times New Roman、Courier New 和 Arial Narrow 字宽相同的替代字体，因此使用这些字体的文档能保持换行和分页；
其他任何缺失的字体都会被替换成字宽不同的字体，文字随之重新排版。出现空白方框（即常说的“豆腐块”
或乱码），或者显示成另一种文字的字体中的字形，说明没有任何可用的字体包含该字符。

要查看 PDF 使用了哪些字体，请对它运行 `pdffonts`（来自 Poppler）；要查看某个字体名在镜像中会解析成什么：

```sh
docker run --rm --entrypoint fc-match quay.io/oxidestack/docx-to-pdf:1.2.0 'Segoe UI'
```

解决办法是把字体提供给服务：把你持有许可证的字体文件挂载到 `/usr/share/fonts/custom`，
如[运维：挂载你自己的字体](operations.md#挂载你自己的字体)所述，然后重启服务；字体在实例启动时读取。

### 第一次转换比之后的慢

每个 LibreOffice 实例在第一次转换时才加载部分代码，而不是在启动时，因此实例转换的第一份文档，
明显比之后转换同一份文档要慢；每次替换之后，新实例都要再付出这个代价
（`docx_to_pdf_instance_starts_total{reason="recycle"}`，默认每 `DOCX_TO_PDF_INSTANCE_MAX_CONVERSIONS`
次转换一次）。挂载了字体时，修改后的第一个实例还会在启动时构建字体缓存。请用
`docx_to_pdf_conversion_duration_seconds` 测量，它是 LibreOffice 自身的时间，并在基准测试中排除每个实例的第一次转换。
如果部署后的第一个请求必须很快，请在 `/readyz` 变为 `200` 后，为每个实例发送一份小文档。

### 每次转换都很慢

一份典型的业务报告在一个核心上约一秒即可转换完成。当一切都更慢时：

- **实例多于核心。** `DOCX_TO_PDF_POOL_SIZE` 高于容器可用的 CPU，或 CPU limit 对它进行了节流，
  会让每次转换共用一个核心。请让池大小等于 CPU request。
- **时间花在等待而不是转换上。** 请把请求时间 `http_server_request_duration_seconds` 与
  `docx_to_pdf_conversion_duration_seconds` 和 `docx_to_pdf_pool_wait_duration_seconds` 进行比较：
  差距大说明是排队（需要更多容量），或者是上传和下载（网络）。
- **PDF 中的图片被降低分辨率。** `DOCX_TO_PDF_PDF_MAX_IMAGE_DPI` 会让 LibreOffice 重新编码图片，这会耗费 CPU；
  未设置时，图片按原样复制。

要了解一台机器能做什么，请用平台探测测量它；见[运维：容量规划](operations.md#容量规划)。
