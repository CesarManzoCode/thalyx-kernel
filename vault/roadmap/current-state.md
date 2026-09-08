---
id: STATE-001
kind: evidence
status: observed
---
# Estado actual

**2026-09-08 · Fundación 0.1.0 · K0 completo. K1–K6 pendientes.**

## Qué existe

Un vault de 39 notas con constitución, 13 contratos de arquitectura, glosario, reconstrucción de Thalyx, 32 fuentes primarias anotadas, ocho decisiones, 23 invariantes, alternativas, integración Linux/nativa, experimentos y ruta de implementación. Se incluyen herramientas documentales y dos modelos finitos de investigación con controles negativos y resultado versionado.

La base de evidencia de Thalyx está fijada al commit `0492f72e487e2463b0d7b938365a8b3383364cb9`: inventario de 407 commits alcanzables, 96 archivos del vault, 38 rutas de código/configuración de evidencia y 15 revisiones históricas seleccionadas. Inventariar no significa haber ejecutado ni auditado exhaustivamente cada archivo.

El repositorio Thalyx se mantuvo sin modificaciones durante este trabajo. El repositorio Thalyx-Kernel estaba vacío al inicio; esta fundación introduce sus primeros artefactos.

## Qué está decidido

Microkernel de capacidades con ámbitos de trabajo; dominios de memoria como frontera adversaria; autoridad por grants/facetas; IPC con origen y cargo; barrera/drenaje/retirada separados; memoria sellada; CPU con cuotas agregadas y recuperación reservada; estado versionado y publicación durable en usuario; x86_64/UEFI y Rust; ABI propio; port de fuente y Linux permanente.

Las decisiones son contratos para implementar, no resultados del sistema. [Registro de decisiones](../decisions/README.md).

## Evidencia ejecutada aquí

| Comprobación | Resultado y alcance |
|---|---|
| Reconstrucción de Thalyx | Lectura estática de código, vault, historial y pruebas existentes. Sin build ni ejecución de Thalyx. |
| MODEL-01 | 294 estados / 615 transiciones, con contraejemplos de las afirmaciones/variantes incorrectas. |
| MODEL-02 | 23 casos de caída del protocolo correcto; variantes incorrectas detectadas. |
| Caso ABA | Confirma la necesidad de generación para rechazar expectativas antiguas sobre contenido repetido. |
| Revisión arquitectónica | Hallazgos y correcciones registrados en [la auditoría](../validation/audit.md). |
| Integridad documental | PASS: 39 IDs y 116 enlaces locales. [Comprobador](../../tools/check_vault.py). Además, 36 destinos de código/historia de Thalyx resueltos contra Git. |

## Qué no existe todavía

Kernel ejecutable, loader de este proyecto, runtime nativo, imagen arrancable, drivers propios, servicio de estado implementado, Thalyx sobre este kernel, pruebas de hardware, mediciones de rendimiento o prueba formal general. No se ha retirado ni reemplazado Linux.

## Siguiente trabajo

Ejecutar el paquete K1 descrito en [la ruta](phases.md): workspace/targets, loader/bootinfo, memoria propia, traps/timer, ring 3, dos dominios y un fallo contenido con preempción. Los permisos y políticas del consumidor no necesitan rediseñarse para empezar.

No hay una elección técnica pendiente que deba devolver el diseño al usuario. [Las preguntas abiertas](open-questions.md) especifican qué dato falta y con qué decisión conservadora avanzar.
