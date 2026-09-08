# Fuentes primarias y trazabilidad

Consulta: **2026-09-08** para todas las entradas. Se utilizaron **27 fuentes primarias externas**; se cuentan documentos o unidades de documentación distintas, no secciones, enlaces repetidos ni los contratos locales. La base local es K0 `f713053b1fcdbd007256e67ff1bad1a838f8d877`.

Las secciones «Implicación», «Deducción» y los interleavings del dossier son análisis de ingeniería sobre K0. No se atribuyen a las fuentes como implementaciones de Thalyx-Kernel. Se leyeron los pasajes identificados; no se declara lectura integral de todos los manuales ni reproducción de resultados experimentales externos.

## R01

**seL4 Reference Manual.** seL4 Foundation; autores y contribuidores, edición **16.0.0, 22 julio 2026**. [PDF oficial](https://sel4.systems/Info/Docs/seL4-manual-latest.pdf).

Pasajes: §§2.4, 3.1–3.3 (untyped, CNodes, derechos, CDT, Delete/Revoke); §§4.2.1–4.2.4 (badges, transferencias y errores, Call/Reply); capítulo 5 (notificaciones); §§6.1–6.2 (ejecución/faults). Uso: K2-02–05/07/08/10–12. Evidencia concreta: copias hermanas en CDT, revocación que conserva la raíz, transferencia parcial, receive slot/unwrapping, reply ligado al caller y self-revocation que puede interrumpir el recorrido.

El enlace `latest` es mutable; la versión y hash inferior identifican la copia leída. El manual distingue MCS y no-MCS: no mezclar el reply implícito del segundo con los reply objects del primero. No se afirma que estas reglas constituyan un protocolo de drenaje de K0.

## R02

**MCS tutorial — Mixed-Criticality System extensions.** Proyecto seL4. [Documentación oficial](https://docs.sel4.systems/Tutorials/mcs.html). Página viva sin versión de kernel fijada por la URL; complementada con R01 16.0.0 para distinguir variantes.

Pasajes: scheduling contexts, budget/period, replenishments, passive servers, timeout faults y reply objects. Uso: K2-09 y K3-06. Sustenta préstamo/devolución del contexto y separación de hilo/presupuesto; no cuotas de ventana fija ni jerarquía de scopes de Thalyx-Kernel. No se extrapolan benchmarks ni alcance de pruebas MCS.

## R03

**What the Proofs Assume.** seL4 Foundation. [Página oficial](https://sel4.systems/Verification/assumptions.html). Documentación viva, sin revisión numerada.

Pasajes: Assembly, Hardware, Hardware management, Boot code, DMA y Information side-channels. Uso: K2-14 y límites generales de evidencia. Sustenta que las conclusiones de una prueba dependen de modelo/configuración y de fronteras explícitas de hardware/arranque. No se usa como inventario exhaustivo de qué plataformas/configuraciones están verificadas en 2026.

## R04

**Zircon Handles.** Fuchsia. [Documentación oficial](https://fuchsia.dev/fuchsia-src/concepts/kernel/handles). La página consultada indica actualización **28 febrero 2025**.

Pasajes: alcance local del entero, derechos, creación/transferencia y «Invalid Handles and handle reuse». Uso: K2-01/03. El kernel puede reutilizar valores; la documentación advierte del uso/cierre concurrente. No se atribuye a Zircon el layout generacional de 64 bits de K0.

## R05

**zx_channel_write_etc.** Fuchsia, referencia de syscalls. [Documentación oficial](https://fuchsia.dev/reference/syscalls/channel_write_etc). Página viva; no se fijó una revisión de implementación.

Pasajes: `zx_handle_disposition_t`, MOVE/DUPLICATE, tratamiento de errores y condición de escritura del mensaje. Uso: K2-05. Sustenta específicamente que MOVE cierra la fuente y que el conjunto debe tener éxito para escribir el mensaje. Distingue consumo fuente y atomicidad de publicación; no se presenta como semántica aceptada para Thalyx-Kernel.

## R06

**Capsicum: practical capabilities for UNIX.** Robert N. M. Watson, Jonathan Anderson, Ben Laurie y Kris Kennaway; **USENIX Security 2010**. [Paper de los autores](https://www.cl.cam.ac.uk/research/security/capsicum/papers/2010usenix-security-capsicum-website.pdf).

Pasajes: §§2.1–2.3, capability mode, wrapping de descriptores, nombres relativos y preparación del sandbox. Uso: K2-08. Se emplea la carrera documentada de reordenación de directorios con capacidades múltiples y el tratamiento de autoridad ambiental. Es evidencia histórica del diseño del paper; no describe toda la API actual de FreeBSD.

## R07

**Resource Containers: A New Facility for Resource Management in Server Systems.** Gaurav Banga, Peter Druschel y Jeffrey C. Mogul; **OSDI 1999**. [Paper original en USENIX](https://www.usenix.org/legacy/events/osdi99/full_papers/banga/banga.pdf).

Pasajes: §§3–4, especialmente 4.1–4.3: actividad que cruza procesos, resource bindings y scheduler bindings. Uso: K2-09/11 y K3-06. El paper deja fuera de su exposición un modelo completo de control de acceso. No demuestra la autenticidad de scopes, cancelación ni conservación de presupuesto SMP de K0. No se reutilizan sus cifras de rendimiento.

## R08

**core::sync::atomic — Memory model for atomic accesses.** Proyecto Rust. [Documentación oficial](https://doc.rust-lang.org/core/sync/atomic/index.html). Copia servida como **core 1.98.1**; URL viva, no toolchain del proyecto.

Pasajes: reglas relacionadas con C++20, happens-before, data races y accesos atómicos de tamaño distinto. Uso: K2-03 y K3-05. Sustenta por qué un contador atómico o la memoria fuerte de x86 no legitiman accesos concurrentes ordinarios. No se atribuye a atomics una garantía de vida automática.

## R09

**core::ptr::read_volatile.** Proyecto Rust. [Referencia oficial](https://doc.rust-lang.org/core/ptr/fn.read_volatile.html). Documentación viva consultada junto a R08.

Pasajes: semántica de volatile, memoria dentro/fuera de allocation y Safety. Uso: K3-05. Un acceso volatile no es atómico ni sincroniza threads; las condiciones de MMIO y validez continúan siendo obligaciones de `unsafe`. No se deduce que cualquier dirección física pueda leerse sin fallar.

## R10

**The Rust Reference — Behavior considered undefined.** Proyecto Rust. [Referencia oficial](https://doc.rust-lang.org/reference/behavior-considered-undefined.html). Página viva sin snapshot de compilador independiente.

Pasajes: data races, dangling/misaligned pointers, aliasing y producción de valores inválidos. Uso: K2-03/06/13 y K3-05. Aplicación: vidas, usercopy y parsing de ABI desde bytes hostiles. La propia referencia no constituye una especificación formal cerrada de todo aliasing Rust; el dossier no inventa una.

## R11

**The Rust Reference — Type layout.** Proyecto Rust. [Referencia oficial](https://doc.rust-lang.org/reference/type-layout.html). Página viva.

Pasajes: size/alignment, Rust representation, C representation, enums y representaciones anidadas. Uso: K2-06/13. Sustenta que `repr(C)` no convierte objetos Rust arbitrarios en bytes ABI seguros. Las reglas little-endian, límites y reservados proceden de K0, no de Rust.

## R12

**Intel 64 and IA-32 Architectures Software Developer’s Manual, Volume 3A: System Programming Guide, Part 1.** Intel; **253668-092US, junio 2026**. [PDF de revisión consultada](https://cdrdv2-public.intel.com/922487/253668-092-sdm-vol-3a.pdf); [entrada oficial de descarga](https://cdrdv2.intel.com/v1/dl/getContent/671190).

Pasajes leídos/selectivamente contrastados: capítulos 2/7/10 sobre estado, descriptores y traps; §§5.1, 5.6, 5.8, 5.10.4–5.10.5 sobre modos, permisos, A/D e invalidaciones; §§11.2–11.4 sobre memoria, serialización y MP initialization; §§13.12.3/13.12.9 sobre orden MSR e ICR x2APIC. Localizadores decisivos: páginas impresas **5-48–5-53, 11-21–11-24, 13-41 y 13-45–13-46**.

Uso: K2-14 y K3-02–05/07/08. Esta revisión renumera capítulos respecto de ediciones antiguas: no trasladar ciegamente «4.10» o «8.4» de otro SDM. No se afirma lectura completa de volúmenes ni conformidad AMD; no se usa el índice como sustituto de las secciones leídas.

## R13

**x86-TSO: A Rigorous and Usable Programmer’s Model for x86 Multiprocessors.** Peter Sewell, Susmit Sarkar, Scott Owens, Francesco Zappa Nardelli y Magnus O. Myreen; **Communications of the ACM, 2010**. [Paper de los autores](https://www.cl.cam.ac.uk/~pes20/weakmemory/cacm.pdf).

Pasajes: alcance del modelo, store buffers, ejemplos y exclusiones expresas. Uso: K3-05. Se separa memoria write-back ordinaria de cambios de tablas, accesos no temporales y otros casos fuera del modelo. No es fuente normativa de MMIO/DMA ni del compilador Rust.

## R14

**Advanced Configuration and Power Interface Specification 6.6, capítulo 5.** UEFI Forum; **mayo 2025**. [Especificación oficial](https://uefi.org/specs/ACPI/6.6/05_ACPI_Software_Programming_Model.html).

Pasajes: §§5.2.5–5.2.8 (raíces/headers) y 5.2.12, especialmente entradas Local APIC, IOAPIC, Interrupt Source Override, Local x2APIC y §5.2.12.19 Multiprocessor Wakeup. Uso: K3-01/04. Sustenta enumeración, IDs/routing y existencia del mailbox opcional. No se ha auditado un intérprete AML ni probado tablas de una máquina física.

## R15

**UEFI Platform Initialization Specification 1.8, Volume 2, DXE Boot Services Protocol.** UEFI Forum. [Especificación oficial, capítulo 13](https://uefi.org/specs/PI/1.8/V2_DXE_Boot_Services_Protocols.html).

Pasajes: §§13.3–13.4, MP Services availability y restricciones previas a ExitBootServices. Uso: K3-01. Sustenta que el servicio firmware no es runtime SMP del kernel. No exige usar MP Services como método de arranque de K3.

## R16

**82093AA I/O Advanced Programmable Interrupt Controller (IOAPIC).** Intel; ficha **preliminary, mayo 1996, 290566-001**. [PDF original alojado por MIT](https://pdos.csail.mit.edu/6.828/2018/readings/ia32/ioapic.pdf).

Pasajes: §§3.1–3.2.4, páginas impresas 8–12: IOREGSEL/IOWIN, versión, redirección, máscaras y Remote IRR. Uso: K3-04. Fuente primaria histórica, no documentación normativa de todo IOAPIC moderno. No se trasladan su cantidad fija de pines, ancho de APIC ID ni chipset a q35/hardware nuevo. Serializar índice/datos y evitar reuso con eventos pendientes son implicaciones deducidas.

## R17

**Linux, arch/x86/mm/tlb.c.** Autores del kernel Linux; **v6.12**, commit `adc218676eef25575469234709c2d87185ca223a`. [Archivo fijado](https://github.com/torvalds/linux/blob/adc218676eef25575469234709c2d87185ca223a/arch/x86/mm/tlb.c).

Pasajes: `switch_mm_irqs_off`, selección de ASID/generación, `flush_tlb_mm_range` y handlers remotos; especialmente comentarios sobre cpumask y `tlb_gen`. Uso: K3-07. Se contrastó el archivo de v6.12 y se resolvió la etiqueta a ese commit. Evidencia de una implementación concreta que coordina entrada al espacio y shootdown; no código copiado ni arquitectura lock-free requerida.

## R18

**Linux — ACPI considerations for PCI host bridges.** Autores del kernel Linux; **v6.12**, mismo commit que R17. [Documento fijado](https://github.com/torvalds/linux/blob/adc218676eef25575469234709c2d87185ca223a/Documentation/PCI/acpi-info.rst).

Pasajes: host bridges, `_CRS`, `_PRT`, MCFG/ECAM y base correspondiente a bus cero. Uso: K3-09. Incluye referencias normativas precisas a ACPI, PCI Express 4.0 §7.2.2 y PCI Firmware 3.2 §4.1.2. Se usa como documentación primaria de integración Linux; las especificaciones PCI-SIG citadas dentro no se cuentan como fuentes íntegramente consultadas.

## R19

**Linux — How To Write Linux PCI Drivers.** Autores del kernel Linux; documentación **6.12**. [Documento oficial](https://docs.kernel.org/6.12/PCI/pci.html).

Pasajes: inicialización del dispositivo, reserva de recursos, DMA y `pci_set_master`. Uso: K3-09. La nota sobre orden de reserva/habilitación de recursos muestra que ni siquiera una secuencia de una implementación conocida debe copiarse sin analizar sus condiciones. No se portan APIs Linux ni se declara probado PCI de Thalyx-Kernel.

## R20

**Linux — The MSI Driver Guide HOWTO.** Autores del kernel Linux; documentación **6.12**. [Documento oficial](https://docs.kernel.org/6.12/PCI/msi-howto.html).

Pasajes: MSI/MSI-X, ordering respecto de datos, asignación/afinidad y locking con múltiples interrupciones. Uso: K3-10. Las funciones Linux ilustran requisitos; no fijan opcodes, vectores ni política IRQ de K0. Una IRQ no sustituye el protocolo de completion del dispositivo.

## R21

**Linux — Dynamic DMA mapping Guide.** Autores del kernel Linux; documentación **6.12**. [Documento oficial](https://docs.kernel.org/6.12/core-api/dma-api-howto.html).

Pasajes: CPU and DMA addresses, DMA masks, coherent/streaming mappings, ownership, coherencia y barreras. Uso: K3-05/11. Sustenta separación de direcciones y necesidad de barreras incluso con memoria coherente. No se considera que una llamada API Linux esté disponible en el nuevo kernel.

## R22

**Linux kernel memory barriers.** Autores del kernel Linux; documentación **6.12**. [Documento oficial](https://docs.kernel.org/6.12/core-api/wrappers/memory-barriers.html).

Pasajes: SMP barriers, DMA ordering, kernel I/O barrier effects y coherencia frente a DMA/MMIO. Uso: K3-05/11. Sirve para distinguir las capas de orden y posted writes; no reemplaza el SDM o la especificación del dispositivo ni define por sí sola el modelo de Rust.

## R23

**VFIO — Virtual Function I/O.** Autores del kernel Linux; documentación **6.12**. [Documento oficial](https://docs.kernel.org/6.12/driver-api/vfio.html).

Pasajes: Groups, Devices, and IOMMUs; unidad de propiedad DMA y topología que reduce aislamiento. Uso: K3-14. Evidencia concreta: funciones con comunicación interna, bridges sin ACS y requester IDs compartidos. No se adopta la API VFIO ni se considera un «grupo» de software prueba suficiente de aislamiento físico.

## R24

**Virtual I/O Device (VIRTIO) Version 1.2.** OASIS Virtual I/O Device TC; **Committee Specification 01, 1 julio 2022**. [Revisión explícita](https://docs.oasis-open.org/virtio/virtio/v1.2/cs01/virtio-v1.2-cs01.html); [URL consultada que identifica esa revisión](https://docs.oasis-open.org/virtio/virtio/v1.2/virtio-v1.2.html).

Pasajes: §§2.4 (reset), 2.5 (config generation), 2.6.1 (queue reset), 2.7 (split virtqueues y barreras), 3.1/3.3 (inicialización/cleanup), 4.1 (PCI), 5.2 (bloque) y 6 (features reservadas). Uso: K3-09/12/13/15. Restricciones decisivas: ACCESS_PLATFORM/ORDER_PLATFORM, FEATURES_OK, propiedad de anillos, reset confirmado, status y condiciones de estabilidad en §5.2.6.2.

Se analizó la ruta relevante a virtio-blk; no todos los dispositivos de la especificación. No se usa el SHOULD de una feature opcional como obligación de implementarla en K3: una feature no gestionada no se negocia, salvo que su ausencia haga insuficiente el perfil requerido.

## R25

**Intel Virtualization Technology for Directed I/O Architecture Specification.** Intel; **revisión 5.20, abril 2026, D51397-019**. [PDF oficial de la revisión consultada](https://cdrdv2-public.intel.com/919688/D51397-019-vt-directed-io-spec.pdf); [entrada de descarga](https://cdrdv2.intel.com/v1/dl/getContent/919688).

Pasajes: capítulos 3–4 (remapping/Device-TLB), 5 (interrupt remapping), 6 (caches, invalidación y write buffers), §§8.3–8.4 (DRHD/device scopes/RMRR), y campos relevantes de §11.4 (CAP/ECAP, estado/control). Localizadores centrales: **§6.5.2.9, pp. 6-35–6-36; §6.5.4, pp. 6-45–6-46; §11.4.2, pp. 11-10–11-12**.

Uso: K3-10/13/14. Se distingue el comportamiento DR/DW según versión de hardware, completion de wait, tráfico traducido ATS y límites para peer destinations. No se asume que todos los equipos implementan 5.20, todos sus modos o ATS/PASID/PRI. Las opciones se descubren en hardware; este documento no certifica AMD-Vi.

## R26

**QEMU — Invocation.** Proyecto QEMU; documentación **master**, misma serie documental consultada que R27 (**11.1.50**, desarrollo). [Manual oficial](https://www.qemu.org/docs/master/system/invocation.html).

Pasajes: `-device intel-iommu`, `intremap`, `caching-mode`, `device-iotlb`, `aw-bits` y restricciones de irqchip/acelerador. Uso: K3-15. URL mutable; las opciones/defaults se deben contrastar con la versión exacta instalada. No se ha ejecutado un comando de K3 ni se presenta master como release seleccionada.

## R27

**QEMU — Multi-threaded TCG.** Proyecto QEMU; documentación consultada **11.1.50**, rama master. [Documento oficial](https://www.qemu.org/docs/master/devel/multi-thread-tcg.html).

Pasajes: diseño general, thread por vCPU, fallback single-thread/round-robin, interacción con icount y memory consistency. Uso: K3-15. Demuestra que «TCG con varios vCPU» no identifica por sí solo el modo de ejecución. El texto mezcla descripción y requisitos de diseño; no se usan sus apartados prospectivos como resultados experimentales.

## Identidad de las copias consultadas

Los hashes identifican bytes descargados, no avalan su contenido ni convierten una URL viva en archivo inmutable. No se redistribuyen manuales/papers en el repositorio. Para código Linux, el enlace por commit es la referencia reproducible principal.

| Fuente | SHA-256 de la copia usada |
|---|---|
| R01 PDF | `dca31cf74703b1588247064d37db5951727449d16eed621ae260bc7e24484037` |
| R06 PDF | `17f0413266519e976f997fadcbe96b10d6a074881fd416066b63a6d0fbf616aa` |
| R12 PDF | `744e6ac6ac6eed2d206424c605cd13fad117b95f60f49a0f3a6d35f361efb00d` |
| R13 PDF | `98cb0ebfc0f06318d5076d9922fba2619c05f64a6e0314fbe9b054995ab2284e` |
| R16 PDF | `0fdf5f2631181d3093ff2feb31d599f7f3714ab70d162357c97f0704c4fe9c84` |
| R17 código | `5f4275e07cfaae899034385fd885a83875f167c555fcc7e3ac19abdbd412e144` |
| R18 documento | `669144e63878d0b546c59d05e9b323b31e0be2ca1463c9d1eb40bcab6a746aa4` |
| R24 HTML de URL consultada | `4bcebecc0150ec604a6e3054eca8adf2cd42621891f13890417bf3f39a4e12bb` |
| R25 PDF | `44179074934b5d28f49e0302805d69e74bd4e7ad323525ef127d6e99e0c0a2fa` |

## Alcance de la clasificación

No hay hallazgos D con evidencia suficiente. No se confunde una política distinta —seL4 CDT, Zircon MOVE, MCS refills— con contradicción del vault. Las cuestiones C quedan localizadas en [K2](k2.md) y [K3](k3.md): consumo MOVE/ABI, faults, fuente temporal, plataforma física y adaptación al código de K1.

No se cuentan como fuentes utilizadas los resultados de buscador, blogs, páginas inaccesibles de PCI-SIG, referencias internas de un paper que no se leyeron ni el inventario histórico de Thalyx. No se auditó la rama K1. Las condiciones de aislamiento físico y rendimiento quedan por validar en sus fases, tal como exige K0.
