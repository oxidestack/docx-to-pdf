# Operaciones

<!-- languages:begin -->
[English](../../../guide/operations.md) · [Русский](../../ru/guide/operations.md) ·
[中文](../../zh/guide/operations.md) · **Español**
<!-- languages:end -->
<!-- translation of docs/guide/operations.md: f36ccbbc6491 -->

Cómo ejecutar el servicio en un contenedor o en Kubernetes, vigilarlo, acotar lo que puede costar un documento,
dimensionar un despliegue y darle fuentes. Cada ajuste que se nombra aquí es una variable de entorno descrita en
[Configuración](configuration.md); esta página explica cómo encajan entre sí. Una prueba mantiene la página fiel al
código: los nombres de métricas de abajo son los que registra el servicio, y el ejemplo de Kubernetes solo define
variables que el servicio lee, con valores que acepta.

## El contenedor

La imagen ejecuta un solo proceso, el servicio, bajo `tini`, de modo que `SIGTERM` le llega y no queda nada atrás
cuando se mata un proceso de LibreOffice. Escucha en el puerto `8080` y está compilada para `linux/amd64` y
`linux/arm64`.

Está hecha para ejecutarse blindada, y se prueba así:

- **como usuario `10001`**, grupo `10001`, que no tiene entrada en `/etc/passwd`; cualquier otro usuario que no sea
  root también sirve;
- **con un sistema de archivos raíz de solo lectura.** El único directorio en el que escribe el servicio es `/tmp`
  (la imagen sitúa `TMPDIR`, `HOME` y `XDG_CACHE_HOME` dentro de él): el perfil de LibreOffice de cada instancia, el
  documento subido, el PDF de camino a la salida, las imágenes cuya escala se está reduciendo y la caché de fuentes
  de las fuentes que usted monte. Monte ahí un volumen con permiso de escritura;
- **sin ninguna capability de Linux** y sin escalada de privilegios. Ningún archivo de la imagen conserva un bit
  setuid o setgid, así que no hay nada a lo que escalar;
- **sin red más allá de su propio puerto**: no llama a ningún otro servicio, salvo a un colector de OpenTelemetry
  cuando usted lo define. La NetworkPolicy del chart de Helm limita el pod a eso
  ([Instalación](installation.md#red-el-servicio-no-descarga-nada)).

```sh
docker run --rm -p 8080:8080 \
  --user 10001:10001 --read-only --cap-drop ALL --security-opt no-new-privileges \
  --mount type=volume,dst=/tmp \
  quay.io/oxidestack/docx-to-pdf:1.2.2
```

### `/tmp`: en disco o en memoria

`/tmp` en disco (un volumen, o un `emptyDir` en Kubernetes) es la opción recomendada: no cuesta memoria. Un tmpfs
(`--tmpfs /tmp:rw,mode=1777,size=512m`, o un `emptyDir` con `medium: Memory`) mantiene las conversiones fuera del
disco, pero todo lo que se escribe ahí cuenta para el límite de memoria del contenedor, así que sume su tamaño al
límite.

Cuánto escribe ahí una conversión: hasta tres veces el tamaño del documento a la vez (el documento, su PDF y los
archivos temporales del propio LibreOffice), o unos 256 MiB para una maquetación larga, un documento pequeño de muchas
páginas cuyo PDF crece mucho. Así que dé a `/tmp` al menos **tamaño del pool × el mayor de 3 ×
`DOCX_TO_PDF_MAX_DOCUMENT_SIZE` y 256 MiB**, más bastante menos de un megabyte por instancia para su perfil.

## Comprobaciones de salud

| Ruta | Responde | Úsela como |
| --- | --- | --- |
| `GET /livez` | `200 ok` en cuanto el servicio escucha, hagan lo que hagan sus instancias. | La comprobación de liveness. Solo falla si el proceso deja de responder HTTP por completo. |
| `GET /readyz` | `200 ok` mientras al menos una instancia de LibreOffice pueda aceptar una conversión; `503 not ready` antes de que la primera instancia esté lista, mientras ninguna pueda, mientras se ejecuta la sonda de dimensionamiento (abajo) y desde el momento en que empieza la parada. | La comprobación de readiness y la comprobación de arranque. |

El servicio escucha antes de que hayan arrancado sus instancias, así que `/livez` responde de inmediato y `/readyz`
pasa a `200` en cuanto la primera instancia está lista, normalmente en uno o dos segundos. Una instancia que no puede
arrancar se vuelve a intentar; durante el arranque, tres fallos seguidos detienen el servicio con un error. Por tanto,
una comprobación de arranque debe conceder más de tres veces `DOCX_TO_PDF_INSTANCE_START_TIMEOUT` (`30s`) más unos
segundos: el ejemplo de abajo concede 120 segundos.

La imagen no lleva ningún cliente HTTP, así que no tiene `HEALTHCHECK` de Docker; deje que el orquestador llame a las
dos rutas.

## Parada

Ante `SIGTERM` (o `SIGINT`), el servicio:

1. pasa `/readyz` a `503` y deja de aceptar conexiones nuevas;
2. deja terminar las peticiones que ya están en curso, durante como máximo `DOCX_TO_PDF_SHUTDOWN_GRACE` (por defecto
   el tiempo límite de conversión, `180s`, para que una conversión que empezó justo antes de la señal pueda llegar
   hasta su propio límite). Una petición que sigue en marcha cuando termina el periodo de gracia recibe `503`; un PDF
   que todavía se está enviando se corta;
3. detiene sus instancias de LibreOffice, hasta 5 segundos;
4. vacía la telemetría que exporta, acotado por el tiempo límite del exportador, y sale con el código 0.

Dé al runtime más tiempo que eso antes de que mate el proceso: el periodo de gracia más unos 40 segundos. En
Kubernetes, deje además que el pod espere unos segundos antes de la señal (`preStop`), para que el Service y cualquier
balanceador de carga dejen de enviarle peticiones antes de que deje de aceptarlas: `terminationGracePeriodSeconds` es
entonces esa espera + el periodo de gracia + 40.

## Kubernetes

Un ejemplo completo: dos conversiones en paralelo por pod, el contexto de seguridad de arriba, `/tmp` en disco, las
tres comprobaciones, una parada ordenada y exportación OTLP. El chart de Helm del proyecto, `charts/docx-to-pdf`,
renderiza lo mismo a partir de unos pocos valores y calcula los recursos por usted.
[Instalación](installation.md#kubernetes) lo instala.

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
          image: quay.io/oxidestack/docx-to-pdf:1.2.2
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

Por qué cada número es el que es:

- **`DOCX_TO_PDF_POOL_SIZE` = la petición de CPU.** Una conversión mantiene ocupado un núcleo; más instancias que
  núcleos no añaden rendimiento y hacen más lenta cada conversión.
- **Memoria: 256 MiB + tamaño del pool × (`DOCX_TO_PDF_INSTANCE_HARD_MEMORY` + 256 MiB)**, aquí 256 + 2 × 2304 =
  4864 MiB. El servicio mata una instancia cuya memoria supera el límite estricto mientras convierte (esa petición
  recibe `413`), y lo comprueba cada 100 ms; 256 MiB por instancia cubren el crecimiento entre dos comprobaciones, y
  256 MiB cubren el propio servicio. Con este límite ningún documento puede hacer que el pod muera por falta de
  memoria. Pida tanta memoria como el límite, para que el planificador coloque el pod donde esa memoria exista. Un
  `/tmp` en tmpfs suma aquí su tamaño.
- **`/tmp`: 2 × max(3 × 10 MiB, 256 MiB) = 512 MiB** con el límite de documento por defecto de 10 MiB.
- **`terminationGracePeriodSeconds`: 5 + 180 + 40 = 225**, la espera de `preStop`, el periodo de gracia de parada y
  el tiempo que el servicio necesita después.
- **`enableServiceLinks: false`.** Un Service llamado `docx-to-pdf` hace que Kubernetes fije `DOCX_TO_PDF_PORT` en
  `tcp://<ip>:8080` en cada pod del namespace. El servicio ignora un valor de esa forma con un aviso, pero desactivar
  los service links, y definir el puerto explícitamente, mantiene el ajuste bajo su control.
- **Sin token de cuenta de servicio**: el servicio nunca llama a la API de Kubernetes.

Un `PodDisruptionBudget` con `maxUnavailable: 1` permite que un nodo se vacíe de pod en pod.

## Telemetría

### Logs

Un objeto JSON por línea en la salida estándar (texto legible cuando la salida estándar es un terminal), filtrado por
`RUST_LOG`, `info` por defecto. Cada línea dentro de una petición lleva su `trace_id` y su `span_id`, de modo que una
línea de log lleva a su traza. Un ajuste que el servicio ignora, como un service link de Kubernetes, es una línea
`warn` al arrancar.

Cada petición deja una línea `info` al terminar, el log de acceso, sin necesidad de OTLP: cuando su respuesta se ha
enviado hasta el último byte, ha fallado o el cliente se ha ido. Su `target` es `docx_to_pdf::access_log`, su mensaje
`request`, y sus campos llevan los nombres de las convenciones HTTP de OpenTelemetry, como en la traza y las métricas:

```json
{"timestamp":"2026-11-12T14:02:07.412039Z","level":"INFO","message":"request","http.request.method":"POST","http.route":"/pdf","http.response.status_code":200,"http.server.request.duration":0.912043,"http.request.body.size":48213,"http.response.body.size":30744,"target":"docx_to_pdf::access_log","span":{"http.request.method":"POST","http.response.status_code":200,"http.route":"/pdf","network.protocol.version":"1.1","otel.kind":"server","otel.name":"POST /pdf","url.path":"/pdf","url.scheme":"http","name":"http.server.request"},"trace_id":"4bf92f3577b34da6a3ce929d0e0e4736","span_id":"b5406989157e633b"}
```

`http.server.request.duration` está en segundos, desde la llegada de la petición hasta el último byte de la
respuesta, el mismo tiempo que cuenta la métrica del mismo nombre. `http.request.body.size` son los bytes del cuerpo
de la petición que el servicio leyó, así que un documento rechazado con 413 mientras llega muestra cuánto se leyó
antes del rechazo; `http.response.body.size` son los bytes del cuerpo de la respuesta enviados. `error.type` aparece
solo en una petición que falló: un estado 5xx, o `client_disconnected`, `shutdown_grace_exceeded`, `_OTHER` (falló el
cuerpo de la respuesta). Una petición que no corresponde a ninguna ruta no tiene `http.route`. Los campos propios del
span van bajo `span`.

Las comprobaciones de salud y la recogida de métricas (`/livez`, `/readyz`, `/metrics`) se registran en `debug`, no
en `info`: una línea por comprobación cada pocos segundos ahogaría las peticiones.
`RUST_LOG=info,docx_to_pdf::access_log=debug` las muestra también; `RUST_LOG=info,docx_to_pdf::access_log=off`, como
`DOCX_TO_PDF_ACCESS_LOG` a `false`, no registra ninguna petición.

### Trazas

Cuando hay un endpoint OTLP definido (`OTEL_EXPORTER_OTLP_ENDPOINT`, consulte
[Configuración](configuration.md#logs-y-telemetría)), cada petición es una traza. Su span raíz lleva el nombre del
método y de la ruta, como `POST /pdf`; debajo de él, `document.sniff` (el reconocimiento del formato), `pool.acquire`
(la espera de una instancia), y `backend.load` y `backend.export` (LibreOffice cargando el documento y escribiendo el
PDF). El arranque de una instancia es un span propio, `instance.start`. Una cabecera W3C `traceparent` en la petición
hace que la traza sea hija de la del llamante.

Un endpoint OTLP es ASCII: escriba un nombre de host internacionalizado en punycode
(`http://xn--bcher-kva.example:4318`, no `http://bücher.example:4318`; `idn2 bücher.example` lo imprime) y codifique
la ruta con porcentajes. Un endpoint con cualquier otro carácter no exporta nada de sus señales y lo dice en una línea
`warn` al arrancar, nombrando la variable.

### Métricas

Las métricas se sirven en el formato de texto de Prometheus en `GET /metrics` en el puerto del servicio, siempre, y
también se exportan por OTLP cuando hay un endpoint definido. Los nombres siguen las convenciones de OpenTelemetry;
Prometheus los escribe con guiones bajos, la unidad como sufijo y `_total` en los contadores, y un atributo como
`http.route` pasa a ser la etiqueta `http_route`. Una serie `target_info` lleva `service.name`, `service.version` y la
versión de moalhaddar/docx-to-pdf con la que el servicio es compatible (`upstream.version`).

| Nombre en OpenTelemetry | Nombre en Prometheus | Tipo | Atributos | Lo que le dice |
| --- | --- | --- | --- | --- |
| `http.server.request.duration` | `http_server_request_duration_seconds` | histograma, segundos | `http.request.method`, `http.route`, `http.response.status_code`, `error.type` | Cuánto tardó cada petición, desde su llegada hasta el último byte de la respuesta. `http.route` es el patrón de ruta (`/pdf`, `/readyz`), nunca la ruta en bruto. |
| `http.server.active_requests` | `http_server_active_requests` | gauge | `http.request.method`, `http.route` | Las peticiones en curso ahora. |
| `docx_to_pdf.document.size` | `docx_to_pdf_document_size_bytes` | histograma, bytes | `format` | El tamaño de cada documento recibido, según el formato como el que se reconoció. |
| `docx_to_pdf.conversions` | `docx_to_pdf_conversions_total` | contador | `outcome`, `retried` | Las conversiones, según cómo terminaron: `success`, `unsupported` (415), `not_loaded` (422), `timeout` (504), `memory_limit` (413), `dead_instance` (500), `cancelled` (el cliente se fue) o `error` (500). `retried` es `true` cuando la primera instancia murió y el documento se volvió a convertir en otra. |
| `docx_to_pdf.conversion.duration` | `docx_to_pdf_conversion_duration_seconds` | histograma, segundos | `outcome` | El tiempo propio de LibreOffice para una conversión, carga más exportación, sin la subida, la cola ni la descarga. |
| `docx_to_pdf.pdf.size` | `docx_to_pdf_pdf_size_bytes` | histograma, bytes | — | El tamaño de cada PDF de respuesta. |
| `docx_to_pdf.pictures` | `docx_to_pdf_pictures_total` | contador | `outcome` | Imágenes modificadas antes de la conversión porque eran demasiado grandes para decodificarlas de forma segura: `downscaled`, `replaced` por un marcador de posición, o `failed` (el documento se convirtió tal como estaba). |
| `docx_to_pdf.pool.size` | `docx_to_pdf_pool_size` | gauge | — | El número configurado de instancias de LibreOffice. |
| `docx_to_pdf.pool.instances` | `docx_to_pdf_pool_instances` | gauge | `state` | Las instancias ahora `idle`, `busy` o `starting`. `idle` en 0 durante mucho tiempo significa que las peticiones están haciendo cola: consulte la espera más abajo. |
| `docx_to_pdf.pool.wait.duration` | `docx_to_pdf_pool_wait_duration_seconds` | histograma, segundos | `outcome` | Cuánto esperó una petición a que quedara libre una instancia; `acquired`, o `rejected` con un 503. Unas esperas que crecen hacia `DOCX_TO_PDF_POOL_WAIT_TIMEOUT` significan que el pod necesita más instancias, o que hacen falta más pods. |
| `docx_to_pdf.instance.starts` | `docx_to_pdf_instance_starts_total` | contador | `reason` | Los arranques de instancias, según el motivo: `initial`, `recycle` (tras `DOCX_TO_PDF_INSTANCE_MAX_CONVERSIONS` o por encima de `DOCX_TO_PDF_INSTANCE_MAX_MEMORY`), `crash`, `timeout`, `memory_limit`, `cancelled` (su conversión se abandonó) o `failed` (el arranque anterior no tuvo éxito). Si cualquiera que no sea `initial` ni `recycle` sube de forma continua, merece un vistazo. |
| `docx_to_pdf.instance.start.duration` | `docx_to_pdf_instance_start_duration_seconds` | histograma, segundos | — | Desde que se arranca una instancia hasta que está lista. |
| `docx_to_pdf.instance.memory.usage` | `docx_to_pdf_instance_memory_usage_bytes` | gauge, bytes | `instance` | La memoria residente actual de cada instancia, según su posición en el pool. |
| `docx_to_pdf.instance.memory.peak` | `docx_to_pdf_instance_memory_peak_bytes` | histograma, bytes | — | La memoria residente más alta de la instancia durante cada conversión. Compárela con `DOCX_TO_PDF_INSTANCE_HARD_MEMORY` para ver cuánto se acercan sus documentos al límite. |
| `process.cpu.time` | `process_cpu_time_seconds_total` | contador, segundos | `cpu.mode` | La CPU que ha usado el propio proceso del servicio, `user` y `system`; las instancias son procesos aparte y no se cuentan aquí. |
| `process.memory.usage` | `process_memory_usage_bytes` | gauge, bytes | — | La memoria residente del propio proceso del servicio. |
| `process.unix.file_descriptor.count` | `process_unix_file_descriptor_count` | gauge | — | Los archivos y sockets que el proceso del servicio mantiene abiertos. |

Para recoger `/metrics` con el operador de Prometheus, apunte un `ServiceMonitor` al puerto `http` del Service y a la
ruta `/metrics`; el chart de Helm tiene uno tras `serviceMonitor.enabled`.

## Documentos pesados: qué se rechaza y qué se acota

Un convertidor que acepta documentos de otros debe sobrevivir a cualquiera de ellos. El servicio acota cada coste que
puede tener un documento, y cada límite termina en una respuesta concreta en lugar de en un pod atascado o muerto.
Todas las respuestas que no son un PDF son `application/problem+json` con una frase `detail` fija.

| Un documento que… | Lo detiene | El cliente recibe |
| --- | --- | --- |
| es mayor de lo permitido | `DOCX_TO_PDF_MAX_DOCUMENT_SIZE` (10 MiB): se comprueba mientras llega el cuerpo, y contra `Content-Length` antes de leer nada de él | `413`, sin que el servicio lea el resto de la subida |
| es un archivo pequeño que se expande enormemente (una «bomba zip») | `DOCX_TO_PDF_MAX_INFLATED_SIZE` (256 MiB): el DOCX u ODT se descomprime de verdad y se cuenta antes de la conversión | `413` |
| contiene una imagen demasiado grande para decodificarla en memoria | `DOCX_TO_PDF_PICTURE_MAX_BITMAP` (256 MiB decodificada): la imagen se reduce, o se sustituye por un marcador de posición | un PDF, con una cabecera `Docx-To-Pdf-Pictures` que dice cuántas imágenes se modificaron |
| necesita para maquetarse más memoria de la permitida | `DOCX_TO_PDF_INSTANCE_HARD_MEMORY` (2 GiB por instancia): la instancia se mata y se sustituye | `413` |
| tarda demasiado en convertirse | `DOCX_TO_PDF_CONVERSION_TIMEOUT` (180 s): la instancia se mata y se sustituye | `504` |
| no está en un formato que el servicio convierte | el formato se reconoce por el contenido, no por el nombre | `415` |
| está en un formato admitido, pero LibreOffice no puede abrirlo | — | `422` |
| hace fallar a LibreOffice | el documento se intenta una vez más en otra instancia | un PDF si el segundo intento tiene éxito; si no, `500` |
| llega demasiado despacio, o deja de llegar | `DOCX_TO_PDF_IDLE_TIMEOUT` (120 s sin un byte) | `408` |

Y cuando hay más peticiones que instancias:

| Cuando… | Lo acota | El cliente recibe |
| --- | --- | --- |
| todas las instancias están ocupadas | las peticiones esperan en una cola, por orden de llegada | el PDF, más tarde |
| la cola está llena | `DOCX_TO_PDF_POOL_MAX_WAITING` (16) | `503` de inmediato, con `Retry-After: 10` |
| una petición esperó demasiado | `DOCX_TO_PDF_POOL_WAIT_TIMEOUT` (60 s) | `503`, con `Retry-After: 10` |

Un cliente que reintenta un `503` tras `Retry-After` y fija su tiempo de espera de lectura por encima del tiempo límite
de espera más el tiempo límite de conversión (240 segundos con los valores por defecto) nunca abandona una petición
que el servicio todavía respondería.

Una instancia también se sustituye, entre conversiones y sin que nada falle, tras
`DOCX_TO_PDF_INSTANCE_MAX_CONVERSIONS` conversiones (200) o cuando su memoria se queda por encima de
`DOCX_TO_PDF_INSTANCE_MAX_MEMORY` (1 GiB) después de una, de modo que un documento que deja a LibreOffice grande no
hace pagarlo a todas las conversiones posteriores. Una sustitución tarda alrededor de un tercio de segundo.

Tras cada conversión, antes de responder, una instancia devuelve al sistema la memoria que liberó LibreOffice
(`malloc_trim`), de modo que su memoria residente entre conversiones es su conjunto de trabajo, no lo que dejó atrás el
documento más grande reciente. En una prueba de resistencia con informes típicos, esto mantiene el contenedor de una
instancia unos 50 MiB más bajo, a cambio de unos pocos milisegundos por conversión; no reduce el pico durante una
conversión, que es a partir del cual se dimensionan los límites de memoria de abajo. No hay ningún ajuste para ello.

## Dimensionamiento

### Un punto de partida

| Recurso | Fórmula | Con los valores por defecto |
| --- | --- | --- |
| CPU | el tamaño del pool, un núcleo por instancia | 1 |
| Memoria (petición = límite) | 256 MiB + tamaño del pool × (`DOCX_TO_PDF_INSTANCE_HARD_MEMORY` + 256 MiB), más un `/tmp` en tmpfs | 2560 MiB para una instancia |
| `/tmp` | tamaño del pool × max(3 × `DOCX_TO_PDF_MAX_DOCUMENT_SIZE`, 256 MiB) | 256 MiB para una instancia |

Un informe de empresa típico se convierte en alrededor de un segundo en un núcleo, así que una instancia atiende
aproximadamente 60 documentos de ese tipo por minuto. Pero los documentos difieren en órdenes de magnitud, y las
máquinas también: mida las suyas.

### Medir una plataforma: la sonda de dimensionamiento

El servicio puede medir lo rápido que convierte la máquina en la que se ejecuta, con los límites que realmente tiene.
Defina `DOCX_TO_PDF_PROBE_PORT` (por ejemplo, en `9090`) y el servicio escucha ahí un único endpoint, `POST /probe`.
Mantenga ese puerto privado: nunca lo exponga a través de un Service. Mientras se ejecuta una sonda, retiene todas las
instancias, así que las conversiones reciben `503` y `/readyz` no está listo, y Kubernetes dirige el tráfico a los
demás pods. Sondee un pod que no reciba tráfico, o uno del que pueda prescindir durante uno o dos minutos.

```sh
kubectl port-forward pod/docx-to-pdf-7c9d8-abcde 9090:9090
curl -F document=@report-1.docx -F document=@report-2.docx \
  'http://localhost:9090/probe?cpus=2' -o platform.json
```

Envíe documentos que se parezcan a su tráfico: hasta 16, de hasta 64 MiB cada uno (o
`DOCX_TO_PDF_MAX_DOCUMENT_SIZE`, si es mayor). La sonda los convierte en el servidor, sin la red de por medio:

1. una vez en cada instancia, para calentar;
2. de uno en uno, `rounds` veces (5 por defecto): la media es **`s1`**, el tiempo de una conversión sin nada más en
   ejecución;
3. en todas las instancias a la vez, `busy` conversiones en total (120 + 3 × tamaño del pool por defecto): cuánto más
   lenta va una conversión con todos los núcleos convirtiendo, como proporción respecto a `s1`, es la **contención**;
4. 1 GiB de escrituras directas bajo `/tmp` (`disk_mib`, `0` para omitirlo; se omite en un tmpfs): los **MB/s**
   sostenidos del disco.

Después devuelve las instancias y responde con JSON: esos tres números, y cada fórmula de dimensionamiento calculada
para este pod (`cpus`, por defecto el tamaño del pool): el tamaño del pool, el rendimiento de un pod en conversiones
por minuto, el tiempo de una conversión sola y con todas las instancias ocupadas, el límite de memoria, el tamaño de
`/tmp`, los tiempos límite adecuados, el tiempo de respuesta con un ritmo de peticiones dado y el número de pods para
un ritmo y un objetivo de tiempo de respuesta. Cada fórmula viene con su justificación. El parámetro opcional `name`
etiqueta la plataforma en la respuesta.

Una segunda sonda mientras se ejecuta otra recibe `503` («a probe is already running»), igual que una enviada
mientras el servicio está arrancando o deteniéndose.

### Convertir la medición en un plan: `bench.py size`

La respuesta es también la entrada de la calculadora de dimensionamiento del repositorio, `tools/bench/bench.py size`,
que solo necesita la biblioteca estándar de Python. Pregúntele qué puede hacer una máquina dada, o qué hace falta para
alcanzar un objetivo:

```sh
# What 4 vCPU and 8 GiB give.
python3 tools/bench/bench.py size hardware --platform platform.json --cpus 4 --memory 8

# Pods for 60 requests a minute with 95 % of them answered within 5 seconds.
python3 tools/bench/bench.py size target --platform platform.json --latency 5 --quantile 0.95 --rate 60/min
```

`size hardware` responde con el pool que permite la máquina (según sus núcleos y según su memoria), el rendimiento en
saturación, el tiempo medio y el de cola de una conversión, el límite de memoria, el tamaño de `/tmp` y los ajustes
con los que arrancar el servicio, como:

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

`size target` enumera las formas de pod (vCPU, pool, memoria) que alcanzan el objetivo y cuántos pods de cada una.

Sin `--platform`, la calculadora usa una plataforma de referencia medida por el proyecto; `--s1` y `--contention`
sustituyen a mano los números de la plataforma.

## Fuentes

Un PDF solo puede parecerse al documento si las fuentes que nombra el documento están disponibles; donde falta una,
LibreOffice la sustituye por otra, y el texto se redistribuye. La imagen solo lleva fuentes cuyas licencias permiten
redistribuirlas:

- **Carlito** y **Caladea**, dibujadas con los mismos anchos que Calibri y Cambria, y **Liberation** Sans, Serif,
  Mono y Sans Narrow, con los anchos de Arial, Times New Roman, Courier New y Arial Narrow: un documento compuesto en
  esas fuentes de Microsoft conserva sus saltos de línea y de página;
- **DejaVu**, **Linux Libertine**, **Gentium** y **OpenSymbol** (la fuente de símbolos de LibreOffice);
- **Noto**: los conjuntos básico y adicional, Mono, UI, chino, japonés y coreano (CJK) y emoji en color, de modo que
  el texto en la mayoría de las escrituras del mundo se representa en lugar de recurrir a cuadros vacíos;
- los tres archivos que incluía moalhaddar/docx-to-pdf, **Droid Kufi** (normal y negrita) y **Cantarell** Extra Bold,
  para que los documentos hechos para ese servicio se vean igual.

### Montar las suyas

El directorio `/usr/share/fonts/custom` está vacío en la imagen y se examina en busca de fuentes. Monte ahí, en solo
lectura, las fuentes para las que tenga licencia, por ejemplo las propias fuentes de Microsoft cuando los sustitutos
de arriba no se parecen lo suficiente:

```sh
docker run --rm -p 8080:8080 --user 10001:10001 --read-only --cap-drop ALL \
  --mount type=volume,dst=/tmp \
  -v "$PWD/fonts:/usr/share/fonts/custom:ro" \
  quay.io/oxidestack/docx-to-pdf:1.2.2
```

En Kubernetes, monte un Secret, un ConfigMap o cualquier volumen en la misma ruta (el valor `fonts` del chart de Helm
lo hace). Las fuentes se leen cuando arranca una instancia: reinicie el pod después de cambiarlas. La primera
instancia tras un cambio escribe la caché de fuentes del directorio montado bajo `/tmp`, lo que lleva un momento al
arrancar y nada después.
