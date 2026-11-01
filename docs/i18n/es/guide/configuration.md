# Configuración

<!-- languages:begin -->
[English](../../../guide/configuration.md) · [Русский](../../ru/guide/configuration.md) ·
[中文](../../zh/guide/configuration.md) · **Español**
<!-- languages:end -->
<!-- translation of docs/guide/configuration.md: 09049b88b00a -->

Cada ajuste es una variable de entorno, que se lee una sola vez cuando arranca el servicio. Esta página las enumera
todas; una prueba compara sus tablas con las variables que lee el código, de modo que no puede existir un ajuste sin
una fila aquí.

## Cómo se leen los valores

- **Sin definir o vacía** es lo mismo: una variable que solo contiene espacios cuenta como sin definir, y se aplica el
  valor por defecto. Los espacios iniciales y finales se recortan de todos los valores.
- **Un valor que el servicio no puede usar lo detiene al arrancar**, con una línea en la salida de error estándar que
  nombra la variable y lo que está mal, y un código de salida distinto de cero. Nunca recurre al valor por defecto ante
  un valor incorrecto.
- **Las duraciones** llevan unidad: `500ms`, `30s`, `2m`, `1h`. Un número entero, sin decimales ni espacios en medio;
  `30` solo se rechaza.
- **Los tamaños** son bytes sin más (`1048576`) o un número entero con unidad, sin distinguir mayúsculas y
  minúsculas: `B`, `KiB`, `MiB`, `GiB`. `KB`, `MB` y `GB` se aceptan y son **binarios**, como en los ajustes de
  Spring, para que un valor copiado de una aplicación Spring conserve su significado: `10MB` son 10 485 760 bytes, lo
  mismo que `10MiB`.
- **Los puertos** son números enteros de 1 a 65535. Hay una excepción, para Kubernetes: un Service llamado
  `docx-to-pdf` hace que Kubernetes inyecte `DOCX_TO_PDF_PORT=tcp://<cluster IP>:<port>` en cada pod de su namespace
  (un «service link»), y uno llamado `docx-to-pdf-probe` hace lo mismo con `DOCX_TO_PDF_PROBE_PORT`. Un valor de esa
  forma (`tcp://`, `udp://` o `sctp://` seguido de una dirección IP y un puerto) en cualquiera de las dos no es un
  ajuste: se ignora con un aviso en el log que nombra la variable, como si la variable no estuviera definida. Cualquier
  otro valor que no sea un puerto sigue deteniendo el arranque. Para usar otro puerto, defínalo explícitamente, o
  ponga `enableServiceLinks: false` en el pod.

## Servidor

| Variable | Valor | Por defecto | Efecto |
| --- | --- | --- | --- |
| `DOCX_TO_PDF_PORT` | puerto | `8080` | El puerto en el que escucha el servicio, en todas las interfaces, para el endpoint de conversión, las comprobaciones de salud y las métricas. Un service link de Kubernetes (`tcp://<ip>:<port>`) aquí se ignora con un aviso, y el puerto pasa a ser `SERVER_PORT` o, si no, `8080`. |
| `SERVER_PORT` | puerto | — | El mismo ajuste con el nombre que usaba moalhaddar/docx-to-pdf. Solo se lee cuando `DOCX_TO_PDF_PORT` no está definida. |
| `DOCX_TO_PDF_PROBE_PORT` | puerto, distinto del puerto del servicio | sin definir: sin sonda | Activa la sonda de plataforma, `POST /probe`, en un puerto propio. La sonda mide lo rápido que convierte esta máquina, para dimensionar un despliegue; mientras se ejecuta retiene todas las instancias de LibreOffice, así que las conversiones reciben 503 y `/readyz` informa de que no está listo. Por eso nunca comparte el puerto de conversión: mantenga este puerto lejos de los clientes. Un service link de Kubernetes aquí se ignora con un aviso, y la sonda sigue desactivada. |
| `DOCX_TO_PDF_IDLE_TIMEOUT` | duración mayor que 0 | `120s` | El tiempo máximo que una petición puede pasar sin recibir un byte de su cabecera o de su cuerpo. Pasado ese tiempo, el cliente recibe 408 y se cierra la conexión. Una subida lenta que sigue enviando se espera lo que tarde en total; el reloj no corre mientras se ejecuta una conversión o se envía su PDF. Una conexión keep-alive sin ninguna petición en curso se cierra tras el mismo tiempo, sin respuesta. |
| `DOCX_TO_PDF_SHUTDOWN_GRACE` | duración, se admite `0s` | el valor de `DOCX_TO_PDF_CONVERSION_TIMEOUT` (`180s`) | Tras `SIGTERM`, cuánto tiempo pueden tardar en terminar las peticiones que ya están en curso. Una petición que sigue en marcha después recibe 503; un PDF que todavía se está enviando se corta. El valor por defecto deja que una conversión que empezó justo antes de la señal llegue hasta su propio límite. |

**La parada tarda como máximo** el periodo de gracia más unos 2 segundos para HTTP, después hasta 5 segundos para
detener las instancias de LibreOffice y, cuando se exporta telemetría, un último vaciado acotado por el tiempo límite
del exportador. Dé al runtime de contenedores más tiempo que eso antes de que mate el proceso: el
`terminationGracePeriodSeconds` de Kubernetes en el periodo de gracia más unos 40 segundos (`220` con los valores por
defecto), o el mismo número a `docker stop -t`.

## El pool de instancias de LibreOffice

El servicio convierte con un pool fijo de procesos de LibreOffice («instancias»), una conversión por instancia a la
vez. Una petición que encuentra todas las instancias ocupadas espera en una cola.

| Variable | Valor | Por defecto | Efecto |
| --- | --- | --- | --- |
| `DOCX_TO_PDF_POOL_SIZE` | número entero, 1 o más | `1` | Cuántas instancias se ejecutan y, por tanto, cuántos documentos se convierten en paralelo. Cada instancia es un proceso de LibreOffice independiente con su propia memoria; consulte `DOCX_TO_PDF_INSTANCE_HARD_MEMORY` para dimensionar el contenedor. También limita cuántos documentos reducen la escala de sus imágenes a la vez (`DOCX_TO_PDF_PICTURE_MAX_BITMAP`), un paso que se ejecuta antes de que un documento ocupe una instancia. |
| `POOL_SIZE` | número entero, 1 o más | — | El tamaño del pool con el nombre que usaba moalhaddar/docx-to-pdf. Solo se lee cuando `DOCX_TO_PDF_POOL_SIZE` no está definida. |
| `pool.size` | número entero, 1 o más | — | La otra grafía de moalhaddar/docx-to-pdf para el tamaño del pool. Solo se lee cuando no está definida ni `DOCX_TO_PDF_POOL_SIZE` ni `POOL_SIZE`. |
| `DOCX_TO_PDF_POOL_MAX_WAITING` | número entero, 0 o más | `16` | Cuántas peticiones pueden esperar a una instancia a la vez. La siguiente recibe 503 de inmediato en lugar de entrar en la cola. `0`: ninguna petición espera; cuando todas las instancias están ocupadas, la respuesta es 503 directamente. |
| `DOCX_TO_PDF_POOL_WAIT_TIMEOUT` | duración, se admite `0s` | `60s` | El tiempo máximo que una petición espera en la cola a que quede libre una instancia, y el tiempo máximo que un documento con una imagen que reducir espera su turno para ello. Pasado ese tiempo, 503. Fije el tiempo de espera de lectura de un cliente por encima de este valor más `DOCX_TO_PDF_CONVERSION_TIMEOUT`, o el cliente abandonará una petición que el servicio todavía respondería. |
| `DOCX_TO_PDF_INSTANCE_START_TIMEOUT` | duración mayor que 0 | `30s` | El tiempo máximo que puede tardar una instancia desde que se arranca hasta que está lista. Una que tarda más se mata, y se intenta otro arranque tras una pausa. Cuando arranca el servicio, una instancia que falla tres veces seguidas detiene el servicio con un error. Normalmente una instancia arranca en bastante menos de un segundo. |
| `DOCX_TO_PDF_CONVERSION_TIMEOUT` | duración mayor que 0 | `180s` | El tiempo máximo que puede durar una conversión. Pasado ese tiempo, la instancia se mata y se sustituye, y el cliente recibe 504. El valor por defecto es unas tres veces el documento más lento medido dentro del límite de tamaño por defecto (un documento de 1000 tablas, alrededor de un minuto en una CPU), de modo que una máquina más lenta o más cargada también lo convierte; un informe típico se convierte en alrededor de un segundo. |
| `DOCX_TO_PDF_INSTANCE_MAX_CONVERSIONS` | número entero, 0 o más | `200` | Una instancia se sustituye por otra nueva tras este número de conversiones; `0` nunca la sustituye por este motivo. Las instancias no han mostrado crecimiento a lo largo de miles de conversiones, así que esto es un seguro; una sustitución cuesta alrededor de un tercio de segundo. |
| `DOCX_TO_PDF_INSTANCE_MAX_MEMORY` | tamaño, se admite `0` | `1GiB` | Una instancia cuya memoria residente supera este valor tras una conversión se sustituye antes de que acepte la siguiente; `0` no fija ese techo. Los informes típicos dejan una instancia en 450 a 650 MiB; un documento muy grande puede dejarla por encima de 1 GiB de forma permanente, y esto lo detecta. |
| `DOCX_TO_PDF_INSTANCE_HARD_MEMORY` | tamaño, se admite `0`; mayor que `DOCX_TO_PDF_INSTANCE_MAX_MEMORY` salvo que uno de los dos sea `0` | `2GiB` | Mientras una instancia convierte, su memoria residente se lee cada 100 ms; por encima de este valor la instancia se mata de inmediato, se sustituye, y la petición recibe 413. `0` no fija ningún límite estricto. Un documento que tumbaría el contenedor entero cuesta, en cambio, una sola petición. Un DOCX u ODT cuyas imágenes, tal como quedarán tras cualquier reducción de escala, suman más que este valor menos 192 MiB (una imagen con transparencia cuenta dos veces y media su mapa de bits) recibe el mismo 413 antes de que se decodifique ninguna imagen. Dimensione la memoria del contenedor a partir de él: **256 MiB + tamaño del pool × (este valor + 256 MiB)**, que son 2.5 GiB para una instancia con el valor por defecto. |

## Límites de lo que puede llevar una petición

| Variable | Valor | Por defecto | Efecto |
| --- | --- | --- | --- |
| `DOCX_TO_PDF_MAX_DOCUMENT_SIZE` | tamaño mayor que 0 | `10MiB` | El documento más grande que se acepta. Por encima, el cliente recibe 413 mientras el cuerpo todavía está llegando, de modo que una subida demasiado grande no se lee hasta el final. La petición completa puede ser 64 KiB mayor, por el encapsulado multipart y otros campos del formulario; un `Content-Length` declarado por encima de eso recibe 413 antes de que se lea nada del cuerpo. Aumentarlo para documentos con muchas imágenes también requiere más memoria por instancia: LibreOffice mantiene decodificadas todas las imágenes mientras convierte, unas 12 veces el tamaño del documento en el caso de fotografías. |
| `DOCX_TO_PDF_MAX_INFLATED_SIZE` | tamaño; `0` para ningún límite, si no, no inferior a `DOCX_TO_PDF_MAX_DOCUMENT_SIZE` | `256MiB`, o 3 × `DOCX_TO_PDF_MAX_DOCUMENT_SIZE` cuando eso es más | Un DOCX u ODT es un archivo ZIP. Cuando sus partes, descomprimidas, suman más que este valor, la petición recibe 413 antes de que empiece ninguna conversión. Las partes se descomprimen de verdad para contarlas; no se confía en los tamaños que declara el archivo. Esto detiene una «bomba zip», un archivo pequeño que se expande a gigabytes. Todo documento corriente dentro del límite de tamaño por defecto queda muy por debajo del valor por defecto. |

## Imágenes

| Variable | Valor | Por defecto | Efecto |
| --- | --- | --- | --- |
| `DOCX_TO_PDF_PICTURE_MAX_BITMAP` | tamaño mayor que 0 | `256MiB` | Un PNG o JPEG dentro de un DOCX u ODT cuyo mapa de bits decodificado (ancho × alto × 1, 3 o 4 bytes por píxel) es mayor que este valor se modifica antes de la conversión: se reduce por la menor potencia de dos que lo deja por debajo del valor o, cuando no puede decodificarse de forma segura, se sustituye por un marcador de posición. La cabecera `Docx-To-Pdf-Pictures` de la respuesta dice qué se modificó. LibreOffice por sí mismo descarta en silencio una imagen de más de 1 073 741 823 bytes decodificada; un valor más alto actúa como ese límite. |
| `DOCX_TO_PDF_PDF_MAX_IMAGE_DPI` | número entero, 1 o más | sin definir: las imágenes tal como están | Una imagen colocada en el documento a una resolución superior a esta se reduce a ella en el PDF. Sin definir, cada imagen se incrusta tal como está (un JPEG byte a byte): un informe típico se convierte con alrededor de un tercio de la CPU, pero un documento con imágenes colocadas por encima de 300 DPI da un PDF más grande. `300` reproduce lo que producía moalhaddar/docx-to-pdf. |

## Logs y telemetría

Los logs siempre van a la salida estándar: un objeto JSON por línea, o texto legible cuando la salida estándar es un
terminal. Cada línea JSON lleva el `trace_id` y el `span_id` de la petición a la que pertenece. Las métricas de
Prometheus siempre se sirven en `GET /metrics`. Las trazas, los logs y las métricas también se exportan por OTLP, el
protocolo de OpenTelemetry, en cuanto se define un endpoint; sin él no se exporta nada.

| Variable | Valor | Por defecto | Efecto |
| --- | --- | --- | --- |
| `RUST_LOG` | un filtro: un nivel (`error`, `warn`, `info`, `debug`, `trace`), o destinos y niveles como `docx_to_pdf=debug,info` | `info` | Qué líneas de log llegan a la salida estándar. No cambia lo que se exporta por OTLP, que es `info` y superior. |
| `OTEL_EXPORTER_OTLP_ENDPOINT` | URL | sin definir: sin exportación | El colector al que enviar las tres señales, p. ej. `http://otel-collector:4318` (HTTP) o `http://otel-collector:4317` (gRPC). Por HTTP se le añade la ruta de cada señal (`/v1/traces`, `/v1/logs`, `/v1/metrics`). |
| `OTEL_EXPORTER_OTLP_<SIGNAL>_ENDPOINT` | URL | sin definir | El colector de una señal, siendo `<SIGNAL>` `TRACES`, `LOGS` o `METRICS`. Activa la exportación de esa señal incluso sin el endpoint general, y se usa tal cual, ruta incluida (`http://collector:4318/v1/traces`). |
| `OTEL_EXPORTER_OTLP_PROTOCOL` | `grpc`, `http/protobuf` o `http/json` | `http/protobuf` | Cómo se envían las señales. |
| `OTEL_EXPORTER_OTLP_<SIGNAL>_PROTOCOL` | como arriba | el protocolo general | El protocolo de una señal. |
| `OTEL_<SIGNAL>_EXPORTER` | `otlp` o `none` | `otlp` | `none` desactiva la exportación de una señal mientras las demás siguen activas (`OTEL_TRACES_EXPORTER`, `OTEL_LOGS_EXPORTER`, `OTEL_METRICS_EXPORTER`). Cualquier otro valor también la desactiva, con un aviso en el log. |
| `OTEL_SDK_DISABLED` | `true` o `false`, sin distinguir mayúsculas y minúsculas | `false` | `true` desactiva toda exportación OTLP. Los logs en la salida estándar y `/metrics` se mantienen. |
| `OTEL_SERVICE_NAME` | texto | `docx-to-pdf` | El `service.name` que lleva cada señal. |
| `OTEL_RESOURCE_ATTRIBUTES` | pares `key=value` separados por comas | — | Atributos adicionales en cada señal, como `deployment.environment=prod`. El servicio añade los suyos, `service.version` y `upstream.version`. |
| `OTEL_EXPORTER_OTLP_HEADERS` | pares `key=value` separados por comas, con los valores codificados para URL | — | Cabeceras que se envían con cada exportación, para un colector que pide un token. |
| `OTEL_EXPORTER_OTLP_<SIGNAL>_HEADERS` | como arriba | las cabeceras generales | Cabeceras para la exportación de una señal. |
| `OTEL_EXPORTER_OTLP_TIMEOUT` | milisegundos | `10000` | El tiempo máximo que puede tardar una exportación. También acota el último vaciado en la parada. |
| `OTEL_EXPORTER_OTLP_<SIGNAL>_TIMEOUT` | milisegundos | el tiempo límite general | Lo mismo, para una señal. |

La compresión de la exportación no está incluida: `OTEL_EXPORTER_OTLP_COMPRESSION` se ignora con un aviso, y las
exportaciones van sin comprimir. Las demás variables estándar del SDK de OpenTelemetry (el agrupamiento por lotes,
como `OTEL_BSP_SCHEDULE_DELAY`, el muestreo con `OTEL_TRACES_SAMPLER`, el intervalo de métricas
`OTEL_METRIC_EXPORT_INTERVAL`) conservan el significado que les da la especificación de OpenTelemetry; el servicio no
define ninguna de ellas por su cuenta.

## El sistema

| Variable | Valor | Por defecto | Efecto |
| --- | --- | --- | --- |
| `TMPDIR` | la ruta de un directorio | `/tmp` | Dónde escribe el servicio mientras trabaja: el perfil de LibreOffice de cada instancia, el documento subido, el PDF de camino a la salida y las imágenes cuya escala se está reduciendo. Una conversión puede ocupar ahí a la vez hasta tres veces el tamaño del documento, o 256 MiB cuando los documentos son pequeños. La imagen del contenedor lo fija en `/tmp`; monte ahí un volumen con permiso de escritura cuando el sistema de archivos raíz sea de solo lectura. |

## Ajustes que no se leen

moalhaddar/docx-to-pdf era una aplicación Spring Boot, y sus despliegues suelen llevar ajustes de Spring y de la JVM.
Estos se ignoran, así que elimínelos o tradúzcalos a las filas de arriba: `JAVA_TOOL_OPTIONS`, todos los `SPRING_*`
(los límites de tamaño multipart pasan a ser `DOCX_TO_PDF_MAX_DOCUMENT_SIZE`), todos los `SERVER_TOMCAT_*` (el tiempo
límite de conexión pasa a ser `DOCX_TO_PDF_IDLE_TIMEOUT`, como duración en lugar de milisegundos), todos los
`MANAGEMENT_*` y `LOGGING_LEVEL_*` (pasa a ser `RUST_LOG`).
