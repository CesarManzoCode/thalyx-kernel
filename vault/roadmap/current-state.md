---
id: STATE-001
kind: evidence
status: observed
---
# Estado actual

**2026-09-08 · Fundación 0.1.0 · K0 y K1 completos. K2–K6 pendientes.**

## Qué existe

Un vault de 40 notas con constitución, 13 contratos de arquitectura, glosario, reconstrucción de Thalyx, 32 fuentes primarias anotadas, ocho decisiones, 23 invariantes, alternativas, integración Linux/nativa, experimentos y ruta de implementación. Se incluyen herramientas documentales y dos modelos finitos de investigación con controles negativos y resultado versionado.

Existe además un kernel que arranca. El workspace tiene loader UEFI, protocolo de arranque, kernel con `arch/x86_64`, ABI de llamadas y cuatro programas de usuario, con toolchain fijada y construcción reproducible. La imagen arranca en QEMU, ejecuta dominios en ring 3, los preempta con timer, contiene sus fallos ilegales y sobrevive. [Qué se ejecutó exactamente](../evidence/k1-protected-boot.md).

La base de evidencia de Thalyx está fijada al commit `0492f72e487e2463b0d7b938365a8b3383364cb9`: inventario de 407 commits alcanzables, 96 archivos del vault, 38 rutas de código/configuración de evidencia y 15 revisiones históricas seleccionadas. Inventariar no significa haber ejecutado ni auditado exhaustivamente cada archivo.

El repositorio Thalyx se mantuvo sin modificaciones durante este trabajo.

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
| Integridad documental | PASS: 40 IDs y enlaces locales. [Comprobador](../../tools/check_vault.py). Además, 36 destinos de código/historia de Thalyx resueltos contra Git. |
| Puerta K1 | PASS en 13 criterios decididos por separado desde los registros del kernel, con controles negativos. [Detalle y límites](../evidence/k1-protected-boot.md). |

## Qué no existe todavía

Capacidades, autoridad, IPC, ámbitos de trabajo, contabilidad de recursos, SMP, drivers propios, DMA, servicio de estado implementado, Thalyx sobre este kernel, pruebas de hardware físico, mediciones de rendimiento o prueba formal general. El plano de diagnóstico de K1 es temporal y no es el plano de recibos. No se ha retirado ni reemplazado Linux.

Un arranque protegido correcto no dice nada sobre las propiedades que K2–K6 deben demostrar.

## Siguiente trabajo

Ejecutar el paquete K2 descrito en [la ruta](phases.md): esquema ABI y bindings, tabla generacional, dominios, grants y ámbitos, reservas, IPC copiado, tickets, barrera y drenaje, y un servicio supervisor con capacidades de arranque explícitas. La primera vertical es un cliente sin permisos ambientales que solicita una operación, deriva autoridad, consume presupuesto y se cancela mientras el servidor retiene trabajo.

No hay una elección técnica pendiente que deba devolver el diseño al usuario. [Las preguntas abiertas](open-questions.md) especifican qué dato falta y con qué decisión conservadora avanzar.
