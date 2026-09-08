---
id: RES-001
kind: research
status: observed
---
# Fuentes primarias y alcance de investigación

Consulta realizada el **2026-09-08**. Se priorizaron diseños originales, documentación mantenida por sus proyectos y código fijado a revisión. «Consultado» no significa reproducido ni validado en hardware. Las páginas vivas pueden cambiar; el vault conserva la conclusión y su límite, no una copia de cada fuente.

El análisis de código e historia de Thalyx está separado en [EVD-001](../evidence/thalyx.md), [EVD-002](../evidence/history.md) y [su manifiesto](../evidence/source-manifest.json). Las conclusiones arquitectónicas son una síntesis propia y no resultados experimentales de estas fuentes.

## Protección, autoridad y frontera

**S01 — seL4, API Reference.** [Documentación del proyecto](https://docs.sel4.systems/projects/sel4/api-doc.html). Se consultaron capacidades, endpoints, llamadas, memoria y badges en Mint/Recv. Aporta mecanismos concretos para separar referencias, derechos y objetos y vincular una llamada a autoridad de servicio. No obliga a copiar CSpaces ni sus reglas exactas de mint. Uso: modelo de autoridad, facetas e IPC, ADR-001/002.

**S02 — seL4, MCS tutorial.** [Mixed-Criticality System extensions](https://docs.sel4.systems/Tutorials/mcs.html). Se revisaron scheduling contexts, budget/period, refills, servidores pasivos y objetos de respuesta. Sustenta la separación entre hilo y presupuesto y muestra costes de refinamiento temporal. Nuestro V0 usa ventanas fijas, no afirma la cota deslizante de un servidor esporádico. Uso: recursos e IPC, ADR-004.

**S03 — seL4, Verification assumptions.** [Supuestos de verificación](https://sel4.systems/Verification/assumptions.html). Se revisaron límites de hardware, arranque, código y configuraciones. La lección es formular el alcance de una prueba y su TCB. Ninguna prueba de seL4 se transfiere a un kernel nuevo escrito en Rust. Uso: invariantes y evidencia.

**S04 — Engler, Kaashoek y O'Toole, 1995.** [Exokernel: An Operating System Architecture for Application-Level Resource Management](https://pdos.csail.mit.edu/6.828/2008/readings/engler95exokernel.pdf). Diseño de bindings seguros y separación de protección/gestión. Sirve para cuestionar políticas en el kernel. Sus resultados históricos no predicen nuestros costes; delegar toda gestión a libOS complicaría revocación y recursos comunes. Uso: frontera, alternativas.

**S05 — Banga, Druschel y Mogul, 1999.** [Resource Containers: A New Facility for Resource Management in Server Systems](https://www.usenix.org/legacy/events/osdi99/full_papers/banga/banga.pdf). Se estudió el desajuste entre procesos y peticiones servidas de forma compartida. Fundamenta un principal de consumo independiente del dominio. No resuelve por sí solo identidad semántica, autoridad o memoria compartida. Uso: ámbitos y recursos.

**S06 — Watson et al., 2010.** [Capsicum: practical capabilities for UNIX](https://www.cl.cam.ac.uk/research/security/capsicum/papers/2010usenix-security-capsicum-website.pdf). Diseño de confinamiento con descriptores y derechos en un sistema UNIX real. Es evidencia contra la falsa equivalencia «capacidades requieren microkernel» y a favor de restringir autoridad ambiental. Uso: Linux y rival monolítico.

**S07 — Fuchsia, Zircon handles.** [Documentación del proyecto](https://fuchsia.dev/fuchsia-src/concepts/kernel/handles). Explica referencias locales, derechos y transporte. Apoya separar entero visible, objeto y autoridad, sin tratar un handle como credencial global. No determina nuestra revocación ni nuestros ámbitos. Uso: objetos y ABI.

**S08 — CHERIABI, ASPLOS 2019.** [CHERIABI: Enforcing Valid Pointer Provenance and Minimizing Pointer Privilege in the POSIX C Run-time Environment](https://www.cl.cam.ac.uk/research/security/ctsrd/pdfs/201904-asplos-cheriabi.pdf). Se consultó el diseño de capacidades de memoria y su relación con un runtime existente. Demuestra una ruta de protección fina, pero pointer provenance no equivale a autorización de una tarea o publicación. Uso: rival de hardware; no dependencia inicial.

**S09 — Aviram et al., 2010.** [Efficient System-Enforced Deterministic Parallelism](https://www.usenix.org/legacy/events/osdi10/tech/full_papers/Aviram.pdf). Determinator usa restricciones estructurales y comunicación controlada para determinismo. Es una alternativa concreta a registrar todo. Sus supuestos restringen memoria compartida y workloads; no justifican prometer replay nativo general de Thalyx. Uso: determinismo, ADR-007.

**S10 — Narayanan et al., 2020.** [RedLeaf: Isolation and Communication in a Safe Operating System](https://www.usenix.org/system/files/osdi20-narayanan_vikram.pdf). Se revisó aislamiento mediante lenguaje, dominios y referencias controladas. Una frontera basada en safe Rust puede ser útil en un entorno restringido; no cubre automáticamente herramientas nativas arbitrarias del consumidor actual. Uso: elección de aislamiento y lenguaje.

**S11 — Baumann et al., 2009.** [The Multikernel: A new OS architecture for scalable multicore systems](https://barrelfish.org/publications/barrelfish_sosp09.pdf). Barrelfish trata coordinación entre núcleos como comunicación explícita. Aporta disciplina sobre replicación y propiedad; no demuestra que replicar desde el primer arranque reduzca complejidad en una máquina coherente pequeña. Uso: SMP y alternativa multinúcleo.

**S12 — Sewell et al., 2010.** [x86-TSO: A Rigorous and Usable Programmer's Model for x86 Multiprocessors](https://www.cl.cam.ac.uk/~pes20/weakmemory/cacm.pdf). Se revisó el modelo y sus exclusiones. Obliga a razonar sobre orden, no solo interleavings secuenciales. No cubre por sí solo TLB, MMIO, DMA ni reglas de alias del compilador Rust. Uso: concurrencia.

**S13 — Saltzer, Reed y Clark, 1984.** [End-to-End Arguments in System Design](https://web.mit.edu/Saltzer/www/publications/endtoend/endtoend.pdf). Se usa el argumento de que ciertas propiedades requieren conocimiento en extremos para delimitar verdad semántica y publicación. No se invoca como una regla absoluta contra cualquier soporte del kernel. Uso: frontera kernel/servicios.

## Sistemas reales, recursos y almacenamiento

**S14 — Linux, cgroup v2.** [Guía del kernel](https://docs.kernel.org/admin-guide/cgroup-v2.html). Se consultaron jerarquía, CPU, memoria y ownership de cargos. Sirve para evaluar el backend existente sin confundir migración de procesos con traslado automático de toda memoria. Uso: EVD-003 y comparación.

**S15 — Linux, seccomp BPF.** [Userspace API](https://docs.kernel.org/userspace-api/seccomp_filter.html). Se revisó alcance del filtro y sus límites como componente de sandbox. Un filtro de syscalls no constituye por sí solo toda una política de objetos. Uso: lectura del aislamiento de Thalyx.

**S16 — Linux, Landlock.** [Documentación de usuario](https://www.kernel.org/doc/html/latest/userspace-api/landlock.html). Muestra evolución y negociación de restricciones por ABI en Linux. Se considera una alternativa real de fortalecimiento del backend, condicionada a soporte y cobertura requeridos; un fallback best-effort no satisface un perfil obligatorio. Uso: alternativas y perfiles.

**S17 — Linux, VFIO.** [Driver API](https://docs.kernel.org/driver-api/vfio.html). Se revisaron grupos IOMMU y asignación de dispositivos. La unidad de aislamiento depende de topología y hardware. Mover el driver a usuario no basta cuando DMA sigue irrestricto. Uso: memoria, hardware y TCB.

**S18 — Linux, SCHED_DEADLINE.** [Documentación del planificador](https://docs.kernel.org/scheduler/sched-deadline.html). Se contrastaron reserva/admisión de ancho de banda y planificación EDF/CBS con simples cuotas. Evita llamar garantía de deadline a un techo de ejecución. Uso: planificador V0 y pregunta abierta temporal.

**S19 — SQLite, Atomic Commit.** [Diseño original mantenido](https://sqlite.org/atomiccommit.html). Se estudiaron orden de escrituras, flush, fallos y supuestos del medio. Aporta disciplina para separar atomicidad visible y durabilidad. No se deriva de aquí que cualquier rename o JSONL sea una transacción completa. Uso: protocolo de publicación.

**S20 — SQLite, Write-Ahead Logging.** [Documentación WAL](https://sqlite.org/wal.html). Se revisaron escritor único, lectores, recuperación y límites de transacciones entre bases. Es evidencia para mantener una fuente de verdad de publicación y tratar índices derivados aparte. No se propone que SQLite deba vivir en el kernel. Uso: persistencia.

## Distribución y evidencia

**S21 — Ongaro y Ousterhout, 2014.** [In Search of an Understandable Consensus Algorithm](https://raft.github.io/raft.pdf). Se consultó replicación y la interacción con clientes que reintentan. Ordenar un log no elimina la necesidad de identidades y respuestas deduplicadas. No se adopta consenso dentro del kernel local. Uso: reintentos y frontera distribuida.

**S22 — Lamport, 1978.** [Time, Clocks, and the Ordering of Events in a Distributed System](https://lamport.azurewebsites.net/pubs/time-clocks.pdf). Se revisó orden parcial de eventos y relojes lógicos. Sustenta distinguir tiempo observado y causalidad, sin afirmar que una secuencia global de logs explique intención. Uso: observabilidad y distribución.

**S23 — CamFlow, documentación del proyecto.** [Practical whole-system provenance for Linux](https://camflow.org/). Se consultaron arquitectura de captura, versionado de entidades y entrega a usuario. Es evidencia de que Linux puede instrumentar provenance del sistema. La cobertura/configuración y significado de eventos limitan lo que se demuestra. No se atribuyen a esta lectura resultados de un paper no reproducido. Uso: observabilidad y rival Linux.

## Especificaciones de implementación

**S24 — Rust, target bare-metal x86_64.** [rustc book](https://doc.rust-lang.org/rustc/platform-support/x86_64-unknown-none.html). Se consultaron soporte, ausencia de std y opciones de código para kernel. Sustenta separar target de kernel y usuario Linux; no prueba seguridad de un kernel Rust.

**S25 — Rust, UEFI targets.** [rustc book](https://doc.rust-lang.org/rustc/platform-support/unknown-uefi.html). Se revisaron target y calling convention de firmware. Loader y kernel son artefactos diferentes; el ABI de firmware no se hereda como ABI de syscall.

**S26 — UEFI Specification 2.11, Boot Services.** [Especificación oficial](https://uefi.org/specs/UEFI/2.11/07_Services_Boot_Services.html). Consulta centrada en mapa de memoria y fin de servicios de arranque. Se requiere respetar clave/vida de memoria, sin presentar la lectura de esa sección como una auditoría completa de firmware.

**S27 — OASIS, Virtio 1.2.** [Especificación oficial](https://docs.oasis-open.org/virtio/virtio/v1.2/virtio-v1.2.html). Referencia para negociación, colas, memoria y bloque/flush. Al implementar se fijarán las secciones y features exactas usadas; anunciar un dispositivo virtio no prueba que el stack cumpla durabilidad.

**S28 — Linux man-pages, openat2(2).** [Manual](https://man7.org/linux/man-pages/man2/openat2.2.html). Se consultó resolución relativa y restricciones de paths. Justifica reconocer mecanismos existentes de Linux contra carreras de nombres. No revoca por sí mismo un descriptor entregado.

**S29 — Linux man-pages, mmap(2).** [Manual](https://man7.org/linux/man-pages/man2/mmap.2.html). Se revisó memoria compartida y persistencia de mapas respecto del descriptor. Apoya distinguir autorización de apertura y accesos posteriores por memoria. Uso: límites de E06/E14 y sellado.

**S30 — Linux, memory barriers.** [Documentación del kernel](https://docs.kernel.org/core-api/wrappers/memory-barriers.html). Se consultaron relaciones de orden y dispositivos. Fuente práctica para no intercambiar barreras de compilador, CPU e I/O. No sustituye el manual de una plataforma concreta.

**S31 — Intel, Software Developer's Manuals.** [Índice oficial de manuales](https://www.intel.com/content/www/us/en/developer/articles/technical/intel-sdm.html). Se consultó el índice como entrada normativa para paginación, excepciones y ejecución. **No se afirma una lectura completa de los volúmenes.** K1/K3 deberán fijar revisiones y secciones efectivamente implementadas, incluyendo documentación AMD cuando se pruebe hardware AMD.

**S32 — NIST, FIPS 180-4 (2015).** [Secure Hash Standard](https://csrc.nist.gov/pubs/fips/180-4/upd1/final). Referencia del algoritmo SHA-256 escogido para identidad de contenido. La biblioteca y los fixtures se fijarán en K4; elegir un algoritmo especificado no verifica su implementación ni proporciona autenticación de autor.

## Balance y límites de la investigación

Se compararon protección por hardware y por lenguaje, capacidades en microkernel y UNIX, políticas en kernel y exokernel, recursos por proceso y petición, persistencia explícita, captura causal, determinismo restringido y distribución por servicios. No se extrapolan benchmarks históricos a una máquina actual.

No se considera probado que el diseño nuevo sea más rápido, menor, formalmente seguro o más útil que Linux. Tampoco se considera cerrada una decisión porque varias fuentes usen la misma abstracción. Las decisiones siguientes explican qué necesidad local justifica su adopción y qué resultado motivaría revisarla.
