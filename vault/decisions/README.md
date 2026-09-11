---
id: ADR-INDEX
kind: navigation
status: accepted
---
# Registro de decisiones

Aceptadas para la arquitectura inicial el 2026-09-08; ADR-009 se acepta el 2026-09-09, después de ejecutar el camino de dispositivo que decide; ADR-010 el 2026-09-11, después de medir lo que revisa. «Aceptada» expresa una decisión de diseño; ninguna afirma implementación. Las condiciones de revisión son preguntas de ingeniería, no puertas de permiso al usuario.

| ID | Decisión | Contrato principal |
|---|---|---|
| [ADR-001](ADR-001-kernel-boundary.md) | Microkernel con ámbitos, sin política semántica en ring 0. | [Arquitectura](../architecture/overview.md) |
| [ADR-002](ADR-002-authority-and-revocation.md) | Capacidades atenuables; admisión, barrera y drenaje separados. | [Autoridad](../architecture/authority.md) |
| [ADR-003](ADR-003-managed-state.md) | Versiones inmutables y publicación durable en un servicio. | [Persistencia](../architecture/persistence.md) |
| [ADR-004](ADR-004-work-accounting.md) | Recursos por ámbito y préstamo explícito a servidores. | [Recursos](../architecture/resources.md) |
| [ADR-005](ADR-005-platform-and-language.md) | x86_64/UEFI, Rust y protección por hardware. | [Hardware](../architecture/hardware.md) |
| [ADR-006](ADR-006-interfaces-and-compatibility.md) | ABI propio y compatibilidad de fuente en usuario. | [ABI](../architecture/abi.md) |
| [ADR-007](ADR-007-evidence-and-determinism.md) | Evidencia con cobertura; determinismo restringido. | [Observabilidad](../architecture/observability.md) |
| [ADR-008](ADR-008-linux-and-consumers.md) | Linux permanente; Thalyx consumidor sin acoplar el kernel a su vocabulario. | [Integración](../integration/thalyx.md) |
| [ADR-009](ADR-009-device-path-and-dma-profiles.md) | Camino de dispositivo mínimo con MSI-X; perfil DMA fuerte rechazado, no aproximado. | [Hardware](../architecture/hardware.md) |
| [ADR-010](ADR-010-k6-parameters-and-wake-policy.md) | Parámetros V0 revisados con medida: traza retenida y declarada, presupuesto de raíz por procesador, log de 256 celdas con lectura que espera, ranuras recicladas bajo demanda, cerrojo de turno; despertar con plazo de gracia. | [Recursos](../architecture/resources.md) |

La persistencia de capacidades se rechaza en ADR-003; la distribución dentro del kernel, en ADR-007. Los límites cuantitativos V0 fueron decisiones provisionales registradas en los contratos y [preguntas abiertas](../roadmap/open-questions.md); K6 midió cinco de ellos sobre KVM y ADR-010 registra cuáles cambió y por qué. El resto sigue sin medir.
