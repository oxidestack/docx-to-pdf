# Установка

<!-- languages:begin -->
[English](../../../guide/installation.md) · **Русский** · [中文](../../zh/guide/installation.md) ·
[Español](../../es/guide/installation.md)
<!-- languages:end -->
<!-- translation of docs/guide/installation.md: 5e99d608a7ce -->

Как запустить сервис: через `docker run`, чтобы попробовать, через Docker Compose на одной машине и в Kubernetes с
чартом Helm. Каждый способ заканчивается тем, что сервис отвечает на `POST /pdf` на порту `8080`; что ему отправлять,
описано в [справочнике API](api.md). Тест сверяет эту страницу с файлами, которые она показывает: файлы compose ниже —
те, что лежат в [`examples/compose/`](../../../../examples/compose/), тест запускает каждый из них и конвертирует через
него документ, а команды `helm` устанавливают чарт так же, как это делает собственный тест чарта.

> **Не выставляйте сервис в публичный интернет.** У него нет ни аутентификации, ни ограничения частоты запросов:
> любой, кто может достучаться до его порта, может заставить его конвертировать что угодно, а каждая конвертация
> занимает ядро CPU и до нескольких GiB памяти на всё время, пока она идёт. Запускайте его в частной сети, где его
> вызывают только ваши собственные приложения, а всё, что обращено к пользователям, ставьте перед этими приложениями,
> а не перед сервисом.

## Docker

```sh
docker run --rm -p 8080:8080 quay.io/oxidestack/docx-to-pdf:1.2.0
```

Образ и чарт публикуются в quay.io. `ghcr.io/oxidestack/docx-to-pdf` и
`oci://ghcr.io/oxidestack/charts/docx-to-pdf` — их зеркала с теми же дайджестами и подписями, для сети, из которой
quay.io недоступен.

`-p 8080:8080` публикует порт на всех сетевых интерфейсах машины, так что его могут вызывать и другие машины в её
сети. Чтобы оставить его доступным только самой машине, привяжите порт к `127.0.0.1`:

```sh
docker run --rm -p 127.0.0.1:8080:8080 quay.io/oxidestack/docx-to-pdf:1.2.0
```

Когда сервис запустится (`/readyz` отвечает `200`, обычно через несколько секунд), сконвертируйте документ:

```sh
curl -F document=@report.docx http://localhost:8080/pdf -o report.pdf
```

Образ собран для `linux/amd64` и `linux/arm64`, поэтому он работает нативно и на Mac с Apple Silicon, и на сервере
x86. Он рассчитан на запуск в закрытом виде — пользователь не root, корневая файловая система только для чтения, без
capabilities Linux, — и раздел [Эксплуатация](operations.md#контейнер) приводит строку `docker run`, которая так его
запускает.

## Docker Compose

`examples/compose/compose.yaml` запускает сервис в закрытом виде, на `127.0.0.1:8080`, с ограничением памяти, которое
нужно для одной конвертации за раз:

```yaml
# docx-to-pdf on this machine only: http://127.0.0.1:8080 (docs/guide/installation.md).
services:
  docx-to-pdf:
    image: quay.io/oxidestack/docx-to-pdf:1.2.0
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

Настройки — это переменные окружения, перечисленные в разделе [Конфигурация](configuration.md). Чтобы конвертировать
больше документов одновременно, увеличьте `DOCX_TO_PDF_POOL_SIZE` и вместе с ним ограничение памяти
([Ресурсы](#ресурсы)). `docker compose down -v` останавливает сервис и удаляет его том `/tmp`.

### С телеметрией

Сервис отправляет трассы, логи и метрики по OTLP коллектору, который указан в `OTEL_EXPORTER_OTLP_ENDPOINT`. Здесь их
принимает OpenTelemetry Collector и печатает по строке на каждый пакет, `examples/compose/telemetry/compose.yaml`:

```yaml
# docx-to-pdf sending its traces, logs and metrics to an OpenTelemetry Collector (docs/guide/installation.md).
services:
  docx-to-pdf:
    image: quay.io/oxidestack/docx-to-pdf:1.2.0
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

и его конфигурация, `examples/compose/telemetry/otel-collector.yaml`:

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

Замените экспортёр `debug` на экспортёр своего бэкенда или направьте `OTEL_EXPORTER_OTLP_ENDPOINT` на коллектор,
который у вас уже работает. Без всякого коллектора `/metrics` по-прежнему отдаёт метрики, которые собирает Prometheus.
Что сообщает сервис, описано в разделе [Эксплуатация](operations.md#телеметрия).

## Kubernetes

### С Helm

Чарт опубликован в `oci://quay.io/oxidestack/charts`. Со значениями по умолчанию он запускает один под, который
конвертирует один документ за раз:

```sh
helm install docx-to-pdf oci://quay.io/oxidestack/charts/docx-to-pdf --version 1.2.0 \
  --namespace docx-to-pdf --create-namespace
```

Значения, которые развёртывание обычно меняет, — в собственном файле:

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
helm upgrade --install docx-to-pdf oci://quay.io/oxidestack/charts/docx-to-pdf --version 1.2.0 \
  --namespace docx-to-pdf --create-namespace --values values.yaml
```

| Значение | По умолчанию | Что задаёт |
| --- | --- | --- |
| `replicaCount` | `1` | Поды. |
| `poolSize` | `1` | `DOCX_TO_PDF_POOL_SIZE`, число параллельных конвертаций на под. Запрос CPU — то же число, а запрос и ограничение памяти вычисляются из него ([Ресурсы](#ресурсы)). |
| `instanceHardMemoryMiB` | `2048` | `DOCX_TO_PDF_INSTANCE_HARD_MEMORY`: память, которую может занять одна конвертация, прежде чем её остановят с `413`. Ограничение памяти вычисляется из него. |
| `otel.endpoint` | пусто: без экспорта | `OTEL_EXPORTER_OTLP_ENDPOINT`, коллектор. |
| `config` | `{}` | Любая другая настройка из раздела [Конфигурация](configuration.md), в виде `NAME: value`. |
| `tmp.medium` | на диске | `Memory` помещает `/tmp` в tmpfs, размер которой, `tmp.sizeMiB`, добавляется к ограничению памяти. |
| `fonts` | нет | Том со шрифтами, на которые у вас есть лицензия ([Эксплуатация](operations.md#подключение-своих-шрифтов)). |
| `serviceMonitor.enabled` | `false` | ServiceMonitor prometheus-operator для `/metrics`. |
| `networkPolicy.enabled` | `true` | NetworkPolicy: входящий трафик на порт 8080, исходящий только к DNS и коллектору ([Сеть](#сеть-сервис-ничего-не-загружает)). |

Все значения перечислены в README чарта, и чарт отклоняет значение, которого не знает. Service релиза —
`docx-to-pdf.docx-to-pdf.svc.cluster.local:8080`, типа `ClusterIP`: он доступен только изнутри кластера. `helm test
docx-to-pdf --namespace docx-to-pdf` конвертирует через него небольшой документ.

### Без Helm

Чтобы развёртывать обычными манифестами — `kubectl apply`, Kustomize или инструментом GitOps, который применяет
файлы, — один раз отрендерите чарт с теми же значениями и сохраните результат:

```sh
helm template docx-to-pdf oci://quay.io/oxidestack/charts/docx-to-pdf --version 1.2.0 \
  --namespace docx-to-pdf --values values.yaml --skip-tests > docx-to-pdf.yaml
kubectl create namespace docx-to-pdf
kubectl apply --namespace docx-to-pdf -f docx-to-pdf.yaml
```

Результат — это Deployment, Service, PodDisruptionBudget и NetworkPolicy. В разделе
[Эксплуатация](operations.md#kubernetes) тот же Deployment написан вручную, с обоснованием каждого числа в нём.

### Сеть: сервис ничего не загружает

Документ может ссылаться на содержимое вне себя: изображение, связанное с URL или файлом, удалённый шаблон, связь
OLE. Сервис никогда ничего из этого не загружает и не читает. Связанное изображение конвертируется в копию, встроенную
в документ, или ни во что, поэтому конвертация не открывает ни одного соединения и не ждёт ни одного хоста, что бы ни
говорил документ. Поду не нужен исходящий трафик, кроме DNS и коллектора телеметрии, и NetworkPolicy чарта,
включённая по умолчанию, ограничивает его этим, чтобы ничто, работающее в конвертере, не могло достучаться до
остального кластера. Она выбирает поды релиза и разрешает:

- входящий трафик только на порт сервиса, 8080, откуда угодно или от пиров, которые перечислены в
  `networkPolicy.ingressFrom`;
- исходящий трафик к DNS, UDP и TCP 53, к подам DNS кластера (`k8s-app: kube-dns` в `kube-system`) или к пирам,
  которые перечислены в `networkPolicy.dnsTo`;
- исходящий трафик на порт `otel.endpoint` (`4318` выше), к пирам, которые перечислены в `networkPolicy.otlpTo`, или
  куда угодно, если там никто не указан; без эндпоинта — никакого правила.

Значения выше с помощью `networkPolicy.otlpTo` ограничивают правило для коллектора пространством имён коллектора,
`observability`.

| Значение | По умолчанию | Что задаёт |
| --- | --- | --- |
| `networkPolicy.enabled` | `true` | NetworkPolicy. `false` оставляет сеть пода вашей собственной политике. |
| `networkPolicy.ingressFrom` | `[]`: любые | Кто может подключаться к порту 8080, в виде пиров (peers) NetworkPolicy. |
| `networkPolicy.dnsTo` | поды `kube-dns` | Где находится DNS. Локальному кэшу DNS на узле здесь нужен его адрес, например `[{ipBlock: {cidr: 169.254.20.10/32}}]`. |
| `networkPolicy.otlpTo` | `[]`: любой адрес на порту эндпоинта | Где находится коллектор. |

Политика действует только там, где сетевой плагин кластера применяет NetworkPolicy (это делают Calico, Cilium, kindnet
из kind и большинство управляемых кластеров); в остальных местах она принимается и ничего не делает.

## Ресурсы

Каждая конвертация занимает одно ядро CPU, а память LibreOffice растёт вместе с документом, поэтому размер пода
рассчитывается из размера его пула — числа конвертаций, которые он выполняет одновременно:

| Ресурс | На под | Со значениями по умолчанию (размер пула 1) | Со значениями выше (размер пула 2) |
| --- | --- | --- | --- |
| CPU | размер пула | 1 ядро | 2 ядра |
| Память, запрос = ограничение | 256 MiB + размер пула × (`DOCX_TO_PDF_INSTANCE_HARD_MEMORY` + 256 MiB) | 2560 MiB | 4864 MiB |
| `/tmp` | размер пула × большее из 3 × `DOCX_TO_PDF_MAX_DOCUMENT_SIZE` и 256 MiB | 256 MiB | 512 MiB |

С таким ограничением памяти ни один документ не может привести к тому, что контейнер убьют из-за памяти: документ,
которому нужно больше, останавливается и получает ответ `413`. Чарт Helm вычисляет запрос CPU и память из `poolSize`;
файлы compose задают `mem_limit` по той же формуле. Типичный деловой отчёт конвертируется примерно за секунду на одном
ядре; как измерить, сколько занимают ваши документы на ваших машинах и сколько подов нужно для нагрузки, описано в
разделе [Эксплуатация](operations.md#расчёт-ресурсов).

## Классический репозиторий Helm

Для инструмента, который устанавливает чарты по URL репозитория, а не из реестра OCI, каждый релиз на GitHub несёт
индекс всех выпущенных к этому моменту чартов, причём каждая версия скачивается из своего релиза; URL ниже всегда
отдаёт индекс последнего релиза:

```sh
helm repo add docx-to-pdf https://github.com/oxidestack/docx-to-pdf/releases/latest/download
helm install docx-to-pdf docx-to-pdf/docx-to-pdf --version 1.2.0 --namespace docx-to-pdf --create-namespace
```

Чарт представлен на [Artifact Hub](https://artifacthub.io/) из своего репозитория на quay.io. Предварительный релиз
(`0.2.0-rc.1`) тоже есть в индексе; `helm search repo --devel` показывает его, и ничто не устанавливает его, если его
версию не запросили явно.

## Проверка релиза

Каждый релиз собирается и публикуется рабочим процессом релиза репозитория из своего тега, и этот рабочий процесс
подписывает всё, что публикует. Образ и чарт несут подпись [cosign](https://docs.sigstore.dev/cosign/) без ключа,
который нужно было бы распространять: сертификат называет рабочий процесс и тег, которые её создали, а подпись
записана в публичный журнал прозрачности Sigstore. Чтобы проверить, что образ — тот самый, который создал релиз:

```sh
cosign verify quay.io/oxidestack/docx-to-pdf:1.2.0 \
  --certificate-identity https://github.com/oxidestack/docx-to-pdf/.github/workflows/release.yml@refs/tags/v1.2.0 \
  --certificate-oidc-issuer https://token.actions.githubusercontent.com
cosign verify quay.io/oxidestack/charts/docx-to-pdf:1.2.0 \
  --certificate-identity https://github.com/oxidestack/docx-to-pdf/.github/workflows/release.yml@refs/tags/v1.2.0 \
  --certificate-oidc-issuer https://token.actions.githubusercontent.com
```

Образ несёт и аттестации: [сведения о сборке SLSA](https://slsa.dev/spec/v1.0/provenance) (какой коммит, какой запуск
рабочего процесса) на мультиархитектурном образе и SBOM в формате SPDX — пакеты Debian и crates, которые он содержит, —
на образе каждой архитектуры. Их проверяет [GitHub CLI](https://cli.github.com/):

```sh
gh attestation verify oci://quay.io/oxidestack/docx-to-pdf:1.2.0 --repo oxidestack/docx-to-pdf
gh attestation verify oci://quay.io/oxidestack/charts/docx-to-pdf:1.2.0 --repo oxidestack/docx-to-pdf
```

Для SBOM укажите образ одной архитектуры по его дайджесту (их перечисляет `docker buildx imagetools inspect`) и тип
его предиката, `--predicate-type https://spdx.dev/Document/v2.3`. SBOM также приложены к релизу на GitHub в виде
файлов, в SPDX и CycloneDX, по одному на архитектуру, вместе с чартом и примерами compose.

Каждый файл, приложенный к релизу на GitHub, подписан так же: его бандл Sigstore приложен рядом с ним как
`<file>.sigstore.json`, а `docx-to-pdf-1.2.0.intoto.jsonl` — сведения о сборке их всех. Бандлы записаны в текущем
формате бандлов Sigstore, который `cosign verify-blob` читает по умолчанию начиная с cosign 3. cosign 2 читает их,
только если добавить к команде `--new-bundle-format`; без него он отвечает `bundle does not contain cert for
verification`. Скачайте файл и его бандл, затем выполните одну из проверок или обе:

```sh
cosign verify-blob docx-to-pdf-1.2.0.tgz --bundle docx-to-pdf-1.2.0.tgz.sigstore.json \
  --certificate-identity https://github.com/oxidestack/docx-to-pdf/.github/workflows/release.yml@refs/tags/v1.2.0 \
  --certificate-oidc-issuer https://token.actions.githubusercontent.com
gh attestation verify docx-to-pdf-1.2.0.tgz --repo oxidestack/docx-to-pdf
```

То же относится к SBOM, примерам compose и `index.yaml` репозитория Helm. Файлы первого релиза, 1.0.0, были приложены
до того, как релиз стал их подписывать, поэтому бандла у них нет: эти две команды применимы начиная с 1.1.0.

Зеркало на ghcr.io несёт те же дайджесты с теми же подписями и аттестациями, скопированными вместе с образом, поэтому
те же команды проверяют `ghcr.io/oxidestack/docx-to-pdf:1.2.0` и `ghcr.io/oxidestack/charts/docx-to-pdf:1.2.0`
(добавьте `--bundle-from-oci` к `gh attestation verify`, чтобы читать аттестации из самого реестра).
