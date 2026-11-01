# 升级

<!-- languages:begin -->
[English](../../../guide/upgrading.md) · [Русский](../../ru/guide/upgrading.md) · **中文** ·
[Español](../../es/guide/upgrading.md)
<!-- languages:end -->
<!-- translation of docs/guide/upgrading.md: abb467a9071c -->

如何把部署从当前运行的版本迁移到更新的版本。找到比你的版本更新的每个版本的章节，从最旧的开始依次应用：
从 1.0.0 升级到 1.2.0，就是先应用 1.1.0 的章节，再应用 1.2.0 的章节。不需要任何改动的版本会明确说明。
然后一步[升级](#如何升级)到最新版本：永远不需要先安装中间版本。

每个章节列出该版本对部署或客户端的要求：**Action required** 下是需要在升级之前或升级时做的改动；
**Deprecated** 下是仍然可用、但会在以后某个版本停止的内容；**Removed** 下是已经停止的内容。
每个版本的其他所有改动见 [changelog](https://github.com/oxidestack/docx-to-pdf/blob/main/CHANGELOG.md)。
只有正式版本才有章节：候选版本（`-rc.N`）在其 GitHub release 中说明，它带来的改动写在发布它的正式版本之下。

你所运行版本的文档也在本站上：每页顶部的版本选择器可在版本之间切换，旧版本的页面会链接到最新版本中的同一页面。

## 如何升级

docx-to-pdf 在两次转换之间不保存任何东西：它没有数据库，也没有需要迁移的文件，因此升级只是替换容器，
回滚也是用旧版本执行同样的步骤。

1. 阅读下面每个比你的版本更新的版本章节，并把其中的改动应用到你的配置、chart 值或客户端。
2. 如果你会校验版本，请校验新的镜像或 chart
   （[验证版本](installation.md#验证版本)）。
3. 替换容器：
   - `docker run` 或 Docker Compose：修改镜像的标签（`quay.io/oxidestack/docx-to-pdf:<version>`），然后重新启动
     （`docker compose up -d`）。
   - Helm：执行与[安装](installation.md#使用-helm)中相同的 `helm upgrade --install`，
     使用新的 `--version` 和你自己的 values 文件。chart 会先启动新 pod 再停止旧 pod；新 pod 在 `/readyz`
     返回 `200` 后开始接收转换，旧 pod 会先完成手头的转换再停止
     （[运维](operations.md#停止)）。
4. 观察最初几分钟：`/readyz`、响应的状态码和转换指标
   （[运维](operations.md#遥测)）。要回滚，请用原来的版本执行第 3 步。

## 升级到 1.2.1

无需任何改动：镜像和 chart 可以从 1.2.0 原地升级。

## 升级到 1.2.0

无需任何改动：镜像和 chart 可以从 1.1.0 原地升级。

## 升级到 1.1.0

无需任何改动：镜像和 chart 可以从 1.0.0 原地升级。
