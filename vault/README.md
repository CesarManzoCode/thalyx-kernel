---
id: NAV-001
kind: navigation
status: accepted
---
# Vault fundacional

Este vault es la constitución técnica inicial de Thalyx-Kernel. Su unidad de lectura es una arquitectura completa: una decisión sobre IPC no puede ignorar cancelación, recursos, autoridad o recuperación.

## Lectura inicial

1. [Constitución](constitution.md): propósito, propiedades, límites y criterio de inclusión.
2. [Thalyx real](evidence/thalyx.md), [historia](evidence/history.md) y [frontera con Linux](evidence/linux-boundary.md): de dónde proceden las necesidades.
3. [Derivación y arquitectura](architecture/overview.md): por qué existe cada frontera.
4. [Invariantes](validation/invariants.md): propiedad, mecanismo y comprobación exigida.
5. [Estado actual](roadmap/current-state.md) y [ruta de implementación](roadmap/phases.md): qué existe y qué se construye después.

## Contratos de arquitectura

| Tema | Documento |
|---|---|
| Vocabulario y distinciones | [Glosario](glossary.md) |
| Objetos y propiedad | [Modelo de objetos](architecture/objects.md) |
| Delegación, vencimiento y revocación | [Autoridad](architecture/authority.md) |
| Trabajo, procesos y cancelación | [Ejecución](architecture/execution.md) |
| Llamadas, atribución y colas | [IPC](architecture/ipc.md) |
| Memoria, versiones y DMA | [Memoria](architecture/memory.md) |
| CPU, límites y contabilidad | [Recursos](architecture/resources.md) |
| Estado durable y efectos | [Persistencia](architecture/persistence.md) |
| Evidencia y causalidad | [Observabilidad](architecture/observability.md) |
| Orden, fallos y concurrencia | [Concurrencia](architecture/concurrency.md) |
| Arranque, plataforma y lenguaje | [Hardware](architecture/hardware.md) |
| Interfaz binaria | [ABI](architecture/abi.md) |
| Distribución y repetibilidad | [Distribución y determinismo](architecture/distributed-determinism.md) |

## Integración, decisiones y trabajo

- [Frontera con Thalyx y portabilidad](integration/thalyx.md).
- [Linux y comparación real](integration/linux-comparison.md).
- [Fuentes primarias](research/sources.md) y [alternativas evaluadas](research/alternatives.md).
- [Registro de decisiones](decisions/README.md).
- [Plan experimental](validation/experiments.md) y [auditoría de coherencia](validation/audit.md).
- [Ejecución de K1 y sus límites](evidence/k1-protected-boot.md).
- [Ejecución de K2 y sus límites](evidence/k2-objects-authority-work.md).
- [Preguntas abiertas](roadmap/open-questions.md).
- [Gobierno del conocimiento](governance.md).
- [Manifiesto de fuentes inspeccionadas](evidence/source-manifest.json).
- [Modelos ejecutables de investigación](../research/models/README.md).

## Cómo interpretar una afirmación

**Hecho observado** significa lectura estática o ejecución identificada; siempre se especifica cuál. **Decisión aceptada** obliga a la primera implementación, pero puede revisarse con evidencia. **Contrato diseñado** describe comportamiento requerido, no comportamiento existente. **Hipótesis** requiere experimento. **Pendiente** conserva una incertidumbre o una obligación concreta.

La precedencia es: requisitos explícitos del proyecto → constitución → decisiones vigentes → contratos → planes. La evidencia no se reescribe para ajustarla a una decisión. Si un contrato contradice otro, la precedencia no autoriza ignorarlo silenciosamente: hay que registrar y resolver la contradicción.

Versión fundacional: **0.1.0**, fecha de corte **2026-09-08**. No existe compatibilidad ABI estable.
