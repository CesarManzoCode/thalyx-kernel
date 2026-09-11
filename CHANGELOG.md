# Historial del proyecto

## 0.7.0 — 2026-09-11

K6 — Comparación, endurecimiento y primera referencia. Primera fase que mide en lugar de solo demostrar que un mecanismo existe.

- Diecisiete benchmarks emparejados de una sola fuente (`tests/k6/bench.c`) sobre dos backends: este kernel, con `plat_native.c`, un supervisor propio y el motor de K5; y Linux como invitado de la misma máquina, con `plat_linux.c` y el motor de Thalyx sin cambios. El esquema (`abi/schema/k6-bench-v1.json`) declara antes de medir qué puede concluir cada comparación: equivalente, comparable, distinto o solo nativo.
- Campaña de referencia: seis rondas, tres brazos (`native`, `linux`, `linux-nomitig`), dieciocho arranques completos, 516 entradas sin muestra perdida ni operación rechazada, sobre KVM en la máquina de desarrollo. `tools/inventory_host.py` registra esa máquina sin arrancar el kernel sobre ella fuera de una VM.
- Ocho cuellos encontrados midiendo y corregidos con la cifra en la mano, documentados en [ADR-010](vault/decisions/ADR-010-k6-parameters-and-wake-policy.md): el plano de diagnóstico por operación, las ranuras de dominio/ámbito nunca recicladas, el presupuesto de raíz de una sola ventana, el despertar sin interrupción, el log de control de 64 celdas, el cerrojo sin turno, la reserva de tablas por mapeo nunca devuelta, y las notas/presupuesto del motor K5.
- Resultado principal: la inferencia del mismo modelo con la misma respuesta byte a byte cuesta un 10 % más aquí que en Linux; una llamada IPC con recibo y objeto de respuesta cuesta 0,41 de un socket con credenciales; el cómputo escala igual en cuatro procesadores en los dos kernels y **el IPC no escala en el nativo** — el cerrojo único de la máquina, medido y no partido en esta fase ([OQ-04](vault/roadmap/open-questions.md)).
- Puerta K6: `tools/check_k6.py` decide 19 criterios desde los artefactos de la campaña más las cinco regresiones K1–K5, con autocomprobación de 27 daños. PASS sobre la campaña de referencia. [Detalle](vault/evidence/k6-comparison-hardening.md).

K0–K6 quedan completos, cada uno en su alcance declarado, sobre el mismo binario de kernel. No hay fase K7 definida.

## 0.6.0 — 2026-09-11

K5 — Port de Thalyx. Primera ejecución de la semántica de Thalyx sobre este kernel con componentes reales.

- Segundo target de usuario, `x86_64-thalyx`, con una libc escrita en este repositorio (`user/native`) y sin dependencia de glibc.
- Cuatro etapas ejecutadas sobre la misma imagen: `smoke` (el target y su runtime), `surface` (la semántica de Thalyx sobre el estado administrado de K4), `work` (QuickJS real, quickjs-ng 0.15.1, validado por una herramienta nativa real, `user/ncheck`), y `engine` (llama.cpp real en `b10665` como motor residente, con completaciones byte a byte idénticas a la referencia de Thalyx en Linux).
- La matriz EXP-10: cinco casos —rivales, cancelación y los tres cortes de K4— ejecutados sobre la etapa `engine`.
- Puerta K5: `tools/check_k5.py` decide 47 criterios, cuatro de ellos las regresiones K1–K4, con autocomprobación de 88 daños. PASS. [Detalle](vault/evidence/k5-thalyx-port.md).
- Diecinueve defectos que compilar no encuentra, corregidos; dos de ellos en el propio kernel (`DOMAIN_ADD_THREAD` y la terminación de un hilo corriendo en otro procesador).

K5 no ejecuta el binario de Thalyx: no hay compilador de Rust ni comprobación de tipos dentro de un dominio.

## 0.5.0 — 2026-09-10

K4 — Estado durable administrado. Primera ejecución en la que algo sobrevive a la ejecución que lo escribió.

- Formato del almacén fijado antes de escribir nada: `abi/schema/k4-store-v1.json` es la única fuente, con `tools/gen_k4_format.py` derivando el módulo Rust del invitado y el módulo Python de la puerta, y 22 vectores dorados que ninguno de los dos define a mano.
- `user/k4store` publica versiones inmutables con CAS sobre un medio virtio-blk real; `user/k4disk` añade un motor de fallos de escritura al driver de K3.
- `tools/run_k4_cases.py` ejecuta quince casos: un baseline, un corte en cada punto donde una publicación puede cortarse, los cuatro modos de escritura fallida, publicadores en competencia y un plano de control perdido.
- Puerta K4: `tools/check_k4.py` decide 31 criterios desde los registros del kernel y los bytes del medio, con autocomprobación de 48 daños. PASS. [Detalle](vault/evidence/k4-durable-state.md).
- Nueve defectos que compilar no encuentra, corregidos.

K4 demuestra que el almacén sobrevive a que el escritor desaparezca en cualquier punto que el driver del invitado puede provocar; no demuestra durabilidad frente a un corte de energía real.

## 0.4.0 — 2026-09-09

K3 — SMP y dispositivos. Primera ejecución del kernel en más de un procesador y con un dispositivo que escribe memoria por su cuenta.

- Arranque de cuatro procesadores (`acpi.rs`, trampolín `ap.rs`, `percpu.rs`, `lapic.rs` con backends xAPIC/x2APIC), planificación bajo un solo cerrojo con reserva de saldo y simultaneidad, invalidación de TLB entre núcleos con reclamación diferida.
- Camino de dispositivo completo: `pci.rs` por ECAM, un driver virtio-blk moderno conducido desde usuario (`k3driver`) con validación del anillo de usados. Perfil de DMA declarado `WEAK_TRUSTED_DRIVER`; el perfil fuerte se rechaza con `UNSUPPORTED_PROFILE`, incluida una unidad DMAR descrita pero no programada ([ADR-009](vault/decisions/ADR-009-device-path-and-dma-profiles.md)).
- Interfaz V0 ampliada a 59 operaciones: un noveno tipo de objeto (`DEVICE`), cuatro derechos nuevos, ocho operaciones de dispositivo.
- Puerta K3: `tools/check_k3.py` decide 28 criterios, con autocomprobación de 57 daños. PASS. [Detalle](vault/evidence/k3-smp-devices.md).
- Seis defectos que compilar no encuentra, tres de ellos fatales y de la misma familia: suponer que el estado de un hilo basta para decir de quién es.

No existe aislamiento de DMA: no hay unidad de remapeo programada, y ninguna nota lo afirma.

## 0.3.1 — 2026-09-09

K2 cerrado. La interfaz entera ejercida y exigida por la puerta.

- Cobertura real de 42 a **51 de 51** operaciones asignadas, sin ninguna nombrada como no tocada. Cuatro pasos nuevos del supervisor la cierran: lectura y reconocimiento de recibos por capacidad, barrera sobre una concesión en lugar de sobre un ámbito, un dominio que se construye y se termina sin activarse nunca, y techos de ámbito que bajan después de crearlo. `SIGNAL_QUERY` se ejerce a ambos lados de la espera del timer.
- El log de control se lee por fin desde un programa: 58 recibos recorridos en lotes acotados, secuencias estrictamente crecientes, y el recibo que el guion escribió declarando un origen falso encontrado con el origen real estampado encima. Leer y reconocer son derechos separables y se comprueba que lo son.
- Criterio de cobertura en la puerta, que compara el recuento contra la lista de operaciones sin tocar en lugar de fiarse de uno de los dos: 21 criterios y 28 mutaciones de autocomprobación, tres de ellas de cobertura.
- Controles negativos de 27 a **41**, en 13 estados distintos, ninguno inesperado y ninguno ausente.
- Tres defectos más que solo aparecieron al ejecutar estos caminos. Las tres operaciones que informan sobre un linaje o lo limpian —`CAP_INSPECT`, `CAP_CLOSE`, `CAP_DRAIN_STATUS`— estaban detrás de la misma puerta de resolución que rechaza el linaje cercado o vencido del que existen para hablar: medido desactivando la corrección, `CAP_DRAIN_STATUS` no se alcanza nunca. `DOMAIN_ADD_THREAD` informaba de un conflicto de estado como límite agotado y consultaba el espacio de direcciones antes que el estado. `CapInfo.lineage_state` no tenía valores con nombre en el esquema; `CapLineage` entra en él.
- Evidencia y estado actualizados con lo medido y con sus límites explícitos, incluidos los que el número de cobertura no cubre: un camino por operación, el contador marcado antes de comprobar derechos, y el hilo del dominio de repuesto que nunca se ejecuta.

K2 queda completo. El siguiente paquete es K3 y no hay trabajo K2 pendiente.

## 0.3.0 — 2026-09-08

K2 — Objetos, autoridad y trabajo. Primer sistema de capacidades ejecutándose.

- Interfaz V0 generada desde un esquema único: bindings Rust y C, fixtures y comprobador que verifica que lo generado coincide byte a byte con el esquema. Cinco entradas de kernel, incluida la lectura del reloj monotónico sin la cual los plazos de la interfaz no podían expresarse.
- Tabla generacional de objetos y capacidades, ámbitos con límites y ventana de CPU, objetos de memoria con sellado, IPC copiado con invocaciones que llevan origen y cargo, señales, timers, log de control con celdas reservadas y copia de usuario acotada.
- Un único punto de admisión: estructura, autoridad y efecto en ese orden, con autoridad y efecto bajo el mismo cerrojo que toma la barrera. Las 51 operaciones del esquema se despachan desde ahí.
- Arranque K2 seleccionado por el paquete: un módulo `SUPERVISOR` construye la raíz, el log y un solo dominio con manifiesto explícito de capacidades; su ausencia mantiene los dominios de K1.
- Primera vertical: supervisor, servidor y cliente sin permisos ambientales. El cliente estrecha autoridad sobre su propio buffer y la delega; el servidor admite un efecto y retiene la obligación; el supervisor cierra el ámbito del cliente mientras eso ocurre; la autoridad delegada muere con la barrera y la obligación sobrevive a ella hasta ser resuelta.
- Veintisiete controles negativos ejecutados dentro de la vertical, incluidas once peticiones malformadas sin efecto parcial y los límites de tabla, cola y log con el cierre todavía disponible.
- Trabajo atribuible: un servidor se vincula al ticket y adopta el ámbito efectivo de la invocación; después de la barrera la vinculación pasa a recuperación y se carga a la reserva de cierre del servicio, no a la del cliente cerrado.
- Presupuesto agregado observable: el planificador retiene hilos cuyo ámbito gastó su ventana, cada ventana cerrada por encima del presupuesto deja constancia del exceso que arrastra, y lo que gasta un hijo cuenta contra sus ancestros.
- Publicación conservadora de memoria: copia entre objetos, mapeo, sello que retira el escritor antes de prometer inmutabilidad, y remapeo de solo lectura para el lector. W^X y sello se comprueban rechazando los mapeos que los violan.
- Comprobador de la puerta K2: 20 criterios decididos por separado, la mayoría desde los registros del kernel, con la regresión de K1 como criterio propio y una autocomprobación de 25 ejecuciones dañadas.
- Registro de lo ejecutado, su alcance y sus límites en el vault, incluidos los diez defectos que solo aparecieron al ejecutar.

EXP-01 queda ejecutado; EXP-02, EXP-03, EXP-04 y EXP-06 quedan ejecutados en su alcance K2, que es uniprocesador, sin dispositivos y sin estado durable. No se incorporan SMP, drivers, DMA, estado durable ni resultados de rendimiento.

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
