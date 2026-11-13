# Instalación

<!-- languages:begin -->
[English](../../../guide/installation.md) · [Русский](../../ru/guide/installation.md) ·
[中文](../../zh/guide/installation.md) · **Español**
<!-- languages:end -->
<!-- translation of docs/guide/installation.md: 3e7d80a570e6 -->

Cómo ejecutar el servicio: con `docker run` para probarlo, con Docker Compose en una sola máquina y en Kubernetes con
el chart de Helm. Cada forma termina con el servicio respondiendo a `POST /pdf` en el puerto `8080`; la
[Referencia de la API](api.md) dice qué enviarle. Una prueba mantiene esta página fiel a los archivos que muestra:
los archivos de Compose de abajo son los de [`examples/compose/`](../../../../examples/compose/), una prueba arranca
cada uno de ellos y convierte un documento a través de él, y los comandos `helm` instalan el chart como lo hace la
propia prueba del chart.

> **Mantenga el servicio fuera de la Internet pública.** No tiene autenticación ni límite de peticiones: cualquiera
> que alcance su puerto puede hacerle convertir lo que envíe, y cada conversión cuesta un núcleo de CPU y hasta unos
> pocos GiB de memoria mientras dura. Ejecútelo en una red privada, donde solo lo llamen sus propias aplicaciones, y
> ponga lo que esté de cara a los usuarios delante de esas aplicaciones, no delante del servicio.

## Docker

```sh
docker run --rm -p 8080:8080 quay.io/oxidestack/docx-to-pdf:1.3.1
```

La imagen y el chart se publican en quay.io. `ghcr.io/oxidestack/docx-to-pdf` y
`oci://ghcr.io/oxidestack/charts/docx-to-pdf` son sus réplicas, con los mismos digests y las mismas firmas, para una
red que no puede alcanzar quay.io.

`-p 8080:8080` publica el puerto en todas las interfaces de red de la máquina, así que otras máquinas de su red también
pueden llamarlo. Para limitarlo a la propia máquina, vincule el puerto a `127.0.0.1`:

```sh
docker run --rm -p 127.0.0.1:8080:8080 quay.io/oxidestack/docx-to-pdf:1.3.1
```

Una vez que haya arrancado (`/readyz` responde `200`, normalmente en pocos segundos), convierta un documento:

```sh
curl -F document=@report.docx http://localhost:8080/pdf -o report.pdf
```

La imagen está compilada para `linux/amd64` y `linux/arm64`, así que se ejecuta de forma nativa tanto en un Mac con
Apple Silicon como en un servidor x86. Está hecha para ejecutarse blindada —un usuario que no es root, un sistema de
archivos raíz de solo lectura, ninguna capability de Linux— y [Operaciones](operations.md#el-contenedor) da la línea
de `docker run` que lo hace.

## Docker Compose

`examples/compose/compose.yaml` ejecuta el servicio blindado, en `127.0.0.1:8080`, con el límite de memoria que
necesita una conversión a la vez:

```yaml
# docx-to-pdf on this machine only: http://127.0.0.1:8080 (docs/guide/installation.md).
services:
  docx-to-pdf:
    image: quay.io/oxidestack/docx-to-pdf:1.3.1
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

Los ajustes son variables de entorno, enumeradas en [Configuración](configuration.md). Para convertir más documentos a
la vez, aumente `DOCX_TO_PDF_POOL_SIZE` y, con él, el límite de memoria ([Recursos](#recursos)).
`docker compose down -v` detiene el servicio y elimina su volumen `/tmp`.

### Con telemetría

El servicio envía trazas, logs y métricas por OTLP al colector que indica `OTEL_EXPORTER_OTLP_ENDPOINT`. Aquí un
OpenTelemetry Collector los recibe e imprime una línea por cada lote, `examples/compose/telemetry/compose.yaml`:

```yaml
# docx-to-pdf sending its traces, logs and metrics to an OpenTelemetry Collector (docs/guide/installation.md).
services:
  docx-to-pdf:
    image: quay.io/oxidestack/docx-to-pdf:1.3.1
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

y su configuración, `examples/compose/telemetry/otel-collector.yaml`:

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

Sustituya el exportador `debug` por el de su backend, o apunte `OTEL_EXPORTER_OTLP_ENDPOINT` a un colector que ya
ejecute. Sin ningún colector, `/metrics` sigue sirviendo las métricas para que Prometheus las recoja. Lo que informa el
servicio está en [Operaciones](operations.md#telemetría).

## Kubernetes

### Con Helm

El chart se publica en `oci://quay.io/oxidestack/charts`. Con sus valores por defecto ejecuta un pod que convierte un
documento a la vez:

```sh
helm install docx-to-pdf oci://quay.io/oxidestack/charts/docx-to-pdf --version 1.3.1 \
  --namespace docx-to-pdf --create-namespace
```

Los valores que un despliegue suele cambiar, en un archivo propio:

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
helm upgrade --install docx-to-pdf oci://quay.io/oxidestack/charts/docx-to-pdf --version 1.3.1 \
  --namespace docx-to-pdf --create-namespace --values values.yaml
```

| Valor | Por defecto | Qué establece |
| --- | --- | --- |
| `replicaCount` | `1` | Los pods. |
| `poolSize` | `1` | `DOCX_TO_PDF_POOL_SIZE`, las conversiones en paralelo por pod. La petición de CPU es el mismo número, y la petición y el límite de memoria se calculan a partir de él ([Recursos](#recursos)). |
| `instanceHardMemoryMiB` | `2048` | `DOCX_TO_PDF_INSTANCE_HARD_MEMORY`: la memoria que puede ocupar una conversión antes de detenerse con `413`. El límite de memoria se calcula a partir de él. |
| `otel.endpoint` | vacío: sin exportación | `OTEL_EXPORTER_OTLP_ENDPOINT`, el colector. |
| `config` | `{}` | Cualquier otro ajuste de [Configuración](configuration.md), como `NAME: value`. |
| `tmp.medium` | en disco | `Memory` pone `/tmp` en un tmpfs, cuyo tamaño `tmp.sizeMiB` se suma al límite de memoria. |
| `fonts` | ninguno | Un volumen de fuentes para las que tenga licencia ([Operaciones](operations.md#montar-las-suyas)). |
| `serviceMonitor.enabled` | `false` | Un ServiceMonitor de prometheus-operator para `/metrics`. |
| `networkPolicy.enabled` | `true` | Una NetworkPolicy: tráfico de entrada al puerto 8080 y de salida solo al DNS y al colector ([Red](#red-el-servicio-no-descarga-nada)). |

Todos los valores están enumerados en el README del chart, y el chart rechaza un valor que no conoce. El Service de la
release es `docx-to-pdf.docx-to-pdf.svc.cluster.local:8080`, de tipo `ClusterIP`: accesible solo desde dentro del
clúster. `helm test docx-to-pdf --namespace docx-to-pdf` convierte un documento pequeño a través de él.

### Sin Helm

Para desplegar con manifiestos simples —`kubectl apply`, Kustomize o una herramienta de GitOps que aplica archivos—,
renderice el chart una vez con los mismos valores y conserve el resultado:

```sh
helm template docx-to-pdf oci://quay.io/oxidestack/charts/docx-to-pdf --version 1.3.1 \
  --namespace docx-to-pdf --values values.yaml --skip-tests > docx-to-pdf.yaml
kubectl create namespace docx-to-pdf
kubectl apply --namespace docx-to-pdf -f docx-to-pdf.yaml
```

El resultado es un Deployment, un Service, un PodDisruptionBudget y una NetworkPolicy.
[Operaciones](operations.md#kubernetes) tiene el mismo Deployment escrito a mano, con el motivo de cada número que
contiene.

### Red: el servicio no descarga nada

Un documento puede nombrar contenido que está fuera de él: una imagen enlazada a una URL o a un archivo, una plantilla
remota, un vínculo OLE. El servicio nunca descarga ni lee nada de eso. Una imagen enlazada se convierte a partir de la
copia incrustada en el documento, o a nada, de modo que una conversión no abre ninguna conexión ni espera a ningún
host, diga lo que diga el documento. El pod no necesita tráfico de salida salvo al DNS y al colector de telemetría, y
la NetworkPolicy del chart, activada por defecto, lo limita a eso, para que nada que se ejecute en el convertidor
pueda alcanzar el resto del clúster. Selecciona los pods de la release y permite:

- tráfico de entrada solo en el puerto del servicio, 8080, desde cualquier origen o desde los pares que indique
  `networkPolicy.ingressFrom`;
- tráfico de salida al DNS, UDP y TCP 53, hacia los pods DNS del clúster (`k8s-app: kube-dns` en `kube-system`) o
  los pares que indique `networkPolicy.dnsTo`;
- tráfico de salida al puerto de `otel.endpoint` (`4318` arriba), hacia los pares que indique
  `networkPolicy.otlpTo`, o a cualquier destino cuando no indica ninguno; sin endpoint, ninguna regla.

Los valores de arriba limitan la regla del colector al namespace del colector, `observability`, con
`networkPolicy.otlpTo`.

| Valor | Por defecto | Qué establece |
| --- | --- | --- |
| `networkPolicy.enabled` | `true` | La NetworkPolicy. `false` deja la red del pod a una política propia. |
| `networkPolicy.ingressFrom` | `[]`: cualquiera | Quién puede conectarse al puerto 8080, como pares de NetworkPolicy. |
| `networkPolicy.dnsTo` | los pods de `kube-dns` | Dónde está el DNS. Una caché DNS local del nodo necesita aquí su dirección, p. ej. `[{ipBlock: {cidr: 169.254.20.10/32}}]`. |
| `networkPolicy.otlpTo` | `[]`: cualquier dirección en el puerto del endpoint | Dónde está el colector. |

La política solo tiene efecto donde el plugin de red del clúster aplica NetworkPolicy (lo hacen Calico, Cilium, el
kindnet de kind y la mayoría de los clústeres gestionados); en los demás se acepta y no hace nada.

## Recursos

Cada conversión mantiene ocupado un núcleo de CPU y la memoria de LibreOffice crece con el documento, así que un pod se
dimensiona a partir del tamaño de su pool, el número de conversiones que ejecuta a la vez:

| Recurso | Por pod | Con los valores por defecto (tamaño del pool 1) | Con los valores de arriba (tamaño del pool 2) |
| --- | --- | --- | --- |
| CPU | el tamaño del pool | 1 núcleo | 2 núcleos |
| Memoria, petición = límite | 256 MiB + tamaño del pool × (`DOCX_TO_PDF_INSTANCE_HARD_MEMORY` + 256 MiB) | 2560 MiB | 4864 MiB |
| `/tmp` | tamaño del pool × el mayor de 3 × `DOCX_TO_PDF_MAX_DOCUMENT_SIZE` y 256 MiB | 256 MiB | 512 MiB |

Con ese límite de memoria ningún documento puede hacer que el contenedor muera por falta de memoria: uno que ocuparía
más se detiene y se responde con `413`. El chart de Helm calcula la petición de CPU y la memoria a partir de
`poolSize`; los archivos de Compose fijan `mem_limit` con la misma fórmula. Un informe de empresa típico se convierte
en alrededor de un segundo en un núcleo; para medir lo que tardan sus documentos en sus máquinas, y cuántos pods
necesita una carga, consulte [Operaciones](operations.md#dimensionamiento).

## Un repositorio de Helm clásico

Para una herramienta que instala charts desde la URL de un repositorio en lugar de desde un registro OCI, cada release
de GitHub lleva el índice de los charts publicados hasta entonces, y cada versión se descarga de su propia release; la
URL de abajo sirve siempre el de la última release:

```sh
helm repo add docx-to-pdf https://github.com/oxidestack/docx-to-pdf/releases/latest/download
helm install docx-to-pdf docx-to-pdf/docx-to-pdf --version 1.3.1 --namespace docx-to-pdf --create-namespace
```

El chart figura en [Artifact Hub](https://artifacthub.io/) a partir de su repositorio de quay.io. Una versión
preliminar (`0.2.0-rc.1`) también está en el índice; `helm search repo --devel` la muestra, y nada la instala a menos
que se pida su versión.

## Verificar una versión

Cada versión la compila y la publica el workflow de release del repositorio a partir de su etiqueta, y ese workflow
firma lo que publica. La imagen y el chart llevan una firma de [cosign](https://docs.sigstore.dev/cosign/) sin
ninguna clave que distribuir: el certificado nombra el workflow y la etiqueta que lo produjeron, y la firma queda
registrada en el registro público de transparencia de Sigstore. Para comprobar que una imagen es la que produjo esa
versión:

```sh
cosign verify quay.io/oxidestack/docx-to-pdf:1.3.1 \
  --certificate-identity https://github.com/oxidestack/docx-to-pdf/.github/workflows/release.yml@refs/tags/v1.3.1 \
  --certificate-oidc-issuer https://token.actions.githubusercontent.com
cosign verify quay.io/oxidestack/charts/docx-to-pdf:1.3.1 \
  --certificate-identity https://github.com/oxidestack/docx-to-pdf/.github/workflows/release.yml@refs/tags/v1.3.1 \
  --certificate-oidc-issuer https://token.actions.githubusercontent.com
```

La imagen lleva además atestaciones: su [procedencia de compilación SLSA](https://slsa.dev/spec/v1.0/provenance) (qué
commit, qué ejecución del workflow) en la imagen multiarquitectura, y un SBOM SPDX, los paquetes de Debian y los
crates que contiene, en la imagen de cada arquitectura. La [CLI de GitHub](https://cli.github.com/) las comprueba:

```sh
gh attestation verify oci://quay.io/oxidestack/docx-to-pdf:1.3.1 --repo oxidestack/docx-to-pdf
gh attestation verify oci://quay.io/oxidestack/charts/docx-to-pdf:1.3.1 --repo oxidestack/docx-to-pdf
```

Para un SBOM, indique la imagen de una arquitectura por su digest (`docker buildx imagetools inspect` los enumera) y su
tipo de predicado, `--predicate-type https://spdx.dev/Document/v2.3`. Los SBOM también se adjuntan a la release de
GitHub como archivos, en SPDX y CycloneDX, uno por arquitectura, junto con el chart y los ejemplos de Compose.

Cada archivo adjunto a la release de GitHub está firmado de la misma manera, con su bundle de Sigstore adjunto al lado
como `<file>.sigstore.json`, y `docx-to-pdf-1.3.1.intoto.jsonl` es la procedencia de compilación de todos ellos. Los
bundles están en el formato de bundle actual de Sigstore, que `cosign verify-blob` lee por defecto a partir de cosign
3. cosign 2 solo los lee cuando se añade `--new-bundle-format` al comando; sin él, responde
`bundle does not contain cert for verification`. Descargue el archivo y su bundle y compruebe uno de los dos, o ambos:

```sh
cosign verify-blob docx-to-pdf-1.3.1.tgz --bundle docx-to-pdf-1.3.1.tgz.sigstore.json \
  --certificate-identity https://github.com/oxidestack/docx-to-pdf/.github/workflows/release.yml@refs/tags/v1.3.1 \
  --certificate-oidc-issuer https://token.actions.githubusercontent.com
gh attestation verify docx-to-pdf-1.3.1.tgz --repo oxidestack/docx-to-pdf
```

Lo mismo vale para los SBOM, los ejemplos de Compose y el `index.yaml` del repositorio de Helm. Los archivos de la
primera versión, 1.0.0, se adjuntaron antes de que la release los firmara, así que no tienen bundle: estos dos comandos
se aplican a partir de 1.1.0.

La réplica en ghcr.io lleva los mismos digests con las mismas firmas y atestaciones, copiadas con la imagen, así que
los mismos comandos verifican `ghcr.io/oxidestack/docx-to-pdf:1.3.1` y `ghcr.io/oxidestack/charts/docx-to-pdf:1.3.1`
(añada `--bundle-from-oci` a `gh attestation verify` para leer las atestaciones del propio registro).
