# Comparación con Gotenberg y moalhaddar/docx-to-pdf

<!-- languages:begin -->
[English](../../../guide/comparison.md) · [Русский](../../ru/guide/comparison.md) ·
[中文](../../zh/guide/comparison.md) · **Español**
<!-- languages:end -->
<!-- translation of docs/guide/comparison.md: f230995a7fb2 -->

Otros dos servicios convierten documentos de oficina a PDF por HTTP con LibreOffice. **Gotenberg** es el más usado:
una API de documentos de uso general en torno a Chromium, LibreOffice y herramientas de PDF.
**moalhaddar/docx-to-pdf** es el servicio cuya interfaz conserva este. Esta página dice qué hace cada uno y después
compara la velocidad, la memoria y lo que hace cada uno con un documento hostil.

Cada cifra de esta página se lee de los resultados en bruto de tres ejecuciones completas de comparación de
`make compare`, la comparación que cualquiera puede repetir ([Cómo repetirlo](#cómo-repetirlo)); la página
[Benchmarks](https://oxidestack.github.io/docx-to-pdf/benchmarks/) muestra cada cifra de la última ejecución y los
resultados en bruto de cada una. La compilación del sitio escribe las cifras de abajo a partir de esos archivos y
falla ante una cifra escrita aquí a mano. Dentro de una ejecución, una cifra es la media ± σ (n) sobre los bloques de
la ejecución, siendo σ la desviación típica muestral, y dos cifras difieren cuando su diferencia es mayor que
2σ = 2·√(σ₁²/n₁ + σ₂²/n₂); un rango abarca las ejecuciones, y la cifra de cada ejecución es uno de sus extremos o está
entre ellos; donde una cifra procede de una sola ejecución, la página lo dice.

## Cuál elegir

- **Gotenberg**, cuando el trabajo es más que documentos de Word a PDF: HTML, URL o Markdown a través de Chromium,
  hojas de cálculo y presentaciones, unir y dividir PDF, PDF/A o PDF/UA, webhooks u opciones en la petición
  (intervalos de páginas, una marca de agua, una contraseña). Hace todo eso, y tiene años en producción a sus
  espaldas.
- **oxidestack/docx-to-pdf**, cuando el trabajo es convertir documentos de Word a PDF con una sola llamada HTTP, en
  volumen, a partir de documentos que usted no controla. Con la carga de trabajo medida, en cada una de tres
  ejecuciones intercaladas, usó menos CPU y menos memoria que Gotenberg y convirtió más documentos por segundo
  ([las proporciones](#velocidad-y-memoria)), y un documento hostil se detiene en un límite en lugar de dejarse en
  ejecución ([los casos](#documentos-hostiles)).
- **moalhaddar/docx-to-pdf**, solo para conservar exactamente lo que hace hoy, incluida la maquetación de
  LibreOffice 7.3. Para dejarlo, la [guía de migración](migration.md) enumera lo que cambian un cliente y un
  despliegue.

## Qué hace cada uno

La columna de Gotenberg está contrastada con su documentación y su código fuente en la versión medida, que nombra
[Velocidad y memoria](#velocidad-y-memoria).

| | Gotenberg | moalhaddar/docx-to-pdf | oxidestack/docx-to-pdf |
| --- | --- | --- | --- |
| Qué convierte | todos los formatos que abre LibreOffice (documentos de texto, hojas de cálculo, presentaciones, dibujos), y HTML, URL y Markdown a través de Chromium | documentos de Word (DOCX, DOC, ODT, RTF y sus variantes); cualquier otra cosa se pasa a LibreOffice sin comprobar | solo documentos de Word: DOCX, DOCM, DOTX, DOC, ODT, OTT, ODT plano, RTF ([los formatos aceptados](api.md#los-formatos-aceptados)) |
| Rutas de conversión | `/forms/libreoffice/convert`, `/forms/chromium/convert/url`, `/forms/chromium/convert/html`, `/forms/chromium/convert/markdown`, y capturas de pantalla de cada entrada de Chromium | `POST /pdf`, `POST /docx-to-pdf` | `POST /pdf`, `POST /docx-to-pdf` |
| Cómo se decide el formato | la extensión del nombre del archivo | no se comprueba | por los bytes del documento; cualquier otra cosa es `415` antes de que LibreOffice la vea |
| Opciones en una petición | muchas en la ruta de LibreOffice: intervalos de páginas, apaisado, una contraseña, PDF/A y PDF/UA, unir varios archivos en uno, una marca de agua, PDF etiquetado | ninguna | ninguna |
| Herramientas de PDF | unir, dividir, PDF/A y PDF/UA, metadatos, marcadores, adjuntos, Factur-X, aplanar, optimizar, marca de agua, sello, rotar, cifrar (`/forms/pdfengines/…`) | ninguna | ninguna |
| Trabajo asíncrono | webhooks; entrada descargada de una URL (`downloadFrom`) | ninguno | ninguno |
| Autenticación, TLS | autenticación básica u OIDC; TLS | no documentado | ninguno: póngalo detrás de su ingress |
| Una conversión que dura demasiado | la API responde `503` al llegar a su tiempo límite (`API_TIMEOUT`, `30s` por defecto); LibreOffice sigue con el documento | sin tiempo límite | se mata al llegar a `DOCX_TO_PDF_CONVERSION_TIMEOUT` (`180s` por defecto) y se responde `504`; la instancia se sustituye |
| Un documento que necesita demasiada memoria | sin límite propio | sin límite propio | la instancia se mata al superar `DOCX_TO_PDF_INSTANCE_HARD_MEMORY` (`2GiB` por defecto) y la petición se responde con `413`; un ZIP que se expande más allá de `DOCX_TO_PDF_MAX_INFLATED_SIZE` (`256MiB`) es `413` antes de la conversión |
| Una imagen demasiado grande para LibreOffice | descartada, respuesta `200` | descartada, respuesta `200` | reducida, y la cabecera `Docx-To-Pdf-Pictures` lo indica |
| Una imagen enlazada en un documento | no se descarga | se descarga, desde direcciones públicas y privadas | no se descarga |
| LibreOffice | 26.8.1, reiniciado tras `LIBREOFFICE_RESTART_AFTER` conversiones (`10` por defecto) | 7.3 | 26.8, una instancia sustituida tras `DOCX_TO_PDF_INSTANCE_MAX_CONVERSIONS` conversiones (`200` por defecto) o al superar su techo de memoria |
| Telemetría | trazas, métricas y logs de OpenTelemetry; Prometheus | ninguna | trazas, métricas y logs de OpenTelemetry; Prometheus `GET /metrics` |
| Imágenes | amd64, arm64 y tres arquitecturas más | solo amd64 | amd64 y arm64 |

Los límites de la columna de oxidestack/docx-to-pdf son sus valores por defecto; cada uno es un ajuste
([Configuración](configuration.md)). La fila de la imagen enlazada y las filas de los límites se miden en la prueba de
tortura de abajo; las de Gotenberg y moalhaddar/docx-to-pdf son como cada uno se distribuye.

## Velocidad y memoria

Las cifras medidas de velocidad y memoria están en la página
[Benchmarks](https://oxidestack.github.io/docx-to-pdf/benchmarks/) y en el
[original en inglés](https://github.com/oxidestack/docx-to-pdf/blob/main/docs/guide/comparison.md#speed-and-memory).

## Documentos hostiles

Las cifras medidas de cada documento hostil están en la página
[Benchmarks](https://oxidestack.github.io/docx-to-pdf/benchmarks/) y en el
[original en inglés](https://github.com/oxidestack/docx-to-pdf/blob/main/docs/guide/comparison.md#hostile-documents).

## Lo que las cifras no pueden decir

- **Una vCPU, un LibreOffice, una mezcla de documentos.** Gotenberg ejecuta un LibreOffice por contenedor, así que
  esta es la celda que compara directamente; no se midió un pool mayor frente a varios contenedores de Gotenberg.
- **Un solo tipo de host.** Un tiempo absoluto o una cifra de CPU varía entre máquinas virtuales de una misma
  definición, y una proporción medida dentro de una ejecución intercalada varía menos, pero aún más que su propio
  error estándar: de ahí el rango sobre las ejecuciones. Todas las ejecuciones fueron en amd64: moalhaddar/docx-to-pdf
  no tiene imagen arm64, y no hay ninguna ejecución en arm64 en los resultados.
- **Una imagen por ejecución.** La última ejecución midió la única imagen de oxidestack/docx-to-pdf que nombra la
  sección de arriba, las ejecuciones anteriores el código previo a ella; una versión posterior la mide una ejecución
  posterior.
- **Una ejecución por servicio para cada documento hostil.** Las respuestas se repiten, porque las fijan los límites;
  un tiempo o un pico es una sola medición.
- **La memoria a lo largo de miles de conversiones.** Las pruebas de resistencia de
  [Benchmarks](https://oxidestack.github.io/docx-to-pdf/benchmarks/#soaks) son una por servicio, lo que no basta para
  decidir si la memoria de un servicio crece: esta página indica el nivel que midió la prueba de resistencia de
  oxidestack/docx-to-pdf y no afirma nada sobre el crecimiento.

Donde Gotenberg lo hace igual de bien o mejor con un documento hostil: tampoco abre ninguna conexión de red, convierte
`tdf38575` y abandona antes una subida detenida. Donde difiere: su respuesta al llegar a su tiempo límite no detiene
LibreOffice, que sigue con el documento mientras se rechazan las peticiones que esperan detrás.

## Renderizado

El renderizado de Gotenberg nunca se ha medido: ningún PDF de Gotenberg se ha comparado con los de
oxidestack/docx-to-pdf, así que esta página no afirma nada al respecto. Los dos ejecutan LibreOffice 26.8, así que
cabría esperar una diferencia en las fuentes y en las opciones de exportación, no en la maquetación, y esa expectativa
no está medida. Frente a moalhaddar/docx-to-pdf, los documentos que difieren lo hacen donde LibreOffice 7.3 y 26.8
maquetan una página de forma distinta; la [guía de migración](migration.md) dice qué debe comprobar un despliegue
que se cambia.

## Cómo repetirlo

`make compare` arranca cada servicio desde cero, ejecuta las celdas de velocidad en bloques (cada bloque ejecuta cada
servicio una vez, en un orden aleatorio, de modo que una deriva del host afecta a todos los servicios por igual),
después los documentos hostiles y las pruebas de resistencia, e imprime las dos tablas. Solo hace falta Docker.

```sh
make compare QUICK=1   # minutes on a laptop: one short block and four hostile documents
make compare           # the full run, for a quiet VM: hours
```

El modo rápido muestra que el comando funciona; sus números no son comparables con los de arriba. El modo completo
escribe un `results.json` como el que se lee para esta página, comparable en el mismo tipo de host, y
moalhaddar/docx-to-pdf solo se ejecuta en amd64. Lo que ejecuta y lo que no muestra está en
[examples/compare](../../../../examples/compare/README.md).
