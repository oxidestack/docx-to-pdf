# Actualización

<!-- languages:begin -->
[English](../../../guide/upgrading.md) · [Русский](../../ru/guide/upgrading.md) · [中文](../../zh/guide/upgrading.md)
· **Español**
<!-- languages:end -->

Cómo llevar un despliegue de la versión que ejecuta a una más nueva. Busque la sección de cada versión más nueva que
la suya y aplíquelas empezando por la más antigua: pasar de 1.0.0 a 1.2.0 significa la sección de 1.1.0 y después
la de 1.2.0. Una versión que no requiere nada lo dice. Después [actualice](#cómo-actualizar) a la versión más
reciente en un solo paso: nunca hace falta instalar antes una versión intermedia.

Cada sección enumera lo que la versión pide a un despliegue o a un cliente: en **Action required**, un cambio que
hacer antes de la actualización o junto con ella; en **Deprecated**, algo que todavía funciona y dejará de hacerlo en
una versión posterior; en **Removed**, algo que ya ha dejado de funcionar. Todo lo demás que cambió cada versión está
en el [changelog](https://github.com/oxidestack/docx-to-pdf/blob/main/CHANGELOG.md). Solo las versiones tienen
sección: una versión candidata (`-rc.N`) se describe en su release de GitHub, y lo que cambia se escribe aquí bajo la
versión que lo publica.

La documentación de la versión que usted ejecuta también está en este sitio: el selector de versión en la parte
superior de cada página cambia entre versiones, y una página de una versión anterior enlaza la misma página en la
última.

## Cómo actualizar

docx-to-pdf no guarda nada entre conversiones: no tiene base de datos ni archivos que migrar, así que una
actualización sustituye los contenedores y nada más, y una vuelta atrás es el mismo paso con la versión anterior.

1. Lea las secciones de abajo de cada versión más nueva que la suya y aplique sus cambios a su configuración, a los
   valores del chart o a sus clientes.
2. Compruebe la nueva imagen o el nuevo chart, si verifica las versiones
   ([Verifying a release](../../../guide/installation.md#verifying-a-release)).
3. Sustituya los contenedores:
   - `docker run` o Docker Compose: cambie la etiqueta de la imagen (`quay.io/oxidestack/docx-to-pdf:<version>`) y
     vuelva a arrancarla (`docker compose up -d`).
   - Helm: ejecute el mismo `helm upgrade --install` que en
     [Installation](../../../guide/installation.md#with-helm), con el nuevo `--version` y su propio archivo de
     valores. El chart arranca cada pod nuevo antes de parar uno antiguo; un pod nuevo acepta conversiones en cuanto
     `/readyz` responde `200`, y uno antiguo termina las conversiones que tiene antes de pararse
     ([Operations](../../../guide/operations.md#stopping)).
4. Vigile los primeros minutos: `/readyz`, los estados de las respuestas y las métricas de conversión
   ([Operations](../../../guide/operations.md#telemetry)). Para volver atrás, ejecute el paso 3 con la versión que
   tenía.

## Actualizar a 1.2.0

Nada que cambiar: la imagen y el chart se actualizan en su sitio desde 1.1.0.

## Actualizar a 1.1.0

Nada que cambiar: la imagen y el chart se actualizan en su sitio desde 1.0.0.
