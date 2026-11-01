# 与 Gotenberg 和 moalhaddar/docx-to-pdf 的比较

<!-- languages:begin -->
[English](../../../guide/comparison.md) · [Русский](../../ru/guide/comparison.md) · **中文** ·
[Español](../../es/guide/comparison.md)
<!-- languages:end -->
<!-- translation of docs/guide/comparison.md: c928c7195494 -->

另外有两个服务也通过 HTTP 用 LibreOffice 把办公文档转换为 PDF。**Gotenberg** 是使用最广泛的：
一个围绕 Chromium、LibreOffice 和 PDF 工具构建的通用文档 API。**moalhaddar/docx-to-pdf** 是本服务所保持其接口的那个服务。
本页先说明各自能做什么，再比较速度、内存，以及各自如何对待恶意文档。

本页的每个数字都读自 `make compare` 三次完整比较运行的原始结果，这项比较任何人都可以重新运行
（[重复测量](#重复测量)）；[Benchmarks](https://oxidestack.github.io/docx-to-pdf/benchmarks/)
页面展示了最近一次运行的每个数字以及每次运行的原始结果。站点构建时会根据这些文件写出下面的数字，
遇到手工输入的数字则构建失败。在一次运行内部，数字是该次运行各区块上的平均值 ± σ (n)，
σ 为样本标准差；当两个数字之差大于 2σ = 2·√(σ₁²/n₁ + σ₂²/n₂) 时，认为两者有差异；
范围取自多次运行，每次运行的数字位于范围的一端或其间；数字只来自一次运行时，本页会说明。

## 如何选择

- **Gotenberg**：当工作不只是把 Word 文档转换为 PDF 时——通过 Chromium 转换 HTML、URL 或 Markdown，
  电子表格和演示文稿，合并和拆分 PDF，PDF/A 或 PDF/UA，webhook，或请求上的选项（页面范围、水印、密码）。
  这些它都能做，而且有多年的生产使用经验。
- **oxidestack/docx-to-pdf**：当工作是通过一次 HTTP 调用把 Word 文档转换为 PDF，量大，且文档来自你无法控制的来源时。
  在所测量的工作负载上，在三次交错运行的每一次中，它比 Gotenberg 使用更少的 CPU 和内存，每秒转换更多文档
  （[比值](#速度与内存)）；恶意文档会在某个限制处被拦下，而不是任其运行下去（[案例](#恶意文档)）。
- **moalhaddar/docx-to-pdf**：仅当需要原样保留它今天所做的一切，包括 LibreOffice 7.3 的版面时。
  要从它迁移出来，[迁移指南](migration.md)列出了客户端和部署需要修改的内容。

## 各自能做什么

Gotenberg 这一列已对照其文档及所测量版本的源代码进行核对，该版本见[速度与内存](#速度与内存)。

| | Gotenberg | moalhaddar/docx-to-pdf | oxidestack/docx-to-pdf |
| --- | --- | --- | --- |
| 转换什么 | LibreOffice 能打开的所有格式（文本文档、电子表格、演示文稿、绘图），以及通过 Chromium 转换的 HTML、URL 和 Markdown | Word 文档（DOCX、DOC、ODT、RTF 及其变体）；其他任何内容都不经检查地交给 LibreOffice | 只有 Word 文档：DOCX、DOCM、DOTX、DOC、ODT、OTT、扁平 ODT、RTF（[接受的格式](api.md#接受的格式)） |
| 转换路由 | `/forms/libreoffice/convert`、`/forms/chromium/convert/url`、`/forms/chromium/convert/html`、`/forms/chromium/convert/markdown`，以及每种 Chromium 输入的截图 | `POST /pdf`、`POST /docx-to-pdf` | `POST /pdf`、`POST /docx-to-pdf` |
| 如何确定格式 | 文件名的扩展名 | 不检查 | 根据文档的字节；其他任何内容都在 LibreOffice 看到之前返回 `415` |
| 请求上的选项 | LibreOffice 路由上有很多：页面范围、横向、密码、PDF/A 和 PDF/UA、把多个文件合并为一个、水印、带标签的 PDF | 无 | 无 |
| PDF 工具 | 合并、拆分、PDF/A 和 PDF/UA、元数据、书签、附件、Factur-X、扁平化、优化、水印、印章、旋转、加密（`/forms/pdfengines/…`） | 无 | 无 |
| 异步工作 | webhook；从 URL 获取输入（`downloadFrom`） | 无 | 无 |
| 身份认证、TLS | basic 认证或 OIDC；TLS | 未说明 | 无：请放在你的 ingress 之后 |
| 运行时间过长的转换 | API 在超时（`API_TIMEOUT`，默认 `30s`）时返回 `503`；LibreOffice 继续处理该文档 | 没有超时 | 在 `DOCX_TO_PDF_CONVERSION_TIMEOUT`（默认 `180s`）时被杀死并返回 `504`；实例被替换 |
| 需要过多内存的文档 | 自身没有限制 | 自身没有限制 | 超过 `DOCX_TO_PDF_INSTANCE_HARD_MEMORY`（默认 `2GiB`）时实例被杀死，请求返回 `413`；解压后超过 `DOCX_TO_PDF_MAX_INFLATED_SIZE`（`256MiB`）的 ZIP 在转换前返回 `413` |
| 对 LibreOffice 而言过大的图片 | 被丢弃，返回 `200` | 被丢弃，返回 `200` | 被缩小，并由 `Docx-To-Pdf-Pictures` 头说明 |
| 文档中链接的图片 | 不获取 | 获取，包括公网和私网地址 | 不获取 |
| LibreOffice | 26.8.1，每 `LIBREOFFICE_RESTART_AFTER` 次转换（默认 `10`）后重启 | 7.3 | 26.8，实例在 `DOCX_TO_PDF_INSTANCE_MAX_CONVERSIONS` 次转换（默认 `200`）后或超过其内存上限时被替换 |
| 遥测 | OpenTelemetry trace、指标和日志；Prometheus | 无 | OpenTelemetry trace、指标和日志；Prometheus `GET /metrics` |
| 镜像 | amd64、arm64 以及另外三种架构 | 仅 amd64 | amd64 和 arm64 |

oxidestack/docx-to-pdf 这一列中的限制是它的默认值；每一项都是一个设置（[配置](configuration.md)）。
链接图片这一行和各限制行由下面的极端测试（torture test）测得；Gotenberg 和 moalhaddar/docx-to-pdf
的则是各自出厂时的状态。

## 速度与内存

测量的数字（运行条件和所测版本、比值，以及 Gotenberg 领先或两者在 2σ 下没有差异的各行）
由测量结果生成、仅以英文写出：请在 [Benchmarks](https://oxidestack.github.io/docx-to-pdf/benchmarks/) 页面和本页的英文原文中查看。

## 恶意文档

每个恶意文档案例在各个服务上的响应、耗时和内存峰值，以及长时间运行测试（soak）的结果，
同样只在 [Benchmarks](https://oxidestack.github.io/docx-to-pdf/benchmarks/) 页面和本页的英文原文中给出。

## 这些数字无法说明什么

- **一个 vCPU、一个 LibreOffice、一种文档组合。** Gotenberg 每个容器运行一个 LibreOffice，
  因此这是可以直接比较的单元；更大的池与多个 Gotenberg 容器之间的对比没有测量过。
- **一种主机。** 绝对的时间或 CPU 数字会在同一规格的不同虚拟机之间波动，而在一次交错运行内部测得的比值波动较小，
  但仍大于其自身的标准误差：因此采用多次运行的范围。每次运行都在 amd64 上：moalhaddar/docx-to-pdf
  没有 arm64 镜像，结果中也没有 arm64 的运行。
- **每次运行一个镜像。** 最近一次运行测量的是上一节所指明的那一个 oxidestack/docx-to-pdf 镜像，
  更早的运行测量的是它之前的代码；之后的版本由之后的运行来测量。
- **每个恶意文档每方只运行一次。** 响应会重复出现，因为它们由限制决定；时间或峰值则是单次测量。
- **数千次转换中的内存。** [Benchmarks](https://oxidestack.github.io/docx-to-pdf/benchmarks/#soaks)
  上的长时间运行测试每方只有一次，不足以确定某个服务的内存是否增长：本页陈述 oxidestack/docx-to-pdf
  的那次测试测得的水平，对增长不作任何断言。

面对恶意文档时 Gotenberg 做得同样好或更好的地方：它也不建立任何网络连接，它能转换 `tdf38575`，
而且对停滞的上传放弃得更早。不同之处：它在超时时给出的响应并不会停止 LibreOffice，后者会继续处理该文档，
而排在它后面的请求则被拒绝。

## 渲染

Gotenberg 的渲染从未测量过：没有任何来自 Gotenberg 的 PDF 与 oxidestack/docx-to-pdf 的 PDF 比较过，
因此本页对此不作任何断言。两者都运行 LibreOffice 26.8，因此预计差异会出现在字体和导出选项上，
而不是版面上，而这一预期尚未经过测量。与 moalhaddar/docx-to-pdf 相比，有差异的文档，
其差异出现在 LibreOffice 7.3 和 26.8 排版页面不同的地方；[迁移指南](migration.md)说明了迁移的部署应检查什么。

## 重复测量

`make compare` 以全新状态启动每个服务，分区块运行速度测试单元（每个区块以打乱的顺序把每个服务运行一次，
使主机的漂移对每个服务的影响相同），然后运行恶意文档和长时间运行测试，并打印两张表。只需要 Docker。

```sh
make compare QUICK=1   # minutes on a laptop: one short block and four hostile documents
make compare           # the full run, for a quiet VM: hours
```

快速模式表明命令可以正常工作；它的数字不能与上面的数字比较。完整模式会写出一份 `results.json`，
与本页所读取的那份类似，在同类主机上可比较；moalhaddar/docx-to-pdf 只能在 amd64 上运行。
它运行什么、不能说明什么，见 [examples/compare](../../../../examples/compare/README.md)。
