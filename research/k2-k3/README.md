# Dossier técnico de K2 y K3

Investigación de implementación sobre K0, commit `f713053b1fcdbd007256e67ff1bad1a838f8d877`, vault fundacional 0.1.0. La rama de trabajo es `research/k2-k3-groundwork`. K1 se desarrolla independientemente; no se ha utilizado su rama como evidencia ni se presupone su cierre.

El objetivo es retirar incertidumbre sobre autoridad, vida del trabajo, concurrencia y dispositivos antes de implementar K2/K3. Las conclusiones distinguen lo que especifica una fuente externa de las consecuencias deducidas para el contrato de Thalyx-Kernel.

| Documento | Uso |
|---|---|
| [k2.md](k2.md) | Handles, grants, admisión y transferencia, tickets, cobro, cancelación, fallos y restricciones ABI. |
| [k3.md](k3.md) | AP bring-up, interrupciones, memoria concurrente, scheduler, TLB, PCI, virtio-blk y aislamiento DMA. |
| [sources.md](sources.md) | 27 fuentes primarias efectivamente utilizadas, revisiones, localizadores, limitaciones y trazabilidad. |

## Cómo usarlo al comenzar cada fase

1. Leer el [estado canónico](../../vault/roadmap/current-state.md), la [ruta](../../vault/roadmap/phases.md) y los contratos enlazados por cada hallazgo. Si el vault cambia después de K0, contrastar los hallazgos afectados con ese cambio; este dossier no lo sustituye.
2. En K2, comenzar por K2-01–06 antes de fijar representación interna y esquema ABI. K2-07–13 delimitan las obligaciones que deben sobrevivir a IPC, fallos y cierre.
3. En K3, resolver el inventario `K1-dependent` antes de extender mecanismos de K1. K3-06–08 son la conexión entre SMP y las garantías de K2; K3-11–14 son la conexión entre dispositivos y drenaje.
4. Usar las tablas de interleavings como casos concretos de EXP-01–06 ya previstos. Son criterios para la implementación futura, no nuevos sprints ni resultados ejecutados.

## Categorías y autoridad

Cada hallazgo tiene una categoría principal: **A**, restricción ya capturada; **B**, detalle necesario de implementación; **C**, pregunta legítimamente abierta; **D**, posible contradicción objetiva con evidencia primaria. Un hallazgo B puede enlazar una obligación A sin convertir su mecanismo ilustrativo en arquitectura aceptada.

Los protocolos propuestos son deducciones de ingeniería compatibles con K0. No asignan nuevos opcodes, no fijan layouts adicionales y no sustituyen los ADR. Las cuestiones C incluyen qué dato falta y una vía conservadora compatible con el alcance existente.

**No se encontró una categoría D sustentada.** Se contrastaron especialmente la semántica de revocación de seL4, la atomicidad de IPC, permisos de mapas x86, el acceso virtio a través de IOMMU y los límites de drenaje VT-d. Las diferencias con otros sistemas no contradicen el vault. La selección física sigue abierta conforme a OQ-05.

## Dependencias concretas de K1

| ID | Información que se observará al cerrar K1 | Hallazgos afectados |
|---|---|---|
| KD-01 | Bootinfo: mapa físico reservado, RSDP/ACPI, vida de tablas y disponibilidad de una página baja para trampoline. | K3-01/02/09 |
| KD-02 | Allocator, locks y preempción; acceso/fijación/copia de buffers de usuario y recuperación de page faults durante copia. | K2-03/06/13; K3-05 |
| KD-03 | CR3/PTE, mapas globales, CR4.PCIDE/PGE, alias de direct map, atributos PAT/MTRR y forma de retirar tablas. | K2-14; K3-02/07/08/11 |
| KD-04 | GDT/IDT/TSS/IST, stacks de hilo/emergencia, syscall MSRs, TLS/per-CPU y marco de traps. | K2-12/13; K3-02/03 |
| KD-05 | Fuente de reloj, calibración, timer, puntos de cobro y semántica de habilitación de interrupciones. | K2-04/09; K3-04/06 |
| KD-06 | Layout y alineación de estado x87/SSE2, configuración CR0/CR4, puntos eager de guardado/restauración y flags efectivos de compilación. | K3-03/06 |

Estas dependencias afectan la adaptación al código que exista; no dejan sin investigar el contrato que ese código deberá sostener.

## Qué no demuestra

No hay código de kernel, prototipos, benchmarks ni ejecución de hardware en este dossier. Leer una especificación no comprueba un driver; leer una implementación verificada no transfiere sus pruebas. Los checks documentales y modelos finitos de K0 conservan su alcance original. No se declara K1, K2 o K3 implementado, ni durabilidad K4, ni aceptación física.

La investigación permanece en cuatro archivos de `research/k2-k3/`. Constitución, contratos, invariantes, decisiones, roadmap y estado de implementación permanecen intactos.

## Comprobaciones de este dossier

Se ejecutaron `python3 tools/check_vault.py` (PASS: 39 notas/IDs y 203 enlaces locales) y `python3 research/models/check_models.py` (MODEL-01: 294 estados/615 transiciones; MODEL-02: 23 casos de caída; controles negativos y caso ABA esperados). Los resultados versionados de K0 no se modificaron.

Se comprobaron además destinos de anchors del dossier, las 27 referencias utilizadas y ausencia de cambios fuera de estos cuatro documentos. Estas verificaciones validan documentación y modelos existentes, no código de K2/K3.
