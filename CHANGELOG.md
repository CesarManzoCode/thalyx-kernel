# Historial del proyecto

## 0.2.0 — 2026-09-08

K1 — Arranque protegido. Primer código ejecutable del proyecto.

- Workspace Rust con toolchain fijada, dos targets bare-metal y construcción reproducible.
- Loader UEFI con validación acotada de ELF y del paquete de arranque, tablas iniciales y handoff.
- Kernel con posesión de marcos físicos y tablas propias, GDT/IDT/TSS con pilas de emergencia, reloj medido, timer periódico del LAPIC, dominios en espacios separados, planificador con preempción, entrada de syscall y estado FP por dominio.
- ABI de llamadas y runtime de usuario mínimo; cuatro dominios, dos de ellos con accesos ilegales deliberados.
- Imagen arrancable en QEMU con evidencia reproducible byte a byte y manifiesto de digests.
- Comprobador de la puerta K1: 13 criterios decididos por separado desde los registros del kernel, validado con controles negativos.
- Registro de lo ejecutado, su alcance y sus límites en el vault.

El plano de diagnóstico de K1 es temporal y no es el plano de recibos. No se incorporan capacidades, autoridad, IPC, SMP, drivers, estado durable ni resultados de rendimiento.

## 0.1.0 — 2026-09-08

Primera constitución técnica de Thalyx-Kernel, construida desde un repositorio vacío.

- Reconstrucción de Thalyx a partir de código, vault e historial fijados a una revisión.
- Arquitectura derivada: capacidades, ámbitos de trabajo, IPC con atribución y publicación versionada en espacio de usuario.
- Contratos de autoridad, memoria, recursos, fallos, persistencia, hardware y ABI.
- Registro de alternativas, fuentes primarias, invariantes y obligaciones de validación.
- Separación de las rutas Thalyx/Linux y Thalyx/Thalyx-Kernel.
- Secuencia de implementación desde arranque hasta un consumidor real y comparación equivalente.
- Modelos finitos de investigación y revisión de coherencia del vault.

No se incorpora código de kernel, una imagen arrancable ni resultados de rendimiento.
