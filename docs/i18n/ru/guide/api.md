# Справочник API

<!-- languages:begin -->
[English](../../../guide/api.md) · **Русский** · [中文](../../zh/guide/api.md) · [Español](../../es/guide/api.md)
<!-- languages:end -->
<!-- translation of docs/guide/api.md: 75631133af9c -->

Сервис конвертирует один офисный документ в PDF за запрос. Эта страница перечисляет каждый путь, на который он
отвечает, что несёт запрос, каждый ответ, который он может дать, и каждую ошибку. Тест сравнивает таблицы ошибок с
ответами, которые может дать код, и повторяет каждый пример `curl` ниже против сервиса, сравнивая его ответ с
показанным, поэтому страница не может разойтись с сервисом.

Примеры предполагают сервис на `localhost:8080`, его порту по умолчанию. Ответы показаны так, как их печатает
`curl -i`, только с важными заголовками; значение в `<angle brackets>` меняется.

## Пути

| Метод и путь | Что делает |
| --- | --- |
| `POST /pdf` | Конвертирует документ из запроса в PDF. |
| `POST /docx-to-pdf` | Та же конвертация под вторым именем; эти два пути взаимозаменяемы. |
| `GET /livez` | Живость: `200`, пока процесс обслуживает HTTP. |
| `GET /readyz` | Готовность: `200`, как только сервис может конвертировать, `503` до этого и с начала остановки. |
| `GET /metrics` | Метрики Prometheus в текстовом формате экспозиции. |
| `POST /probe` | Проба платформы, на собственном порту и только когда она включена (см. [Проба платформы](#проба-платформы)). |

Каждый путь, который отвечает на `GET`, отвечает и на `HEAD`. Любой другой путь — `404`; известный путь с другим
методом — `405` с заголовком `Allow`, который называет принимаемые им методы.

## Конвертация документа

```sh
curl -sS -D - -o report.pdf -F document=@report.docx http://localhost:8080/pdf
```

```http
HTTP/1.1 200 OK
content-type: application/pdf
content-length: <bytes>
```

Запрос — `multipart/form-data` с документом в **файловой части с именем `document`**: части, чей
`Content-Disposition` несёт `filename` (или `filename*`). Ровно это и отправляет `curl -F document=@<file>`.

- **Конвертируется только первая файловая часть с именем `document`.** Любая следующая и каждое другое поле
  пропускаются и игнорируются. Обычное поле формы с именем `document` (без имени файла) — не документ.
- **Имя файла и `Content-Type` части не играют роли.** Формат определяется по байтам документа, поэтому DOCX с
  именем `scan.pdf` конвертируется, а изображение с именем `letter.docx` отклоняется. Имя файла на любом языке
  принимается и никогда не попадает в файловую систему.
- **Тело можно отправить с `Content-Length` или по частям (chunked).** Запрос, который объявляет `Content-Length`
  больше ограничения, отклоняется с `413` до того, как прочитан хоть байт тела, поэтому клиент, который отправляет
  `Expect: 100-continue`, получает `413` вместо `100 Continue` и вообще не загружает файл.
- **Документ может быть до 10 MiB** по умолчанию
  ([`DOCX_TO_PDF_MAX_DOCUMENT_SIZE`](configuration.md#ограничения-на-содержимое-запроса)), а весь запрос — на 64 KiB
  больше, для обрамления multipart и других полей.

Ответ на сконвертированный документ — `200` с `Content-Type: application/pdf`, `Content-Length` и PDF в теле.
Больше ничто в запросе не меняет PDF: параметров нет.

### Принимаемые форматы

| Формат | Как он распознаётся |
| --- | --- |
| DOCX (Word 2007 и новее), DOCM (с макросами; макросы никогда не выполняются), DOTX (шаблон Word) | Архив ZIP, чей `[Content_Types].xml` объявляет тип основного документа Word этого формата. |
| ODT (текст OpenDocument), OTT (его шаблон) | Архив ZIP, чья запись `mimetype` — `application/vnd.oasis.opendocument.text` или `…text-template`. |
| Плоский ODT (`.fodt`, текст OpenDocument одним файлом XML) | Документ XML, корень которого — `office:document` с `office:mimetype="application/vnd.oasis.opendocument.text"`, причём атрибут заканчивается в первых 4000 байтах. |
| DOC (Word 97-2003) | Составной файл (OLE), в корне которого есть поток с именем `WordDocument`. |
| RTF | Байты начинаются с `{\rtf`. RTF, перед которым стоит метка порядка байтов UTF-8, отклоняется. |

Всё остальное получает `415` до начала любой конвертации: электронные таблицы, презентации, PDF, изображения,
простой текст, HTML, пустой файл. Целой должна быть только структура, по которой узнаётся формат: обрезанный DOCX, у
которого уцелел `[Content_Types].xml`, или DOC с повреждённым текстовым потоком принимается, и LibreOffice решает,
может ли он его открыть. Когда не может, ответ — `422`.

### Изображения, изменённые перед конвертацией

LibreOffice молча выбрасывает изображение, чей декодированный битмап больше примерно 1 GiB, и в PDF остаётся пустая
рамка. Чтобы этого не было, PNG или JPEG в DOCX, DOCM, DOTX, ODT или OTT, чей декодированный битмап больше
[`DOCX_TO_PDF_PICTURE_MAX_BITMAP`](configuration.md#изображения) (256 MiB по умолчанию), уменьшается перед
конвертацией или, когда его нельзя безопасно декодировать, заменяется заглушкой. Когда это происходит, ответ `200`
несёт заголовок, который об этом говорит:

```http
Docx-To-Pdf-Pictures: downscaled=1, replaced=0
```

Заголовка нет, когда ни одно изображение не изменено.

Прежде чем декодировать хоть одно изображение, сервис складывает, сколько LibreOffice держал бы для изображений PNG и
JPEG документа, каждого в том виде, в каком оно будет после уменьшения, и считая изображение с прозрачностью в два с
половиной раза больше его битмапа. Когда сумма превышает то, что оставляет
[`DOCX_TO_PDF_INSTANCE_HARD_MEMORY`](configuration.md#пул-экземпляров-libreoffice) (ограничение минус 192 MiB),
ответ — сразу `413` по памяти, без минуты уменьшения, которую конвертация всё равно не пережила бы. Документы,
изображения которых нужно уменьшить, ждут своей очереди: одновременно не больше, чем
[`DOCX_TO_PDF_POOL_SIZE`](configuration.md#пул-экземпляров-libreoffice), остальные ждут.

### Вызов из браузера

Два пути конвертации отвечают на кросс-доменные запросы с любого источника: ответ на запрос с заголовком `Origin`
несёт `Access-Control-Allow-Origin: *` и открывает странице `Docx-To-Pdf-Pictures`. На предварительный запрос
(preflight) приходит ответ `200`:

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

Заголовки, о которых спрашивает предварительный запрос, разрешаются, какими бы они ни были, и браузер может
кешировать ответ на 30 минут. Ответ на предварительный запрос всегда называет только `POST`, о каком бы методе ни
спрашивали. У остальных путей кросс-доменных заголовков нет.

## Ошибки

Каждая ошибка — документ problem ([RFC 9457](https://www.rfc-editor.org/rfc/rfc9457)) с
`Content-Type: application/problem+json` и ровно четырьмя полями:

| Поле | Значение |
| --- | --- |
| `type` | Всегда `about:blank`: что это за ошибка, говорит статус. |
| `title` | Поясняющая фраза статуса, например `Unsupported Media Type`. |
| `status` | Код статуса числом. |
| `detail` | Неизменная фраза для ситуации из таблицы ниже. В ней никогда нет имени файла, пути, содержимого документа или трассировки стека, поэтому её безопасно показывать и записывать в лог. |

```sh
curl -sS -i -F document=@photo.png http://localhost:8080/pdf
```

```http
HTTP/1.1 415 Unsupported Media Type
content-type: application/problem+json

{"type":"about:blank","title":"Unsupported Media Type","status":415,"detail":"the document is not DOCX, DOC, ODT or RTF"}
```

Клиенту следует принимать решение по статусу. `detail` говорит человеку, какая это была ситуация.

### Все ошибки сервиса

| Статус | `detail` | Когда |
| --- | --- | --- |
| 400 | `the body is not multipart/form-data with a boundary` | У запроса нет `Content-Type`, он другой или это multipart без boundary. |
| 400 | `the multipart body is malformed` | Тело multipart не удаётся разобрать, или оно заканчивается до закрывающего boundary. |
| 400 | `the body has no file part named "document"` | Ни одна часть не является файловой частью с именем `document`. |
| 404 | `no such path; the service converts at POST /pdf and POST /docx-to-pdf` | Путь не входит в перечисленные в разделе [Пути](#пути). |
| 405 | `this path does not take this method; see Allow` | Путь существует, но не с этим методом; `Allow` называет принимаемые. `HEAD` на путь конвертации получает этот статус без тела. |
| 408 | `no part of the request arrived within the idle limit` | Ни один байт заголовка или тела запроса не пришёл за [`DOCX_TO_PDF_IDLE_TIMEOUT`](configuration.md#сервер) (по умолчанию 2 минуты). Ответ несёт `Connection: close`, и соединение закрывается. |
| 413 | `the document or the request is over the size limit` | Документ больше [`DOCX_TO_PDF_MAX_DOCUMENT_SIZE`](configuration.md#ограничения-на-содержимое-запроса) или запрос больше этого значения плюс 64 KiB. Ответ приходит, как только ограничение превышено: остаток тела не читается. |
| 413 | `the document's compressed parts inflate past the size limit` | DOCX, DOCM, DOTX, ODT или OTT, чьи части в распакованном виде в сумме больше [`DOCX_TO_PDF_MAX_INFLATED_SIZE`](configuration.md#ограничения-на-содержимое-запроса): «zip-бомба». Ответ приходит до начала любой конвертации. |
| 413 | `the document needs more memory to convert than the service allows` | Конвертация заняла больше памяти, чем [`DOCX_TO_PDF_INSTANCE_HARD_MEMORY`](configuration.md#пул-экземпляров-libreoffice), и была остановлена; или, до любой конвертации, изображения документа в сумме больше того, что оставляет это ограничение ([Изображения, изменённые перед конвертацией](#изображения-изменённые-перед-конвертацией)). Повтор с тем же документом, скорее всего, упрётся в то же ограничение. |
| 415 | `the document is not DOCX, DOC, ODT or RTF` | Документ не относится к [принимаемым форматам](#принимаемые-форматы) или пуст. |
| 422 | `the document could not be loaded` | Формат принимается, но LibreOffice не может открыть документ: он повреждён так, что LibreOffice не может прочитать его дальше этого места. |
| 500 | `the service failed to answer this request` | Всё остальное: экземпляр LibreOffice погиб во время конвертации и снова во время автоматического повтора, LibreOffice не смог записать PDF или сервис не смог записать документ в свой временный каталог. Сервис пишет причину в лог. |
| 503 | `the service is not ready to convert documents yet` | Сервис только что запустился, и его экземпляры LibreOffice ещё не поднялись. `Retry-After: 30`. |
| 503 | `no instance came free within the wait limit` | Все экземпляры оставались занятыми в течение [`DOCX_TO_PDF_POOL_WAIT_TIMEOUT`](configuration.md#пул-экземпляров-libreoffice); или, для документа с изображением, которое нужно уменьшить, всё это время столько других документов, сколько задаёт `DOCX_TO_PDF_POOL_SIZE`, уменьшали свои. `Retry-After: 10`. |
| 503 | `too many requests are already waiting for an instance` | В очереди уже [`DOCX_TO_PDF_POOL_MAX_WAITING`](configuration.md#пул-экземпляров-libreoffice) запросов. Ответ приходит сразу. `Retry-After: 10`. |
| 503 | `the service is shutting down` | Сервис получил `SIGTERM`. Запрос, который ещё не дошёл до экземпляра, получает `Retry-After: 10`; запрос, который всё ещё выполняется в конце времени на остановку, — `Retry-After: 1`. Его может принять другая реплика. |
| 503 | `the service is measuring its platform; another replica can convert meanwhile` | Проба платформы занимает все экземпляры. `Retry-After: 10`. |
| 504 | `the conversion did not finish within the conversion timeout` | Конвертация шла дольше [`DOCX_TO_PDF_CONVERSION_TIMEOUT`](configuration.md#пул-экземпляров-libreoffice) (по умолчанию 3 минуты) и была остановлена. |

Каждый `503` несёт `Retry-After` в целых секундах. Сервис сам один раз повторяет конвертацию, когда под ней гибнет
её экземпляр LibreOffice, поэтому `500` уже был повторён. `422`, `413` и `415` не изменятся при повторе с тем же
документом; `503` и `504` могут.

Ошибки путей конвертации несут те же кросс-доменные заголовки, что и их `200`, поэтому страница в браузере может их
прочитать.

### Примеры

Форма, файловое поле которой называется иначе:

```sh
curl -sS -i -F file=@report.docx http://localhost:8080/pdf
```

```http
HTTP/1.1 400 Bad Request
content-type: application/problem+json

{"type":"about:blank","title":"Bad Request","status":400,"detail":"the body has no file part named \"document\""}
```

Документ больше ограничения размера (здесь 11 MiB при ограничении по умолчанию 10 MiB):

```sh
curl -sS -i -F document=@scan.docx http://localhost:8080/pdf
```

```http
HTTP/1.1 413 Payload Too Large
content-type: application/problem+json

{"type":"about:blank","title":"Payload Too Large","status":413,"detail":"the document or the request is over the size limit"}
```

DOCX в несколько сотен KiB, части которого распаковываются в 257 MiB, больше ограничения по умолчанию в 256 MiB:

```sh
curl -sS -i -F document=@bomb.docx http://localhost:8080/pdf
```

```http
HTTP/1.1 413 Payload Too Large
content-type: application/problem+json

{"type":"about:blank","title":"Payload Too Large","status":413,"detail":"the document's compressed parts inflate past the size limit"}
```

Документ в принимаемом формате, который LibreOffice не может открыть:

```sh
curl -sS -i -F document=@damaged.rtf http://localhost:8080/docx-to-pdf
```

```http
HTTP/1.1 422 Unprocessable Entity
content-type: application/problem+json

{"type":"about:blank","title":"Unprocessable Entity","status":422,"detail":"the document could not be loaded"}
```

Путь конвертации, вызванный с `GET`:

```sh
curl -sS -i http://localhost:8080/pdf
```

```http
HTTP/1.1 405 Method Not Allowed
allow: POST
content-type: application/problem+json

{"type":"about:blank","title":"Method Not Allowed","status":405,"detail":"this path does not take this method; see Allow"}
```

Путь, которого не существует:

```sh
curl -sS -i http://localhost:8080/actuator/health
```

```http
HTTP/1.1 404 Not Found
content-type: application/problem+json

{"type":"about:blank","title":"Not Found","status":404,"detail":"no such path; the service converts at POST /pdf and POST /docx-to-pdf"}
```

## Проверки здоровья

```sh
curl -sS -i http://localhost:8080/livez
```

```http
HTTP/1.1 200 OK
content-type: text/plain; charset=utf-8

ok
```

`/livez` отвечает `200 ok`, пока процесс обслуживает HTTP, в каком бы состоянии ни был LibreOffice. Используйте его
как проверку живости: сбой означает, что процесс нужно перезапустить.

```sh
curl -sS -i http://localhost:8080/readyz
```

```http
HTTP/1.1 200 OK
content-type: text/plain; charset=utf-8

ok
```

`/readyz` отвечает `200 ok`, как только экземпляры LibreOffice запустились, и `503` с текстовым телом `not ready` до
этого, пока идёт проба платформы, и с момента, когда сервис получает `SIGTERM`. Используйте его как проверку
готовности, чтобы балансировщик нагрузки отправлял конвертации только реплике, которая может их принять. Эти два
ответа — простой текст, а не документы problem. Как подключить их к контейнерной платформе и сколько времени должна
давать проверка запуска, описано в разделе [Эксплуатация: проверки здоровья](operations.md#проверки-здоровья).

## Метрики

```sh
curl -sS -i http://localhost:8080/metrics
```

```http
HTTP/1.1 200 OK
content-type: text/plain; version=0.0.4; charset=utf-8

<the metrics>
```

`/metrics` отдаёт метрики сервиса в текстовом формате Prometheus: HTTP-запросы по пути и статусу, конвертации по
результату и их длительность, размеры документов и PDF, пул экземпляров и процесс. Эндпоинт всегда включён и не
требует настройки. Те же метрики вместе с трассами и логами можно также экспортировать через OpenTelemetry; см.
[Логи и телеметрия](configuration.md#логи-и-телеметрия). Каждая метрика, её имя в Prometheus и её смысл перечислены
в разделе [Эксплуатация: метрики](operations.md#метрики).

## Проба платформы

`POST /probe` измеряет, как быстро конвертирует эта машина, для расчёта ресурсов развёртывания. Он существует, только
когда задан [`DOCX_TO_PDF_PROBE_PORT`](configuration.md#сервер), и только на этом порту, никогда на порту
конвертации: пока проба идёт, она занимает все экземпляры LibreOffice, поэтому конвертации получают `503`, а
`/readyz` сообщает о неготовности. Она занимает минуту-две. Что значит её отчёт и как превратить его в размер
развёртывания, описано в разделе
[Эксплуатация: замер платформы](operations.md#замер-платформы-проба-для-расчёта-ресурсов).

Запрос — `multipart/form-data` с 1–16 файловыми частями с именем `document`, документами для замера, каждый в
пределах ограничения размера документа. Необязательные параметры запроса:

| Параметр | По умолчанию | Значение |
| --- | --- | --- |
| `rounds` | `5` | Сколько раз каждый документ конвертируется по одному (от 1 до 100). |
| `busy` | 120 + 3 × размер пула | Сколько конвертаций выполняется, когда заняты все экземпляры (до 10000). |
| `disk_mib` | `1024` | Сколько MiB записывается для замера диска (до 65536); `0` пропускает замер. |
| `cpus` | размер пула | Число CPU, для которого рассчитываются формулы ответа (от 1 до 1024). |
| `name` | модель и число CPU | Имя платформы в ответе. |

Ответ — `200` с отчётом в JSON. Его ошибки — документы problem, как выше; кроме тех ошибок путей конвертации, что к
нему применимы, у пробы есть такие:

| Статус | `detail` | Когда |
| --- | --- | --- |
| 400 | `rounds, busy, cpus and disk_mib are whole numbers within their bounds` | Параметр не целое число или превышает свою границу. |
| 400 | `the probe takes rounds, busy, disk_mib, cpus and name` | В запросе есть другой параметр. |
| 413 | `the document or the request is over the size limit` | Документ больше ограничения размера, или запрос несёт больше 16 документов. |
| 422 | `a probe document could not be loaded` | LibreOffice не может открыть один из документов. |
| 503 | `a probe is already running` | Экземпляры занимает другая проба. `Retry-After: 30`. |
| 503 | `the conversions under way did not end in time` | Проба ждёт, пока закончатся уже идущие конвертации, прежде чем занять экземпляры; они не закончились за тайм-аут конвертации плюс тайм-аут запуска экземпляра. `Retry-After: 30`. |
| 503 | `the service is shutting down` | Сервис получил `SIGTERM`. `Retry-After: 30`. |
