# Migrar desde moalhaddar/docx-to-pdf

<!-- languages:begin -->
[English](../../../guide/migration.md) · [Русский](../../ru/guide/migration.md) · [中文](../../zh/guide/migration.md)
· **Español**
<!-- languages:end -->
<!-- translation of docs/guide/migration.md: dc32e801d4cb -->

Para quien ejecuta `moalhaddar/docx-to-pdf` (2.1.0-x) o lo llama, y quiere pasar a este servicio. Convierte los mismos
documentos a través de la misma interfaz, con un LibreOffice actual, sin los modos de fallo del original y con
OpenTelemetry integrado. Esta página dice qué sigue igual, qué cambia un cliente, qué cambia un despliegue y, al final,
cada diferencia deliberada con su motivo. Una prueba mantiene la página fiel a la lista de diferencias y a las demás
páginas de la guía, de modo que no puede existir una diferencia sin una fila aquí.

## Lo que sigue igual

- El puerto **8080**.
- `POST /pdf` y `POST /docx-to-pdf`, un cuerpo `multipart/form-data` con el documento en la parte de archivo llamada
  **`document`**. El nombre de archivo y el `Content-Type` de la parte no intervienen.
- El éxito es **`200`**, `Content-Type: application/pdf`, con el PDF como cuerpo.
- Las peticiones de origen cruzado a esas dos rutas: `Access-Control-Allow-Origin: *`, y una petición preliminar
  respondida con `Access-Control-Allow-Methods: POST`, las cabeceras pedidas permitidas y
  `Access-Control-Max-Age: 1800`.
- Los documentos que se convierten: DOCX, DOC, ODT y RTF, y las variantes de la familia Word que el original
  convierte de verdad: DOCM, DOTX, OTT y ODT plano ([los formatos aceptados](api.md#los-formatos-aceptados)).
- El aspecto del PDF: las mismas páginas, los mismos tamaños de página y las mismas palabras. El PDF no es idéntico
  byte a byte, porque LibreOffice es más reciente.
- Las fuentes libres que incluía el original, y los mismos sustitutos para las fuentes habituales de Word, como
  Calibri, Arial y Times New Roman ([Fuentes](operations.md#fuentes)).

Un cliente que envía un documento, lee el PDF de vuelta y trata cualquier otra respuesta como un fallo no necesita
ningún cambio.

## Lo que cambia un cliente

### Estados

El original respondía a la mayoría de los errores con `500`, y a algunas entradas que no son documentos con un PDF.
Este servicio responde a cada situación con su propio estado, y cada cuerpo de error es un documento de problema
([Errores](api.md#errores)).

| Situación | moalhaddar/docx-to-pdf | Este servicio | ¿Reintentar? |
| --- | --- | --- | --- |
| El cuerpo no es multipart, está mal formado o no tiene ninguna parte de archivo llamada `document` | 500 o 400 | 400 | no |
| Una ruta desconocida, incluidas todas las rutas `/actuator` | 404 | 404 | no |
| Una ruta conocida con otro método (`GET /pdf`) | 405 | 405 | no |
| No llega ningún byte de la petición durante dos minutos | 500 | 408 | no |
| El documento supera el límite de tamaño (10 MiB por defecto) | 500, tras la subida | 413, antes del resto de la subida | no |
| Una bomba zip, o un documento que necesita más memoria de la que puede ocupar una instancia | sin límite: el contenedor se queda antes sin memoria o sin disco | 413 | no |
| Texto, un archivo vacío, una hoja de cálculo, una imagen, bytes aleatorios | 200, un PDF de lo que LibreOffice hiciera con ello | 415 | no |
| Un documento que LibreOffice no puede abrir | 500 | 422 | no |
| Todas las instancias ocupadas más allá del límite de espera, la cola llena, arrancando o deteniéndose | esperaba sin límite | 503 con `Retry-After` | sí, tras `Retry-After` |
| Una conversión que dura más que el tiempo límite de conversión (3 minutos por defecto) | esperaba sin límite | 504 | una vez, como mucho |
| Cualquier otra cosa | 500 | 500 | no |

Qué cambiar:

- **Cuerpos de error.** El documento `{"timestamp","status","error","path"}` de Spring y la «Whitelabel Error Page»
  en HTML han desaparecido. Un cliente que los leía pasa a leer `status`, o solo el estado HTTP. No compare con
  `detail`: es una frase fija por situación, pensada para una persona
  ([todos los errores](api.md#todos-los-errores-que-responde-el-servicio)).
- **Reintentos.** El original nunca rechazaba trabajo, así que el único fallo de un cliente era su propio tiempo de
  espera de lectura. Este servicio responde `503` cuando está saturado. Un cliente que solo reintenta los errores de
  conexión debe reintentar también un `503` tras el `Retry-After` que lleva; reintentar un `504` una vez es razonable;
  un `4xx` no cambiará al reintentar.
- **Tiempo de espera de lectura.** Fíjelo por encima del límite de espera más el tiempo límite de conversión,
  60 + 180 segundos por defecto, con margen, para que el cliente reciba el `503` o el `504` del servicio en lugar de
  cortar él mismo la conexión.
- **Entradas que no son documentos.** El texto plano, los archivos vacíos y otros formatos reciben ahora `415`.
  Conviértalos antes a DOCX.
- **Imágenes modificadas.** Un `200` puede llevar `Docx-To-Pdf-Pictures: downscaled=<n>, replaced=<n>` cuando una
  imagen demasiado grande para convertirse de forma segura se redujo o se sustituyó ([Imágenes modificadas antes de
  la conversión](api.md#imágenes-modificadas-antes-de-la-conversión)). Un cliente que quiera avisar a su usuario la
  lee; cualquier otro puede ignorarla.

## Lo que cambia un despliegue

### Comprobaciones de salud y métricas

| Propósito | moalhaddar/docx-to-pdf | Este servicio |
| --- | --- | --- |
| Liveness | `GET /actuator/health/liveness` | `GET /livez` |
| Readiness, y la espera de un arranque | `GET /actuator/health/readiness`, `GET /actuator/health` | `GET /readyz` |
| Métricas de Prometheus | — | `GET /metrics` |
| Trazas, logs y métricas por OTLP | — | defina `OTEL_EXPORTER_OTLP_ENDPOINT` |

Todas las rutas `/actuator` responden `404`, así que sustituya cada referencia a ellas: sondas de Kubernetes,
estrategias de espera de Testcontainers (`.forPath("/actuator/health")` pasa a ser `.forPath("/readyz")`),
comprobaciones de monitorización, scripts. [Comprobaciones de salud](operations.md#comprobaciones-de-salud) dice qué
responde cada ruta y cuánto debe esperar una comprobación de arranque; [Métricas](operations.md#métricas) enumera
todas las métricas, y [Telemetría](configuration.md#logs-y-telemetría) los ajustes de OpenTelemetry.

### Ajustes

Cada ajuste es una variable de entorno; [Configuración](configuration.md) los enumera todos con sus valores por
defecto. Lo que cambia para un despliegue de moalhaddar/docx-to-pdf es solo lo que este fijaba:

| Ajuste de moalhaddar/docx-to-pdf | En este servicio |
| --- | --- |
| `SERVER_PORT` | Se sigue leyendo. `DOCX_TO_PDF_PORT` tiene prioridad cuando ambas están definidas. |
| `POOL_SIZE`, `pool.size` | Se siguen leyendo, en ese orden. `DOCX_TO_PDF_POOL_SIZE` tiene prioridad sobre las dos. |
| `SPRING_SERVLET_MULTIPART_MAX_FILE_SIZE`, `SPRING_SERVLET_MULTIPART_MAX_REQUEST_SIZE` | No se leen: defina `DOCX_TO_PDF_MAX_DOCUMENT_SIZE`. Un valor como `1000MB` conserva su significado. Sin cambios, el límite es de 10 MiB y un documento mayor recibe `413`. |
| `SERVER_TOMCAT_CONNECTION_TIMEOUT` | No se lee: `DOCX_TO_PDF_IDLE_TIMEOUT` es lo más parecido, una duración en lugar de milisegundos; `120000` es su valor por defecto, `120s`. |
| `SERVER_TOMCAT_MAX_HTTP_FORM_POST_SIZE` | No se lee, y nada lo sustituye: ninguna ruta acepta un formulario codificado como URL. |
| `JAVA_TOOL_OPTIONS` | No se lee: no hay JVM. Un `-Dpool.size` dentro de él tampoco se lee. La memoria se dimensiona a partir de `DOCX_TO_PDF_INSTANCE_HARD_MEMORY` ([Dimensionamiento](operations.md#dimensionamiento)). |
| `LOGGING_LEVEL_*` | No se lee: `RUST_LOG`. |
| otros `SPRING_*`, `SERVER_TOMCAT_*`, `MANAGEMENT_*` | No se leen. Elimínelos. |

Dos ajustes son nuevos y merecen un vistazo en cualquier despliegue: `DOCX_TO_PDF_CONVERSION_TIMEOUT` y
`DOCX_TO_PDF_POOL_WAIT_TIMEOUT`, que acotan lo que el original esperaba sin límite.

### Memoria, disco y CPU

Sin JVM, la memoria del contenedor la determinan las instancias de LibreOffice. Cada una se mata y se sustituye, y su
petición se responde con `413`, si supera `DOCX_TO_PDF_INSTANCE_HARD_MEMORY` (2 GiB por defecto), de modo que un
límite de memoria de 256 MiB + tamaño del pool × (ese límite + 256 MiB) no puede superarse, sean cuales sean los
documentos. Un despliegue que aumentó el límite de documento para documentos grandes con imágenes aumenta con él el
límite estricto de memoria. `/tmp` debe admitir escritura, en disco por defecto.
[Dimensionamiento](operations.md#dimensionamiento) da las fórmulas, y una sonda que mide su propia máquina.

### La imagen

- Compilada para `linux/amd64` y `linux/arm64`: en una máquina Arm, Apple Silicon incluido, se ejecuta de forma
  nativa y no bajo emulación.
- Se ejecuta como usuario `10001` sin entrada en passwd, cualquier otro usuario que no sea root sirve, el sistema de
  archivos raíz puede ser de solo lectura, y `/tmp` es el único directorio en el que escribe
  ([El contenedor](operations.md#el-contenedor)).
- El PID 1 es `tini`; `SIGTERM` inicia una parada ordenada. Dé al runtime el periodo de gracia de parada más unos
  40 segundos antes de que mate el proceso ([Parada](operations.md#parada)).
- No lleva ningún cliente HTTP, así que no tiene `HEALTHCHECK` de Docker: use las comprobaciones HTTP del orquestador.
- Dos fuentes que llevaba la imagen original, Brown Std y GE Dinar Two, tienen licencias comerciales y no están en
  esta imagen: los documentos compuestos con ellas reciben un sustituto. Monte los archivos de fuentes para los que
  tenga licencia ([Montar las suyas](operations.md#montar-las-suyas)).

### La red

El servicio no abre ninguna conexión propia salvo a un colector de OpenTelemetry, si hay uno definido. La
NetworkPolicy del chart de Helm está activada por defecto y limita el pod a eso: tráfico de entrada solo en el puerto
8080, tráfico de salida solo al DNS (UDP y TCP 53, hacia los pods `kube-dns` del clúster) y al puerto de
`otel.endpoint`. Un clúster con una caché DNS local del nodo fija `networkPolicy.dnsTo` en su dirección, un colector
al que solo se llega en direcciones concretas fija `networkPolicy.otlpTo`, y `networkPolicy.enabled: false` deja la
red del pod a una política propia ([Red](installation.md#red-el-servicio-no-descarga-nada)).

### Una lista de comprobación

1. Sustituya la referencia a la imagen por la imagen de este servicio.
2. Sustituya cada `/actuator/health*` por `/livez` para liveness y por `/readyz` para readiness y para las esperas de
   arranque.
3. Traduzca los ajustes con la tabla de arriba, y elimine los que no se leen.
4. Aumente el tiempo de espera de lectura del cliente por encima del límite de espera más el tiempo límite de
   conversión, y reintente un `503` tras `Retry-After`.
5. Busque código que analice el cuerpo de error del original, dependa de un `500` o envíe algo que no sea un
   documento de procesador de textos.
6. Apunte `OTEL_EXPORTER_OTLP_ENDPOINT` a un colector, o recoja `/metrics`.
7. Ejecute las pruebas del cliente contra la nueva imagen.

## Todas las diferencias deliberadas

Cada una de las diferencias de abajo es intencionada: un error del original que no se ha trasladado, un límite que le
faltaba o un detalle de Spring que no forma parte de la interfaz. El propio PDF solo difiere en la medida en que lo
maqueta un LibreOffice más reciente.

| Diferencia | moalhaddar/docx-to-pdf | Este servicio | Por qué |
| --- | --- | --- | --- |
| Un LibreOffice más reciente | LibreOffice 7.3: números de página y de lista vacíos en algunos campos, una tabla flotante larga que cuelga la conversión, una línea menos por página con el interlineado de 1.15 de Word | Un LibreOffice actual: esos campos rellenos, esa tabla convertida en menos de un segundo, una línea más por página; las palabras son las mismas | LibreOffice 7.3 ya no recibe mantenimiento, y el más reciente representa mejor estos documentos. |
| Las imágenes en el PDF | cada imagen decodificada y reducida a 300 DPI como máximo | incrustadas tal como están, un JPEG byte a byte; `DOCX_TO_PDF_PDF_MAX_IMAGE_DPI=300` las reduce como hacía el original | Un informe típico se convierte con un tercio de la CPU, y las imágenes no pierden nada; una imagen colocada por encima de 300 DPI da un PDF más grande. |
| Contenido fuera del documento | una imagen enlazada a una URL o a un archivo, una plantilla remota o un vínculo OLE pueden descargarse | nunca se descargan ni se leen: una imagen enlazada se convierte a partir de su copia incrustada, o a nada | Un documento no debe hacer que el servicio alcance otros hosts de su red, ni que espere a un host que nunca responde. |
| Texto en chino, japonés y coreano | ninguna fuente para él: cuadros vacíos o una fuente de reserva incorrecta | dibujado con Noto CJK | El texto en esas escrituras debe poder leerse en el PDF. |
| Una petición que no es una subida de formulario | 500, o 400 con el error de Spring | 400 | La petición es un error del cliente, que es lo que dice un 4xx. |
| Un documento por encima del límite de tamaño | 500 tras toda la subida | 413, antes de leer el resto del cuerpo | Una subida demasiado grande es un error del cliente, y leerla hasta el final le cuesta al servicio sin ningún beneficio. |
| Un archivo que no es un documento de procesador de textos | 200, un PDF de lo que LibreOffice hiciera con él, un RTF detrás de una marca de orden de bytes compuesto como su propio marcado | 415 | Un PDF de basura parece un éxito; solo se aceptan los formatos que el original convierte de verdad. |
| Un documento dañado | 500 | 422 | La culpa es del documento, no del servicio. |
| Un documento que necesita demasiada memoria | sin límite: el contenedor se queda sin memoria y se pierden todas las conversiones que hay en él | 413 de inmediato; la instancia se sustituye y las demás conversiones siguen | Un solo documento no debe tumbar todo el servicio. |
| Una bomba zip | sin comprobación: LibreOffice la descomprime en disco mientras la carga | 413 antes de cualquier conversión | Unos pocos megabytes no deben llenar gigabytes de disco y de memoria. |
| Una imagen demasiado grande para decodificarla | 200; por encima de aproximadamente 1 GiB decodificada, la imagen falta sin aviso | 200 con la imagen reducida, o sustituida por un marcador de posición, y una cabecera `Docx-To-Pdf-Pictures` que lo indica | Perder contenido sin avisar es un error, y rechazar el documento dejaría al cliente sin nada. |
| Una conversión que no termina | ninguna respuesta, la instancia retenida mientras dure | 504 tras el tiempo límite de conversión, la instancia sustituida | Un documento atascado no debe retener para siempre una instancia, ni al cliente. |
| Más peticiones que instancias | esperan sin límite | una cola acotada; más allá de ella, o del límite de espera, 503 con `Retry-After` | Un cliente debe saber que el servicio está saturado, y otra réplica puede atender la petición. |
| Una subida lenta | 500 cuando la subida se detiene unos milisegundos, como ocurre en las redes reales | se espera, tarde lo que tarde la subida completa | Una pausa en la red no es un error del cliente. |
| Un cliente que deja de enviar | 500 tras el tiempo límite de conexión | 408 tras dos minutos sin un byte, y la conexión cerrada | El cliente se detuvo, que es lo que dice un 408. |
| Un cliente que se desconecta durante una conversión | la conversión sigue y retiene la instancia | la conversión se detiene y la instancia se libera de inmediato | Nadie está esperando ese PDF. |
| Cuerpos de error | el documento JSON de Spring, o una página HTML para un navegador | `application/problem+json`, cuyo `detail` nunca nombra el convertidor | El formato estándar para los errores HTTP, sin los detalles internos del framework. |
| Comprobaciones de salud y métricas | el Spring Actuator bajo `/actuator`; la salud indica que está activo mientras la única instancia está atascada en una conversión | `/livez`, `/readyz` y `/metrics`; `/readyz` es `503` mientras ninguna instancia pueda aceptar una conversión | La readiness debe decir si una petición puede atenderse ahora, para que el tráfico vaya a una réplica que pueda. |
| La sonda de plataforma | ninguna | `POST /probe` en un puerto propio, desactivada por defecto, mide la máquina para dimensionar | Dimensionar un despliegue requiere números de la máquina en la que se ejecuta. |
| Peticiones de origen cruzado fuera de una conversión | 403 `Invalid CORS request` para una petición preliminar a otra ruta o para otro método, y las cabeceras `Allow` de Spring | ningún tratamiento de origen cruzado fuera de las dos rutas de conversión; una petición preliminar nombra solo `POST` | El navegador aplica por sí mismo las reglas de origen cruzado; el 403 y las cabeceras eran cosa de Spring. |
| Una instancia que no arranca | el servicio no arranca | el arranque se vuelve a intentar, hasta tres veces, con `/readyz` en `503` mientras tanto | Un fallo transitorio no debe meter al servicio en un bucle de reinicios. |
| Un pool mayor que las CPU | el arranque puede colgarse para siempre cuando el pool es mayor que las CPU que ve la JVM | todas las instancias arrancan, sea cual sea el tamaño del pool | Un arranque que nunca termina es un error. |
| Ajustes de Spring, Tomcat y la JVM | se leen | no se leen; sus sustitutos están en [Ajustes](#ajustes) | En este servicio no hay Spring, ni Tomcat, ni JVM. |
