# Referencia de la API

<!-- languages:begin -->
[English](../../../guide/api.md) · [Русский](../../ru/guide/api.md) · [中文](../../zh/guide/api.md) · **Español**
<!-- languages:end -->
<!-- translation of docs/guide/api.md: f98dcbb15dbb -->

El servicio convierte un documento de oficina a PDF por petición. Esta página enumera cada ruta que responde, lo que
lleva una petición, cada respuesta que puede dar y cada error. Una prueba compara las tablas de errores con las
respuestas que puede dar el código, y repite cada ejemplo de `curl` de abajo contra el servicio, comparando su
respuesta con la que se muestra, de modo que la página no puede desviarse del servicio.

Los ejemplos suponen el servicio en `localhost:8080`, su puerto por defecto. Las respuestas se muestran tal como las
imprime `curl -i`, recortadas a las cabeceras que importan; un valor entre `<angle brackets>` varía.

## Rutas

| Método y ruta | Qué hace |
| --- | --- |
| `POST /pdf` | Convierte a PDF el documento de la petición. |
| `POST /docx-to-pdf` | La misma conversión con un segundo nombre; las dos son intercambiables. |
| `GET /livez` | Liveness: `200` mientras el proceso sirve HTTP. |
| `GET /readyz` | Readiness: `200` en cuanto el servicio puede convertir, `503` antes de eso y desde el inicio de la parada. |
| `GET /metrics` | Métricas de Prometheus, en el formato de exposición de texto. |
| `POST /probe` | La sonda de plataforma, en un puerto propio y solo cuando está activada (consulte [La sonda de plataforma](#la-sonda-de-plataforma)). |

Toda ruta que responde a `GET` también responde a `HEAD`. Cualquier otra ruta es `404`; una ruta conocida con otro
método es `405`, con una cabecera `Allow` que nombra los métodos que acepta.

## Convertir un documento

```sh
curl -sS -D - -o report.pdf -F document=@report.docx http://localhost:8080/pdf
```

```http
HTTP/1.1 200 OK
content-type: application/pdf
content-length: <bytes>
```

La petición es `multipart/form-data` con el documento en una **parte de archivo llamada `document`**: una parte cuyo
`Content-Disposition` lleva un `filename` (o `filename*`). `curl -F document=@<file>` envía exactamente eso.

- **Solo se convierte la primera parte de archivo llamada `document`.** Cualquier otra posterior, y todos los demás
  campos, se leen sin tenerse en cuenta y se ignoran. Un campo de formulario simple llamado `document` (uno sin nombre
  de archivo) no es un documento.
- **El nombre del archivo y el `Content-Type` de la parte no intervienen.** El formato se decide por los bytes del
  documento, así que un DOCX llamado `scan.pdf` se convierte, y una imagen llamada `letter.docx` se rechaza. Se acepta
  un nombre de archivo en cualquier idioma, y nunca llega al sistema de archivos.
- **El cuerpo puede enviarse con `Content-Length` o por fragmentos (chunked).** Una petición que declara un
  `Content-Length` por encima del límite se rechaza con `413` antes de leer nada del cuerpo, de modo que un cliente
  que envía `Expect: 100-continue` recibe el `413` en lugar de `100 Continue`, y no llega a subir el archivo.
- **El documento puede ocupar hasta 10 MiB** por defecto
  ([`DOCX_TO_PDF_MAX_DOCUMENT_SIZE`](configuration.md#límites-de-lo-que-puede-llevar-una-petición)), y la petición
  completa 64 KiB más, por el encapsulado multipart y otros campos.

La respuesta a un documento convertido es `200` con `Content-Type: application/pdf`, un `Content-Length` y el PDF como
cuerpo. Nada más en la petición cambia el PDF: no hay opciones.

### Los formatos aceptados

| Formato | Cómo se reconoce |
| --- | --- |
| DOCX (Word 2007 y posteriores), DOCM (con macros; las macros nunca se ejecutan), DOTX (plantilla de Word) | Un archivo ZIP cuyo `[Content_Types].xml` declara el tipo de documento principal de Word de ese formato. |
| ODT (texto OpenDocument), OTT (su plantilla) | Un archivo ZIP cuya entrada `mimetype` es `application/vnd.oasis.opendocument.text`, o `…text-template`. |
| ODT plano (`.fodt`, texto OpenDocument como un único archivo XML) | Un documento XML cuya raíz es `office:document` con `office:mimetype="application/vnd.oasis.opendocument.text"`, y el atributo termina dentro de los primeros 4000 bytes. |
| DOC (Word 97-2003) | Un archivo compuesto (OLE) cuya raíz contiene un flujo llamado `WordDocument`. |
| RTF | Los bytes empiezan por `{\rtf`. Un RTF precedido de una marca de orden de bytes UTF-8 se rechaza. |

Todo lo demás recibe `415`, antes de que empiece ninguna conversión: hojas de cálculo, presentaciones, PDF, imágenes,
texto plano, HTML, un archivo vacío. Solo tiene que estar intacta la estructura que identifica un formato: un DOCX
truncado cuyo `[Content_Types].xml` haya sobrevivido, o un DOC con un flujo de texto dañado, se acepta, y LibreOffice
decide si puede abrirlo. Cuando no puede, la respuesta es `422`.

### Imágenes modificadas antes de la conversión

LibreOffice descarta en silencio una imagen cuyo mapa de bits decodificado supera aproximadamente 1 GiB, lo que deja
un marco vacío en el PDF. Para evitarlo, un PNG o JPEG de un DOCX, DOCM, DOTX, ODT u OTT cuyo mapa de bits
decodificado es mayor que [`DOCX_TO_PDF_PICTURE_MAX_BITMAP`](configuration.md#imágenes) (256 MiB por defecto) se
reduce antes de la conversión, o se sustituye por un marcador de posición cuando no puede decodificarse de forma
segura. Cuando eso ocurre, la respuesta `200` lleva una cabecera que lo indica:

```http
Docx-To-Pdf-Pictures: downscaled=1, replaced=0
```

La cabecera no aparece cuando no se modificó ninguna imagen.

Antes de decodificar ninguna imagen, el servicio suma lo que LibreOffice retendría por las imágenes PNG y JPEG del
documento, cada una tal como quedará tras cualquier reducción de escala, y contando una imagen con transparencia como
dos veces y media su mapa de bits. Cuando eso supera lo que deja
[`DOCX_TO_PDF_INSTANCE_HARD_MEMORY`](configuration.md#el-pool-de-instancias-de-libreoffice) (el límite menos
192 MiB), la respuesta es el `413` de memoria de inmediato, sin el minuto de reducción de escala al que la conversión
no sobreviviría. Los documentos cuyas imágenes necesitan reducción de escala esperan su turno: como mucho tantos a la
vez como [`DOCX_TO_PDF_POOL_SIZE`](configuration.md#el-pool-de-instancias-de-libreoffice), y los demás esperan.

### Llamadas desde un navegador

Las dos rutas de conversión responden a peticiones de origen cruzado desde cualquier origen: la respuesta a una
petición con cabecera `Origin` lleva `Access-Control-Allow-Origin: *` y expone `Docx-To-Pdf-Pictures` a la página.
Una petición preliminar (preflight) se responde con `200`:

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

Las cabeceras que pide una petición preliminar se permiten, sean cuales sean, y el navegador puede guardar la
respuesta en caché durante 30 minutos. Una petición preliminar siempre nombra solo `POST`, sea cual sea el método por
el que preguntó. Las demás rutas no tienen cabeceras de origen cruzado.

## Errores

Cada error es un documento de problema ([RFC 9457](https://www.rfc-editor.org/rfc/rfc9457)) con
`Content-Type: application/problem+json` y exactamente cuatro campos:

| Campo | Valor |
| --- | --- |
| `type` | Siempre `about:blank`: el estado dice de qué tipo de error se trata. |
| `title` | La frase de motivo del estado, como `Unsupported Media Type`. |
| `status` | El código de estado, como número. |
| `detail` | Una frase fija para cada situación, de la tabla de abajo. Nunca contiene el nombre del archivo, una ruta, el contenido del documento ni una traza de pila, así que es seguro mostrarla o registrarla en el log. |

```sh
curl -sS -i -F document=@photo.png http://localhost:8080/pdf
```

```http
HTTP/1.1 415 Unsupported Media Type
content-type: application/problem+json

{"type":"about:blank","title":"Unsupported Media Type","status":415,"detail":"the document is not DOCX, DOC, ODT or RTF"}
```

Un cliente debe decidir según el estado. El `detail` le dice a una persona qué situación fue.

### Todos los errores que responde el servicio

| Estado | `detail` | Cuándo |
| --- | --- | --- |
| 400 | `the body is not multipart/form-data with a boundary` | La petición no tiene `Content-Type`, tiene otro, o uno multipart sin boundary. |
| 400 | `the multipart body is malformed` | El cuerpo multipart no puede analizarse, o termina antes de su boundary de cierre. |
| 400 | `the body has no file part named "document"` | Ninguna parte es una parte de archivo llamada `document`. |
| 404 | `no such path; the service converts at POST /pdf and POST /docx-to-pdf` | La ruta no es ninguna de las de [Rutas](#rutas). |
| 405 | `this path does not take this method; see Allow` | La ruta existe, pero no con este método; `Allow` nombra los que acepta. Un `HEAD` a una ruta de conversión recibe este estado sin cuerpo. |
| 408 | `no part of the request arrived within the idle limit` | No llegó ningún byte de la cabecera ni del cuerpo de la petición durante [`DOCX_TO_PDF_IDLE_TIMEOUT`](configuration.md#servidor) (2 minutos por defecto). La respuesta lleva `Connection: close` y la conexión se cierra. |
| 413 | `the document or the request is over the size limit` | El documento es mayor que [`DOCX_TO_PDF_MAX_DOCUMENT_SIZE`](configuration.md#límites-de-lo-que-puede-llevar-una-petición), o la petición mayor que eso más 64 KiB. Se responde en cuanto se cruza el límite: el resto del cuerpo no se lee. |
| 413 | `the document's compressed parts inflate past the size limit` | Un DOCX, DOCM, DOTX, ODT u OTT cuyas partes, descomprimidas, suman más que [`DOCX_TO_PDF_MAX_INFLATED_SIZE`](configuration.md#límites-de-lo-que-puede-llevar-una-petición): una «bomba zip». Se responde antes de que empiece ninguna conversión. |
| 413 | `the document needs more memory to convert than the service allows` | La conversión ocupó más memoria que [`DOCX_TO_PDF_INSTANCE_HARD_MEMORY`](configuration.md#el-pool-de-instancias-de-libreoffice) y se detuvo; o, antes de cualquier conversión, las imágenes del documento suman más de lo que deja ese límite ([Imágenes modificadas antes de la conversión](#imágenes-modificadas-antes-de-la-conversión)). Un reintento con el mismo documento muy probablemente chocará con el mismo límite. |
| 415 | `the document is not DOCX, DOC, ODT or RTF` | El documento no está en ninguno de [los formatos aceptados](#los-formatos-aceptados), o está vacío. |
| 422 | `the document could not be loaded` | El formato se acepta, pero LibreOffice no puede abrir el documento: está dañado de una forma que no puede sortear. |
| 500 | `the service failed to answer this request` | Cualquier otra cosa: la instancia de LibreOffice murió durante la conversión y de nuevo durante el reintento automático, LibreOffice no pudo escribir el PDF, o el servicio no pudo escribir el documento en su directorio temporal. El servicio registra la causa en el log. |
| 503 | `the service is not ready to convert documents yet` | El servicio acaba de arrancar y sus instancias de LibreOffice todavía no están listas. `Retry-After: 30`. |
| 503 | `no instance came free within the wait limit` | Todas las instancias siguieron ocupadas durante [`DOCX_TO_PDF_POOL_WAIT_TIMEOUT`](configuration.md#el-pool-de-instancias-de-libreoffice); o, para un documento con una imagen que reducir, tantos otros documentos como `DOCX_TO_PDF_POOL_SIZE` estuvieron reduciendo las suyas durante todo ese tiempo. `Retry-After: 10`. |
| 503 | `too many requests are already waiting for an instance` | Ya hay [`DOCX_TO_PDF_POOL_MAX_WAITING`](configuration.md#el-pool-de-instancias-de-libreoffice) peticiones en la cola. Se responde de inmediato. `Retry-After: 10`. |
| 503 | `the service is shutting down` | El servicio recibió `SIGTERM`. Una petición que no había llegado a una instancia recibe `Retry-After: 10`; una que sigue ejecutándose al final del periodo de gracia de parada recibe `Retry-After: 1`. Otra réplica puede atenderla. |
| 503 | `the service is measuring its platform; another replica can convert meanwhile` | La sonda de plataforma retiene todas las instancias. `Retry-After: 10`. |
| 504 | `the conversion did not finish within the conversion timeout` | La conversión duró más que [`DOCX_TO_PDF_CONVERSION_TIMEOUT`](configuration.md#el-pool-de-instancias-de-libreoffice) (3 minutos por defecto) y se detuvo. |

Todo `503` lleva `Retry-After`, en segundos enteros. El servicio reintenta una conversión una vez por su cuenta
cuando su instancia de LibreOffice muere durante ella, así que un `500` ya se ha reintentado. Un `422`, un `413` y un
`415` no cambiarán al reintentar con el mismo documento; un `503` y un `504` pueden cambiar.

Los errores de las rutas de conversión llevan las mismas cabeceras de origen cruzado que su `200`, de modo que una
página en un navegador puede leerlos.

### Ejemplos

Un formulario cuyo campo de archivo tiene otro nombre:

```sh
curl -sS -i -F file=@report.docx http://localhost:8080/pdf
```

```http
HTTP/1.1 400 Bad Request
content-type: application/problem+json

{"type":"about:blank","title":"Bad Request","status":400,"detail":"the body has no file part named \"document\""}
```

Un documento por encima del límite de tamaño (aquí 11 MiB, siendo el límite por defecto 10 MiB):

```sh
curl -sS -i -F document=@scan.docx http://localhost:8080/pdf
```

```http
HTTP/1.1 413 Payload Too Large
content-type: application/problem+json

{"type":"about:blank","title":"Payload Too Large","status":413,"detail":"the document or the request is over the size limit"}
```

Un DOCX de unos cientos de KiB cuyas partes se descomprimen a 257 MiB, por encima del límite por defecto de 256 MiB:

```sh
curl -sS -i -F document=@bomb.docx http://localhost:8080/pdf
```

```http
HTTP/1.1 413 Payload Too Large
content-type: application/problem+json

{"type":"about:blank","title":"Payload Too Large","status":413,"detail":"the document's compressed parts inflate past the size limit"}
```

Un documento en un formato aceptado que LibreOffice no puede abrir:

```sh
curl -sS -i -F document=@damaged.rtf http://localhost:8080/docx-to-pdf
```

```http
HTTP/1.1 422 Unprocessable Entity
content-type: application/problem+json

{"type":"about:blank","title":"Unprocessable Entity","status":422,"detail":"the document could not be loaded"}
```

Una ruta de conversión llamada con `GET`:

```sh
curl -sS -i http://localhost:8080/pdf
```

```http
HTTP/1.1 405 Method Not Allowed
allow: POST
content-type: application/problem+json

{"type":"about:blank","title":"Method Not Allowed","status":405,"detail":"this path does not take this method; see Allow"}
```

Una ruta que no existe:

```sh
curl -sS -i http://localhost:8080/actuator/health
```

```http
HTTP/1.1 404 Not Found
content-type: application/problem+json

{"type":"about:blank","title":"Not Found","status":404,"detail":"no such path; the service converts at POST /pdf and POST /docx-to-pdf"}
```

## Comprobaciones de salud

```sh
curl -sS -i http://localhost:8080/livez
```

```http
HTTP/1.1 200 OK
content-type: text/plain; charset=utf-8

ok
```

`/livez` responde `200 ok` mientras el proceso sirva HTTP, sea cual sea el estado de LibreOffice. Úselo como
comprobación de liveness: un fallo significa que hay que reiniciar el proceso.

```sh
curl -sS -i http://localhost:8080/readyz
```

```http
HTTP/1.1 200 OK
content-type: text/plain; charset=utf-8

ok
```

`/readyz` responde `200 ok` en cuanto han arrancado las instancias de LibreOffice, y `503` con el cuerpo de texto
plano `not ready` antes de eso, mientras se ejecuta la sonda de plataforma y desde el momento en que el servicio
recibe `SIGTERM`. Úselo como comprobación de readiness, para que un balanceador de carga envíe conversiones solo a una
réplica que pueda aceptarlas. Estas dos respuestas son texto plano, no documentos de problema. Cómo conectarlas a una
plataforma de contenedores, y cuánto tiempo debe conceder una comprobación de arranque, está en
[Operaciones: comprobaciones de salud](operations.md#comprobaciones-de-salud).

## Métricas

```sh
curl -sS -i http://localhost:8080/metrics
```

```http
HTTP/1.1 200 OK
content-type: text/plain; version=0.0.4; charset=utf-8

<the metrics>
```

`/metrics` sirve las métricas del servicio en el formato de texto de Prometheus: las peticiones HTTP por ruta y
estado, las conversiones por resultado y sus duraciones, los tamaños de los documentos y de los PDF, el pool de
instancias y el proceso. Siempre está activo y no necesita ningún ajuste. Las mismas métricas, con trazas y logs,
también pueden exportarse por OpenTelemetry; consulte [Logs y telemetría](configuration.md#logs-y-telemetría). Cada
métrica, su nombre en Prometheus y lo que significa se enumeran en
[Operaciones: métricas](operations.md#métricas).

## La sonda de plataforma

`POST /probe` mide lo rápido que convierte esta máquina, para dimensionar un despliegue. Solo existe cuando
[`DOCX_TO_PDF_PROBE_PORT`](configuration.md#servidor) está definida, y solo en ese puerto, nunca en el puerto de
conversión: mientras se ejecuta retiene todas las instancias de LibreOffice, así que las conversiones reciben `503` y
`/readyz` informa de que no está listo. Tarda uno o dos minutos. Qué significa su informe y cómo convertirlo en el
tamaño de un despliegue está en
[Operaciones: medir una plataforma](operations.md#medir-una-plataforma-la-sonda-de-dimensionamiento).

La petición es `multipart/form-data` con de 1 a 16 partes de archivo llamadas `document`, los documentos con los que
medir, cada uno dentro del límite de tamaño de documento. Parámetros de consulta opcionales:

| Parámetro | Por defecto | Significado |
| --- | --- | --- |
| `rounds` | `5` | Cuántas veces se convierte cada documento, de uno en uno (de 1 a 100). |
| `busy` | 120 + 3 × el tamaño del pool | Cuántas conversiones se ejecutan con todas las instancias ocupadas (hasta 10000). |
| `disk_mib` | `1024` | Cuántos MiB se escriben para medir el disco (hasta 65536); `0` lo omite. |
| `cpus` | el tamaño del pool | El número de CPU para el que se calculan las fórmulas de la respuesta (de 1 a 1024). |
| `name` | el modelo y el número de CPU | Un nombre para la plataforma en la respuesta. |

La respuesta es `200` con un informe JSON. Sus errores son documentos de problema, como arriba; además de los de las
rutas de conversión que le son aplicables, la sonda tiene estos:

| Estado | `detail` | Cuándo |
| --- | --- | --- |
| 400 | `rounds, busy, cpus and disk_mib are whole numbers within their bounds` | Un parámetro no es un número entero, o supera su límite. |
| 400 | `the probe takes rounds, busy, disk_mib, cpus and name` | La consulta tiene otro parámetro. |
| 413 | `the document or the request is over the size limit` | Un documento supera el límite de tamaño, o la petición lleva más de 16 documentos. |
| 422 | `a probe document could not be loaded` | LibreOffice no puede abrir uno de los documentos. |
| 503 | `a probe is already running` | Otra sonda retiene las instancias. `Retry-After: 30`. |
| 503 | `the conversions under way did not end in time` | La sonda espera a que terminen las conversiones que ya se están ejecutando antes de tomar las instancias; no terminaron dentro del tiempo límite de conversión más el tiempo límite de arranque de instancia. `Retry-After: 30`. |
| 503 | `the service is shutting down` | El servicio recibió `SIGTERM`. `Retry-After: 30`. |
