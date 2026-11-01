# Solución de problemas

<!-- languages:begin -->
[English](../../../guide/troubleshooting.md) · [Русский](../../ru/guide/troubleshooting.md) ·
[中文](../../zh/guide/troubleshooting.md) · **Español**
<!-- languages:end -->
<!-- translation of docs/guide/troubleshooting.md: 00faa880a539 -->

Lo que encuentra un cliente o un operador cuando algo va mal, lo que significa y lo que hay que cambiar. La primera
parte tiene una entrada por cada error que responde el servicio, encabezada por su estado y su `detail`, para que
pueda buscar en esta página la frase de la respuesta que recibió. La segunda parte trata los problemas que no son una
respuesta de error: un pod que muere por falta de memoria, imágenes modificadas, fuentes incorrectas, conversiones
lentas. Cada entrada nombra el ajuste que cambiar, la métrica que vigilar o la línea de log que buscar. Una prueba
mantiene la página fiel al servicio: cada error de la
[Referencia de la API](api.md#todos-los-errores-que-responde-el-servicio) tiene aquí una entrada, y cada ajuste,
métrica y línea de log que se nombra abajo existe.

**Dónde mirar.** Los ajustes están en [Configuración](configuration.md); las métricas se sirven en `GET /metrics` y
se enumeran en [Operaciones: métricas](operations.md#métricas). Los logs son un objeto JSON por línea en la salida
estándar; aquí una línea de log se cita por su campo de mensaje, como
`"message":"the conversion exceeded its timeout"`, para que pueda buscarla en los logs. Cada línea escrita mientras se
atendía una petición lleva el `trace_id` de esa petición.

Un contador de Prometheus por respuesta: `http_server_request_duration_seconds_count` con la etiqueta
`http_response_status_code` cuenta cada petición por su estado, y `docx_to_pdf_conversions_total` cuenta las
conversiones según cómo terminaron.

## Errores

### 400 `the body is not multipart/form-data with a boundary`

La petición no es una subida de formulario. El documento debe enviarse como `multipart/form-data`, en una parte de
archivo llamada `document`; un cuerpo en bruto (`curl --data-binary @report.docx`), un JSON con el documento en
base64 o una cabecera `Content-Type: multipart/form-data` escrita a mano sin su `boundary=` reciben esta respuesta. Con
`curl`, use `-F document=@report.docx` y deje que escriba la cabecera; en el código, deje que la biblioteca HTTP
construya el cuerpo multipart en lugar de fijar `Content-Type` usted mismo. Cuéntelas con
`http_server_request_duration_seconds_count{http_response_status_code="400"}`.

### 400 `the multipart body is malformed`

El cuerpo dice ser multipart pero no puede analizarse: un boundary que no coincide con el de la cabecera, o un cuerpo
cortado antes de su boundary de cierre, normalmente un cliente o un proxy que abandonó a mitad de la subida. Revise la
biblioteca del cliente y cualquier cosa entre él y el servicio que reescriba los cuerpos. Aparece en
`http_server_request_duration_seconds_count{http_response_status_code="400"}`.

### 400 `the body has no file part named "document"`

El formulario no tiene ninguna parte de archivo llamada `document`. O el campo tiene otro nombre (`file`, `upload`),
o es un campo de formulario simple sin nombre de archivo: `curl -F document=<report.docx` (con `<`) envía el
contenido del archivo como texto y se rechaza, `curl -F document=@report.docx` (con `@`) envía un archivo. Cambie el
nombre del campo, o haga que la parte sea una parte de archivo. Aparece en
`http_server_request_duration_seconds_count{http_response_status_code="400"}`.

### 404 `no such path; the service converts at POST /pdf and POST /docx-to-pdf`

La ruta no existe. El servicio solo responde a las rutas enumeradas en [la Referencia de la API](api.md#rutas): un
cliente o una comprobación de salud que todavía apunta a otra ruta, como el `/actuator/health` de Spring Boot, recibe
`404`. Use `/livez` y `/readyz` para las comprobaciones de salud. Cuéntelas con
`http_server_request_duration_seconds_count{http_response_status_code="404"}`.

### 405 `this path does not take this method; see Allow`

La ruta existe, pero no con este método: un `GET` a `/pdf`, un `POST` a `/readyz`. La cabecera `Allow` de la
respuesta nombra los métodos que acepta la ruta. Un `HEAD` a una ruta de conversión también recibe este estado, lo que
importa para una comprobación de salud de un balanceador de carga configurada con `HEAD /pdf`: apúntela a `/readyz`.
Aparece en `http_server_request_duration_seconds_count{http_response_status_code="405"}`.

### 408 `no part of the request arrived within the idle limit`

El cliente empezó una petición y después no envió nada, ni un solo byte, durante
[`DOCX_TO_PDF_IDLE_TIMEOUT`](configuration.md#servidor) (2 minutos por defecto). Una subida lenta que sigue enviando
nunca se corta; se trata de un cliente que se quedó parado o de una conexión que murió por el camino. La conexión se
cierra tras la respuesta. El servicio registra `"message":"closing a connection idle past the limit"`. Si clientes
legítimos se quedan parados tanto tiempo (una red móvil, una subida que se detiene mientras el cliente lee el archivo
de un almacenamiento lento), aumente `DOCX_TO_PDF_IDLE_TIMEOUT`.

### 413 `the document or the request is over the size limit`

El documento es mayor que
[`DOCX_TO_PDF_MAX_DOCUMENT_SIZE`](configuration.md#límites-de-lo-que-puede-llevar-una-petición) (10 MiB por defecto),
o la petición completa es mayor que eso más 64 KiB. Aumente el límite si se esperan documentos así;
`docx_to_pdf_document_size_bytes` muestra los tamaños que recibe realmente. Un documento con muchas fotografías
necesita más memoria por instancia a medida que crece (consulte dos entradas más abajo), así que aumente
`DOCX_TO_PDF_INSTANCE_HARD_MEMORY` junto con él si esos documentos reciben entonces el `413` de memoria.

Un `413` cuyo cuerpo no es un documento de problema (HTML, o un `Content-Type` distinto de
`application/problem+json`) viene de un proxy delante del servicio, no del servicio: aumente el límite de cuerpo de
ese proxy, como el `client_max_body_size` de nginx, que es de 1 MiB por defecto.

La sonda de plataforma responde el mismo `413` cuando una petición lleva más de 16 documentos.

### 413 `the document's compressed parts inflate past the size limit`

Un DOCX u ODT es un archivo ZIP, y las partes de este, descomprimidas, suman más que
[`DOCX_TO_PDF_MAX_INFLATED_SIZE`](configuration.md#límites-de-lo-que-puede-llevar-una-petición) (256 MiB por
defecto). Ese es el aspecto de una «bomba zip», y ningún documento corriente dentro del límite de tamaño por defecto
se le acerca. El servicio registra `"message":"the document's parts inflate past the limit; refused before conversion"`
con los tamaños que encontró. Si se rechaza un documento auténtico, aumente `DOCX_TO_PDF_MAX_INFLATED_SIZE`; no puede
fijarse por debajo del límite de tamaño de documento.

### 413 `the document needs more memory to convert than the service allows`

La memoria de LibreOffice superó
[`DOCX_TO_PDF_INSTANCE_HARD_MEMORY`](configuration.md#el-pool-de-instancias-de-libreoffice) (2 GiB por defecto)
mientras convertía este documento, así que su instancia se mató y se sustituyó, y las demás conversiones siguieron.
La petición no se reintenta: el mismo documento mataría también la siguiente instancia. Los logs muestran
`"message":"the instance passed the hard memory limit during a conversion; killing it, the request fails"` y
`"message":"the conversion passed the hard memory limit"`, con la memoria que había alcanzado la instancia; la métrica
es `docx_to_pdf_conversions_total{outcome="memory_limit"}`.

La misma respuesta llega antes de cualquier conversión cuando las imágenes de un DOCX u ODT, cada una tal como quedará
tras cualquier reducción de escala, suman más de lo que deja el límite estricto (el límite menos 192 MiB; una imagen
con transparencia cuenta dos veces y media su mapa de bits): no se toca ninguna instancia y la respuesta tarda
milisegundos. El log muestra
`"message":"the document's pictures, after the planned downscale, need more memory than the hard limit leaves; refused before any is decoded"` <!-- markdownlint-disable-line MD013 --><!-- a log message, quoted whole -->
con `held_bytes`, `budget_bytes` y el número de imágenes. El remedio es el mismo: aumente el límite estricto, o
reduzca [`DOCX_TO_PDF_PICTURE_MAX_BITMAP`](configuration.md#imágenes) para que cada imagen demasiado grande se reduzca
más.

Lo que ocupa la memoria son sobre todo las imágenes: LibreOffice mantiene decodificadas todas las imágenes mientras
convierte, unas 12 veces el tamaño del documento en el caso de fotografías. Para convertir esos documentos, aumente
`DOCX_TO_PDF_INSTANCE_HARD_MEMORY`, y con él el límite de memoria del contenedor, según la fórmula de
[Operaciones: Kubernetes](operations.md#kubernetes): 256 MiB + tamaño del pool × (el límite estricto + 256 MiB).
`docx_to_pdf_instance_memory_peak_bytes` muestra cuánto se acercan sus documentos al límite.

### 415 `the document is not DOCX, DOC, ODT or RTF`

Los bytes del documento no están en ninguno de [los formatos aceptados](api.md#los-formatos-aceptados). El nombre del
archivo y el `Content-Type` de la parte se ignoran, así que cambiar el nombre de un archivo no cambia la respuesta.
Los casos habituales:

- una hoja de cálculo, una presentación, un PDF o una imagen;
- un archivo vacío, a menudo de un cliente que envió la parte antes de que se escribiera el archivo;
- un archivo «Word» exportado por una aplicación web que en realidad es HTML o texto plano con un nombre `.doc`;
- un DOCX protegido con contraseña de apertura: Word lo guarda cifrado dentro de un archivo compuesto que no contiene
  ningún flujo de documento de Word;
- un RTF que empieza por una marca de orden de bytes UTF-8.

Abra el archivo en un procesador de textos y vuelva a guardarlo como DOCX. Cuéntelos con
`docx_to_pdf_conversions_total{outcome="unsupported"}`.

### 422 `the document could not be loaded`

El formato es uno de los que acepta el servicio, pero LibreOffice no puede abrir este documento: está truncado, dañado
o cifrado (un documento de Word 97-2003 con contraseña de apertura). El servicio registra
`"message":"the backend could not load the document"` con el error del propio LibreOffice en el campo
`backend_error`. Un reintento da la misma respuesta. Si Word abre el archivo, vuelva a guardarlo desde Word como DOCX
y convierta ese. Cuéntelos con `docx_to_pdf_conversions_total{outcome="not_loaded"}`.

### 500 `the service failed to answer this request`

Falló algo que no es culpa del documento, o no solo suya. La línea de log de nivel `ERROR` con el `trace_id` de la
petición dice qué:

- `"message":"the conversion got no answer"`: la instancia de LibreOffice murió durante la conversión, y de nuevo en
  el reintento automático en otra instancia (`docx_to_pdf_conversions_total{outcome="dead_instance"}`). Cada muerte
  registra además `"message":"the instance died during a conversion; retrying once on another instance"`. Un
  documento que tumba LibreOffice dos veces muy probablemente lo volverá a hacer; si muchos documentos lo hacen,
  revise la memoria de las instancias (`docx_to_pdf_instance_memory_usage_bytes`) y los mensajes de falta de memoria
  del kernel.
- `"message":"the backend could not export the PDF"`: LibreOffice abrió el documento pero no pudo escribir su PDF.
- `"message":"the spooled document could not be read back"` o
  `"message":"the document's pictures could not be checked"`: el servicio no pudo usar su directorio temporal.
  Compruebe que [`TMPDIR`](configuration.md#el-sistema) (`/tmp` en la imagen) admite escritura y no está lleno; con un
  sistema de archivos raíz de solo lectura necesita un volumen propio (consulte
  [Operaciones: `/tmp`](operations.md#tmp-en-disco-o-en-memoria)).

### 503 `the service is not ready to convert documents yet`

El servicio acaba de arrancar y ninguna de sus instancias de LibreOffice está lista todavía. La respuesta lleva
`Retry-After: 30`. Una comprobación de readiness en `/readyz` mantiene el tráfico alejado hasta que la primera
instancia está lista, normalmente en uno o dos segundos desde el arranque; si las peticiones reciben esta respuesta,
el balanceador de carga envía tráfico antes de que `/readyz` sea `200`. Si el servicio se queda en este estado,
consulte [El servicio no arranca](#el-servicio-no-arranca-o-nunca-llega-a-estar-listo);
`docx_to_pdf_pool_instances{state="starting"}` muestra las instancias que todavía están arrancando.

### 503 `no instance came free within the wait limit`

Todas las instancias siguieron ocupadas durante
[`DOCX_TO_PDF_POOL_WAIT_TIMEOUT`](configuration.md#el-pool-de-instancias-de-libreoffice) (60 segundos por defecto)
mientras esta petición esperaba en la cola. Al servicio le falta capacidad para la carga que recibe:
`docx_to_pdf_pool_instances{state="idle"}` se queda en 0 y `docx_to_pdf_pool_wait_duration_seconds` crece hacia el
límite de espera; `docx_to_pdf_pool_wait_duration_seconds_count{outcome="rejected"}` cuenta estas respuestas. Añada
réplicas, o dé a cada una más CPU y un `DOCX_TO_PDF_POOL_SIZE` acorde (una instancia por núcleo; más instancias que
núcleos hacen más lenta cada conversión y no añaden nada).
[Operaciones: dimensionamiento](operations.md#dimensionamiento) calcula los números. La respuesta lleva
`Retry-After: 10`, y un cliente que reintenta después recibe su PDF en cuanto pasa la ráfaga.

Un documento con una imagen que reducir recibe la misma respuesta cuando, durante toda la espera, tantos otros
documentos como `DOCX_TO_PDF_POOL_SIZE` estuvieron reduciendo las suyas: ese paso se ejecuta antes de que un documento
ocupe una instancia y tiene el mismo límite. El log muestra entonces
`"message":"no turn of the pictures step came free within the wait limit"`.

### 503 `too many requests are already waiting for an instance`

Ya había [`DOCX_TO_PDF_POOL_MAX_WAITING`](configuration.md#el-pool-de-instancias-de-libreoffice) peticiones (16 por
defecto) en la cola, así que esta se rechazó de inmediato en lugar de entrar en ella. La causa y el remedio son los de
la entrada anterior: muy poca capacidad para la carga. Aumentar `DOCX_TO_PDF_POOL_MAX_WAITING` solo alarga la cola, y
cada petición en cola espera más. `docx_to_pdf_pool_wait_duration_seconds_count{outcome="rejected"}` también cuenta
estas respuestas. `Retry-After: 10`.

### 503 `the service is shutting down`

El servicio recibió `SIGTERM` (una actualización gradual, una reducción de escala, el vaciado de un nodo) y ya no
acepta conversiones. Una petición que todavía no había llegado a una instancia recibe `Retry-After: 10`; una que
seguía convirtiendo cuando se agotó [`DOCX_TO_PDF_SHUTDOWN_GRACE`](configuration.md#servidor) recibe
`Retry-After: 1`, y el servicio registra
`"message":"the grace period ran out; cutting the requests still in flight short"`. Un cliente que reintenta llega a
otra réplica.

Las peticiones que llegan después de la señal significan que el balanceador de carga todavía dirige tráfico al pod: en
Kubernetes, añada la espera `preStop` de [Operaciones: parada](operations.md#parada). Las peticiones cortadas al final
del periodo de gracia significan que el periodo de gracia es más corto que sus conversiones: deje
`DOCX_TO_PDF_SHUTDOWN_GRACE` en su valor por defecto, el tiempo límite de conversión, y dé al pod un
`terminationGracePeriodSeconds` del periodo de gracia más unos 40 segundos. La sonda de plataforma responde el mismo
`503`, con `Retry-After: 30`, cuando se envía durante una parada.

### 503 `the service is measuring its platform; another replica can convert meanwhile`

Alguien está ejecutando la sonda de plataforma en esta réplica, y la sonda retiene todas las instancias durante uno o
dos minutos. El servicio registra `"message":"the pool is reserved for the platform probe; draining it"` cuando
empieza y `"message":"the platform probe has given the pool back"` cuando termina. Sondee una réplica que no reciba
tráfico, y mantenga [`DOCX_TO_PDF_PROBE_PORT`](configuration.md#servidor) lejos de los clientes; elimínela cuando no
esté midiendo.

### 504 `the conversion did not finish within the conversion timeout`

LibreOffice seguía convirtiendo después de
[`DOCX_TO_PDF_CONVERSION_TIMEOUT`](configuration.md#el-pool-de-instancias-de-libreoffice) (3 minutos por defecto),
así que su instancia se mató y se sustituyó. Los logs muestran
`"message":"the conversion did not end within the timeout; killing its instance"` y
`"message":"the conversion exceeded its timeout"`; la métrica es `docx_to_pdf_conversions_total{outcome="timeout"}`.

El valor por defecto es unas tres veces el documento más lento medido dentro del límite de tamaño por defecto, así
que un `504` con un documento corriente suele significar que la máquina es más lenta de lo que debería: más
instancias que núcleos de CPU (`DOCX_TO_PDF_POOL_SIZE` por encima del límite de CPU, o un límite de CPU que estrangula
el pod), de modo que cada conversión recibe una fracción de núcleo. Compare `docx_to_pdf_conversion_duration_seconds`
con el tiempo que tarda el mismo documento en una réplica sin carga. Para documentos que son lentos de verdad
(cientos de tablas, miles de páginas), aumente `DOCX_TO_PDF_CONVERSION_TIMEOUT`, y los tiempos de espera de lectura de
los clientes por encima de él.

## Errores de la sonda de plataforma

La sonda, `POST /probe` en [`DOCX_TO_PDF_PROBE_PORT`](configuration.md#servidor), también responde el `413` de tamaño
y el `503` de parada de arriba. Cómo usarla está en
[Operaciones: medir una plataforma](operations.md#medir-una-plataforma-la-sonda-de-dimensionamiento).

### 400 `rounds, busy, cpus and disk_mib are whole numbers within their bounds`

Un parámetro de consulta no es un número entero, o supera su límite: `rounds` hasta 100, `busy` hasta 10000,
`disk_mib` hasta 65536, `cpus` de 1 a 1024. Consulte [los parámetros de la sonda](api.md#la-sonda-de-plataforma). La
sonda solo escucha cuando `DOCX_TO_PDF_PROBE_PORT` está definida.

### 400 `the probe takes rounds, busy, disk_mib, cpus and name`

La consulta tiene un parámetro que la sonda no conoce, a menudo una errata como `cpu=4` por `cpus=4`. La sonda solo
escucha cuando `DOCX_TO_PDF_PROBE_PORT` está definida.

### 422 `a probe document could not be loaded`

LibreOffice no puede abrir uno de los documentos enviados a la sonda; la sonda no mide nada hasta que se convierten
todos los documentos. Convierta primero cada documento a través de `POST /pdf`: el que reciba ahí
[`422`](#422-the-document-could-not-be-loaded) es el que hay que sustituir. El servicio registra
`"message":"the backend could not load the document"` por él.

### 503 `a probe is already running`

Otra sonda retiene las instancias de esta réplica. Espere a que responda (uno o dos minutos) y envíe la suya después;
la respuesta lleva `Retry-After: 30`. El servicio registra `"message":"the platform probe has given the pool back"`
cuando termina la que está en marcha.

### 503 `the conversions under way did not end in time`

Antes de tomar las instancias, la sonda espera a que terminen las conversiones que ya se están ejecutando. No
terminaron dentro de [`DOCX_TO_PDF_CONVERSION_TIMEOUT`](configuration.md#el-pool-de-instancias-de-libreoffice) más
`DOCX_TO_PDF_INSTANCE_START_TIMEOUT`. La réplica está ocupada con tráfico: sondee una que no reciba ninguno.

## Otros problemas

### El servicio no arranca o nunca llega a estar listo

**Sale de inmediato con una línea en la salida de error estándar**, `docx-to-pdf:` y un espacio, seguidos de la
variable y de lo que está mal en su valor: un ajuste tiene un valor que el servicio no puede usar
(`DOCX_TO_PDF_IDLE_TIMEOUT=30` sin unidad, por ejemplo). Corrija el valor;
[Configuración](configuration.md#cómo-se-leen-los-valores) dice qué admite cada uno.

**Registra `"message":"cannot listen"` y sale**: el puerto de `DOCX_TO_PDF_PORT` está ocupado, o es inferior a 1024
para un usuario que no es root.

**Registra `"message":"instance start failed"` o `"message":"instance start timed out; killed"`**, después
`"message":"retrying a failed first start"`, y tras tres fallos seguidos `"message":"the pool could not start"` y
sale: LibreOffice no puede arrancar. Lo más frecuente es que su directorio temporal no admita escritura: con un
sistema de archivos raíz de solo lectura, `/tmp` necesita un volumen (consulte
[Operaciones: el contenedor](operations.md#el-contenedor)). En una máquina muy lenta o muy cargada, una instancia
puede necesitar más que `DOCX_TO_PDF_INSTANCE_START_TIMEOUT` (30 segundos); normalmente una arranca en bastante menos
de un segundo.

**`/readyz` sigue en `503` mientras el proceso se ejecuta**: las instancias todavía están arrancando o siguen
fallando al hacerlo. `docx_to_pdf_pool_instances{state="starting"}` y
`docx_to_pdf_instance_starts_total{reason="failed"}` muestran cuál de las dos cosas ocurre. Una comprobación de
arranque de Kubernetes debe conceder más de tres veces `DOCX_TO_PDF_INSTANCE_START_TIMEOUT`; consulte
[Operaciones: comprobaciones de salud](operations.md#comprobaciones-de-salud).

### El servicio escucha en un puerto distinto del configurado, o la sonda no arranca

Un Service de Kubernetes llamado `docx-to-pdf` (o `docx-to-pdf-probe`) en el mismo namespace hace que Kubernetes fije
`DOCX_TO_PDF_PORT` (o `DOCX_TO_PDF_PROBE_PORT`) en `tcp://<ip>:<port>` en cada pod. El servicio ignora un valor de esa
forma con un aviso al arrancar, como si la variable no estuviera definida. Defina el puerto explícitamente y ponga
`enableServiceLinks: false` en el pod; consulte [Cómo se leen los valores](configuration.md#cómo-se-leen-los-valores).

### El cliente abandona antes de la respuesta

Un cliente cuyo tiempo de espera de lectura es más corto que lo que puede tardar legítimamente una petición abandona
peticiones que el servicio todavía respondería; el servicio registra entonces
`"message":"the client went away before the answer; the conversion is cancelled"` y cuenta
`docx_to_pdf_conversions_total{outcome="cancelled"}`. Una petición puede esperar hasta
`DOCX_TO_PDF_POOL_WAIT_TIMEOUT` a una instancia y después convertir durante hasta `DOCX_TO_PDF_CONVERSION_TIMEOUT`:
fije el tiempo de espera de lectura del cliente, y el de cada proxy del camino, por encima de la suma (240 segundos
con los valores por defecto), o reduzca los dos ajustes.

### El pod muere por falta de memoria (OOMKilled)

El servicio acota la memoria de cada instancia con `DOCX_TO_PDF_INSTANCE_HARD_MEMORY`, así que un contenedor
dimensionado con la fórmula 256 MiB + tamaño del pool × (`DOCX_TO_PDF_INSTANCE_HARD_MEMORY` + 256 MiB) no muere por
culpa de un documento. Cuando muere de todos modos, se cumple una de estas condiciones:

- el límite de memoria está por debajo de la fórmula, a menudo porque se aumentó `DOCX_TO_PDF_POOL_SIZE` sin
  aumentarlo a él;
- `DOCX_TO_PDF_INSTANCE_HARD_MEMORY` es `0`, lo que desactiva el límite;
- `/tmp` es un tmpfs (`emptyDir` con `medium: Memory`): lo que se escribe ahí cuenta para la memoria del contenedor,
  así que su tamaño debe sumarse al límite, o `/tmp` debe pasar a disco.

`docx_to_pdf_instance_memory_usage_bytes` muestra la memoria de cada instancia y `process_memory_usage_bytes` la del
propio proceso del servicio. Consulte [Operaciones: Kubernetes](operations.md#kubernetes) para ver los números
calculados.

### Las instancias se sustituyen a menudo

Cada sustitución se cuenta en `docx_to_pdf_instance_starts_total` con su motivo. `recycle` es rutina: una instancia se
sustituye tras `DOCX_TO_PDF_INSTANCE_MAX_CONVERSIONS` conversiones, o cuando su memoria se queda por encima de
`DOCX_TO_PDF_INSTANCE_MAX_MEMORY` después de una (registrado como
`"message":"the instance is above the memory ceiling; recycling it"`), y cuesta alrededor de un tercio de segundo.
`memory_limit` y `timeout` siguen a un `413` o a un `504` (arriba). `crash` significa que LibreOffice murió; la
petición se reintentó, y solo sigue un `500` cuando el reintento también muere. Un flujo constante de `crash` con
documentos corrientes merece un informe, con las líneas de log de una de las veces.

### Una imagen del PDF es más pequeña, está borrosa o es un marcador gris

La respuesta lleva entonces una cabecera como `Docx-To-Pdf-Pictures: downscaled=1, replaced=0`. Un PNG o JPEG cuyo
mapa de bits decodificado es mayor que [`DOCX_TO_PDF_PICTURE_MAX_BITMAP`](configuration.md#imágenes) (256 MiB por
defecto, unos 90 megapíxeles de color RGB) se reduce antes de la conversión, porque de lo contrario LibreOffice lo
descartaría del PDF. El servicio registra
`"message":"a picture's decoded bitmap passes the threshold; downscaled before conversion"` con el tamaño de la imagen
y el factor, y `docx_to_pdf_pictures_total{outcome="downscaled"}` las cuenta.

Una imagen que no puede reducirse de forma segura se sustituye en cambio por un marcador de posición, registrado como
`"message":"a picture's decoded bitmap passes the threshold; replaced by a placeholder before conversion"` con el
motivo en su campo `why` (un PNG entrelazado, una imagen dañada, una codificación JPEG que el servicio no decodifica)
y contado como `docx_to_pdf_pictures_total{outcome="replaced"}`. Vuelva a guardar esa imagen en un formato corriente y
con un tamaño razonable (una imagen de 20000 × 20000 píxeles está muy por encima de lo que imprime una página) y
vuelva a ponerla en el documento.

Aumentar `DOCX_TO_PDF_PICTURE_MAX_BITMAP` conserva tal cual imágenes más grandes, pero LibreOffice mantiene cada una
decodificada mientras convierte, así que aumente `DOCX_TO_PDF_INSTANCE_HARD_MEMORY` junto con él. Por encima de
aproximadamente 1 GiB, LibreOffice descarta una imagen sea cual sea el ajuste.

### Falta una imagen en el PDF y su marco está vacío

El servicio solo comprueba las imágenes de los archivos PNG y JPEG de un DOCX, DOCM, DOTX, ODT u OTT. Una imagen muy
grande en un DOC, un RTF o un ODT plano, o en otro formato de imagen, llega a LibreOffice tal cual, y LibreOffice
descarta una cuyo mapa de bits decodificado supera aproximadamente 1 GiB. Guarde el documento como DOCX, o la imagen
más pequeña. Cuando falla la propia comprobación de imágenes, el documento se convierte sin cambios; el servicio
registra `"message":"the pictures step panicked; the document is converted as it was received"` y cuenta
`docx_to_pdf_pictures_total{outcome="failed"}`.

### El PDF es más grande de lo esperado

Por defecto, cada imagen se incrusta tal como está en el documento, un JPEG byte a byte, así que una fotografía
colocada en pequeño en la página conserva toda su resolución. Fije
[`DOCX_TO_PDF_PDF_MAX_IMAGE_DPI`](configuration.md#imágenes) en `300` para reducir las imágenes de más de 300 DPI tal
como están colocadas, como hacía moalhaddar/docx-to-pdf; cuesta CPU en los documentos con imágenes.
`docx_to_pdf_pdf_size_bytes` muestra los tamaños de los PDF que responde.

### El texto del PDF sale en otra fuente, se redistribuye o muestra cuadros vacíos

El PDF usa las fuentes que nombra el documento cuando están disponibles, y un sustituto cuando no lo están. La imagen
lleva sustitutos con los mismos anchos para Calibri, Cambria, Arial, Times New Roman, Courier New y Arial Narrow, así
que esas fuentes conservan sus saltos de línea y de página; cualquier otra fuente que falte se sustituye por una de
anchos distintos, y el texto se redistribuye. Los cuadros vacíos, o un glifo de la fuente de otra escritura,
significan que ninguna fuente disponible tiene el carácter.

Para ver qué fuentes usa un PDF, ejecute `pdffonts` (de Poppler) sobre él; para ver a qué se resuelve el nombre de
una fuente dentro de la imagen:

```sh
docker run --rm --entrypoint fc-match quay.io/oxidestack/docx-to-pdf:1.2.0 'Segoe UI'
```

La solución es darle la fuente al servicio: monte los archivos de fuentes para los que tenga licencia en
`/usr/share/fonts/custom`, como se describe en [Operaciones: montar las suyas](operations.md#montar-las-suyas), y
reinicie el servicio; las fuentes se leen cuando arranca una instancia.

### La primera conversión es más lenta que las siguientes

Cada instancia de LibreOffice carga parte de su código en su primera conversión en lugar de al arrancar, así que el
primer documento que convierte una instancia tarda notablemente más que el mismo documento después; una instancia
nueva lo vuelve a pagar, tras cada sustitución (`docx_to_pdf_instance_starts_total{reason="recycle"}`, cada
`DOCX_TO_PDF_INSTANCE_MAX_CONVERSIONS` conversiones por defecto). Con fuentes montadas, la primera instancia tras un
cambio construye además su caché de fuentes al arrancar. Mida con `docx_to_pdf_conversion_duration_seconds`, que es
el tiempo propio de LibreOffice, y deje la primera conversión de cada instancia fuera de un benchmark. Si la primera
petición tras un despliegue tiene que ser rápida, envíe un documento pequeño por instancia en cuanto `/readyz` pase a
`200`.

### Todas las conversiones son lentas

Un informe de empresa típico se convierte en alrededor de un segundo en un núcleo. Cuando todo va más lento:

- **Más instancias que núcleos.** Un `DOCX_TO_PDF_POOL_SIZE` por encima de las CPU que puede usar el contenedor, o un
  límite de CPU que lo estrangula, hace que cada conversión comparta un núcleo. Mantenga el tamaño del pool igual a
  la petición de CPU.
- **Tiempo de espera, no de conversión.** Compare el tiempo de la petición, `http_server_request_duration_seconds`,
  con `docx_to_pdf_conversion_duration_seconds` y `docx_to_pdf_pool_wait_duration_seconds`: una gran diferencia es la
  cola (hace falta más capacidad) o la subida y la descarga (la red).
- **Imágenes reducidas en el PDF.** `DOCX_TO_PDF_PDF_MAX_IMAGE_DPI` hace que LibreOffice vuelva a codificar las
  imágenes, lo que cuesta CPU; sin definir, las imágenes se copian tal como están.

Para saber lo que puede hacer una máquina, mídala con la sonda de plataforma; consulte
[Operaciones: dimensionamiento](operations.md#dimensionamiento).
