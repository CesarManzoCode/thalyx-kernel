---
id: RES-003
kind: research
status: observed
---
# K8: dónde pierde K1 contra L1 en una transacción y cómo lo resolvieron otros sistemas

Consulta e inspección del **2026-10-30**. Este documento orienta K8. Separa cuatro cosas: lo **medido** en este árbol, lo que dicen las **fuentes**, las **hipótesis** que K8 debe confirmar o descartar con una medida, y las **reglas de medición** que siguen de ambas cosas. Ninguna fuente externa se reprodujo aquí. «Consultado» no quiere decir validado en esta máquina.

## 1. Lo medido en este árbol

Fuente: la última medida de K7, `build/k7/check-20261029T182638` (kernel `2eca0c8`, Thalyx `3ac575f`, KVM, imagen de medida, 6 rondas × 16 casos). La copia que fija el cierre de K7 está en `vault/private/evidence/k7-final/`.

**El signo.** `dev/exp13/analyze.py` de Thalyx calcula `K1 − L1` como la mediana de las diferencias pareadas por ronda y caso. Su resumen dice: «A negative K1−L1 means the kernel arm was faster». Positivo quiere decir que K1 es más lento.

**El total ya no es +26 ms.** El baseline de K7 (`1eacb11`) medía +26,2 ms y +27,2 ms en sus dos réplicas. Después de K7, el agregado es **+4,447 ms [+4,322, +4,932]**. Por caso, K1 tarda entre 7,4 y 31 ms y L1 entre 3,1 y 26,7 ms. K1 es unas dos veces más lento en los casos cortos.

**Dónde está la diferencia.** Las medianas por caso, sumando las fases de cada transacción:

| Grupo de casos | `open` K1 / L1 (ms) | `settle` K1 / L1 (ms) | Publicaciones K4 | Llamadas a K4 |
|---|---|---|---|---|
| 10 casos en los que `settle` no publica | 6,2–6,5 / 1,9–2,1 | 0,5–0,8 / 0,5–0,7 | 2 | 36–41 |
| 5 casos en los que `settle` publica | 6,3–6,9 / 1,8–2,6 | 3,3–4,0 / 0,9–1,2 | 3 | 50–62 |
| `two-transactions-in-a-row` (dos trabajos) | 8,6 / 2,4 | 6,8 / 1,9 | 5 | 83 |

En cada caso, `open` cuesta en K1 unos +4,4 ms más que en L1. Cada publicación de más en `settle` suma en K1 entre +2,4 y +2,8 ms, y en L1 unos +0,2 ms. `end`, `request`, `validate` y `observe` difieren en centésimas de milisegundo. Las tres últimas corren en el anfitrión sin llamar al link, así que esa diferencia es ruido del anfitrión y no hay nada que optimizar ahí. Las medianas por fase no se suman al total porque cada una es la mediana de su propia distribución.

**Dónde vive cada medio.** Observado en `crates/thalyx-cli/tests/exp13_equivalence.rs` de Thalyx `3ac575f`:

- El medio de K4 del brazo K1 es `medium.img` dentro de `tempfile::Builder::new().prefix("k1med").tempdir_in("/tmp")`.
- QEMU lo abre con `cache=writeback` (`tools/run_k5.py`).
- `/tmp` es `tmpfs` en esta máquina (`findmnt /tmp`). El almacén de `linux-managed` (L1) también es un `tempfile::tempdir()` en `/tmp`.
- L1 hace `sync_data` por cada registro del log y `sync_all` por cada objeto y su directorio (`crates/thalyx-managed/src/lib.rs`). Sobre `tmpfs`, ese `fsync` no llega a ningún disco.
- Solo L0 usa la arena btrfs de `/mnt/exp13`, y `K1 − L1` no la necesita. Se comprobó: el análisis de la misma medida sin los directorios de L0 da el mismo `K1 − L1` (+4,447 ms) con `arms_present` = `['L1', 'K1']`.

**Consecuencia.** La hipótesis de que la diferencia sale del disco del anfitrión no se sostiene. Ni L1 ni K1 pagan un flush real del anfitrión. Lo que paga K1 está en su camino propio: Thalyx → socket UNIX → `virtio-serial` → `k5link` → IPC a `k4store` → IPC a `k4disk` → `virtio-blk` → QEMU, y la vuelta por el mismo camino.

**Cuánto escribe K4 por publicación.** Observado en `user/k4store/src/state.rs`: `prepare` va seguido de `flush`, los objetos van seguidos de `flush`, `commit` va seguido de `flush` y el outbox de su propio `flush`. Son tres o cuatro flushes por publicación, cada uno con su ida y vuelta por `virtio-blk`. `k4disk` espera cada completación con `signal_wait` sobre su interrupción.

**El anfitrión, medido el 2026-10-30.** Gobernador `powersave` con `amd-pstate` en modo `active` y EPP `balance_performance`. Boost encendido. `kvm.halt_poll_ns` = 200000. Sin `isolcpus` ni `nohz_full` en `/proc/cmdline`. 12 hilos en 6 núcleos con SMT. `/mnt/exp13` no está montado.

**Lo que no se midió.** Cuántos mensajes de `virtio-serial`, peticiones de `virtio-blk` por tipo, interrupciones y despertares hay por transacción, ni cuánto tarda cada uno. El `stats` del link solo da `store_calls`, `publications`, `reads_cached`, `stages_reused`, `binds_reused`, `tx_waits`, `works_*` y `compactions`. Thalyx pasa ese JSON tal cual a `observation.kernel`, sin nombrar los campos. Un contador nuevo en el link aparece en la medida sin tocar Thalyx.

## 2. Lo que dicen las fuentes

### Commits durables: cuántos flushes y cómo se agrupan

- **PostgreSQL, group commit.** «`commit_delay` … defines for how many microseconds a group commit leader process will sleep after acquiring a lock within `XLogFlush`, while group commit followers queue up behind the leader … so that all of them will be flushed by the leader's eventual sync operation.» Con `commit_delay = 0` el grupo lo forman las sesiones que llegan mientras el flush anterior sigue en curso. La propia documentación advierte que una espera demasiado larga «can increase transaction latency». [PostgreSQL, WAL Configuration](https://www.postgresql.org/docs/current/wal-configuration.html). La lección para una transacción sola es que el commit es un único `XLogFlush`. Agrupar solo sirve con concurrencia.
- **SQLite WAL.** «Writers sync the WAL on every transaction commit if PRAGMA synchronous is set to FULL but omit this sync if … NORMAL». WAL «uses many fewer fsync() operations» que el journal de rollback y escribe el contenido «once (versus twice for rollback-journal transactions)». El costo del checkpoint lo paga, de vez en cuando, el commit que hace crecer el WAL más allá de su límite. [SQLite, Write-Ahead Logging](https://www.sqlite.org/wal.html).
- **ext4/jbd2.** Una transacción de jbd2 agrupa muchas actualizaciones y termina en un bloque de commit. Si las barreras están activas y no hay `async_commit`, el bloque de commit sale con `REQ_PREFLUSH | REQ_FUA`: un flush previo y escritura forzada, todo en una sola petición. Con `async_commit`, el commit sale sin barrera junto con los datos, la suma de verificación de la transacción garantiza que esté completa y hay un solo `blkdev_issue_flush` al final. [`fs/jbd2/commit.c`](https://raw.githubusercontent.com/torvalds/linux/master/fs/jbd2/commit.c). Los fast commits guardan «the minimal delta needed to recreate the affected metadata» y «help reduce commit latency significantly» ([ext4 journal](https://www.kernel.org/doc/html/latest/filesystems/ext4/journal.html)). `max_batch_time` limita en 15 ms por defecto cuánto espera un commit para juntar más trabajo ([ext4 admin guide](https://www.kernel.org/doc/html/latest/admin-guide/ext4.html)).
- **Btrfs, tree-log.** El log tree es un log de escritura anticipada para que `fsync` no exija un commit completo de la transacción. Varios `fsync` concurrentes se unen a la misma transacción del log mediante `btrfs_log_ctx`, y algunos casos (renombres y desvinculaciones sin el inodo en el log) fuerzan el commit completo. [`fs/btrfs/tree-log.h`](https://codebrowser.dev/linux/linux/fs/btrfs/tree-log.h.html) y los comentarios de `tree-log.c`.
- **Bloque de Linux: flush y FUA.** `REQ_PREFLUSH` garantiza que «previously completed write requests are on non-volatile storage before the flagged bio starts». Con `REQ_FUA`, «I/O completion for this request is only signaled after the data has been committed». Si el dispositivo no anuncia FUA, la capa de bloque emula FUA enviando un flush después de la escritura. [writeback_cache_control](https://www.kernel.org/doc/html/latest/block/writeback_cache_control.html).
- **virtio-blk.** No tiene FUA. Una escritura es estable cuando se completa un `VIRTIO_BLK_T_FLUSH` enviado después de ella (5.2.6.2). El dispositivo atiende las peticiones «probably out of order» (5.2). `VIRTIO_F_EVENT_IDX` y los flags de la cola permiten suprimir notificaciones, y `VIRTIO_F_IN_ORDER` permite marcar un lote completo con una sola entrada usada (2.7). [virtio 1.2](https://docs.oasis-open.org/virtio/virtio/v1.2/csd01/virtio-v1.2-csd01.html).
- **QEMU.** `cache=writeback` (el valor por defecto), `none`, `writethrough` y `directsync` respetan el flush del invitado. Solo `unsafe` activa `cache.no-flush`. `aio=` puede ser `threads`, `native` o `io_uring` (`man qemu`, sección `-drive`, QEMU 11.1.1 instalado aquí).
- **Orden sin durabilidad.** OptFS separa el orden de la durabilidad: `osync()` ordena y `dsync()` además hace durable. Usa sumas de verificación para no tener que ordenar las escrituras del commit, y en algunas cargas gana un orden de magnitud ([Chidambaram et al., SOSP 2013](https://research.cs.wisc.edu/wind/Publications/optfs-sosp13.html)). El IO stack con barreras de Won et al. reemplaza el flush por una barrera de orden donde el dispositivo la ofrece ([FAST 2018](https://www.usenix.org/conference/fast18/presentation/won)). virtio-blk no ofrece barreras: el `VIRTIO_BLK_T_BARRIER` heredado «does not flush caches in the underlying backend». En este árbol, el orden se consigue con sumas de verificación, no con barreras de dispositivo.

### Microkernels y sistemas con capacidades

- **seL4, sDDF** ([diseño 0.7, 2026-08-19](https://trustworthy.systems/projects/drivers/sddf-design.pdf)). Los datos pasan por colas de un productor y un consumidor en memoria compartida. Cada cola lleva un campo `consumer_signalled`, y un productor que ve al consumidor despierto no le manda señal. El documento cuenta 15 entradas al kernel por ida y vuelta de un paquete en su modelo, contra 4 en Linux, y lo llama «the inherent overhead of the microkernel-based design». Lo compensan con lotes: el driver procesa todo lo pendiente antes de volver a esperar. Con eso su sistema usa menos CPU que Linux en toda la carga medida («At all load levels it uses far less CPU than Linux»), y la ventaja se achica con la carga «due to batching». El subsistema de almacenamiento usa colas Rq/Rs con peticiones de barrera en la cola: «All requests before the barrier must be completed … before any of the requests after the barrier are initiated».
- **Fuchsia.** El protocolo FIFO de bloque agrupa peticiones en una transacción con `BLOCK_IO_FLAG_GROUP_ITEM` y `GROUP_LAST`, y el servidor responde una vez por grupo. Además define `BLOCK_IO_FLAG_PRE_BARRIER` y `FORCE_ACCESS`, y el flush es un código de operación ([`block-fifo.h`](https://fuchsia.googlesource.com/fuchsia/+/HEAD/src/storage/lib/block_protocol/block-fifo.h)). Fxfs usa un journal lógico para agrupar mutaciones atómicas sobre varios objetos y verifica sumas de los datos escritos desde el último flush conocido ([RFC-0136](https://fuchsia.dev/docs/contribute/governance/rfcs/0136_fxfs)). Un cambio de 2025 solo emite la barrera en una escritura del journal si hubo escrituras de datos desde la anterior ([roll de integración](https://fuchsia.googlesource.com/integration/+/879c5da8ca1e6370d238d1a9bd41882cd7686645)).
- **Redox.** RedoxFS duplicó su rendimiento «by reducing the number of context switch roundtrips per block read and write», con el mismo cambio en los drivers SATA, NVMe, USB SCSI y VirtIO ([This Month in Redox, junio de 2024](https://www.redox-os.org/news/this-month-240630/)). La fuente no da cifras absolutas.

El patrón común: en un sistema con servicios separados, el costo es el número de cruces (IPC, notificación, despertar) por operación lógica, no el trabajo de cada cruce. Los tres lo bajaron con lotes, con supresión de notificaciones redundantes y con menos idas y vueltas por operación.

### Medición reproducible en QEMU/KVM

- **Halt polling del anfitrión.** KVM hace sondeo antes de ceder la CPU de un vCPU parado: «Polling provides a latency advantage in cases where the guest can be run again very quickly». Es un compromiso entre consumo y latencia ([KVM halt polling](https://www.kernel.org/doc/html/latest/virt/kvm/halt-polling.html)). Aquí está en 200 µs.
- **Halt polling del invitado.** `cpuidle-haltpoll` hace que el vCPU sondee antes de `HLT`. Así se evita la salida a la VM, y un vCPU remoto puede saltarse la IPI de despertar porque el bit POLL está puesto. El valor por defecto es 200 µs ([guest halt polling](https://www.kernel.org/doc/html/latest/virt/guest-halt-polling.html)). Es el mismo mecanismo que puede usar un kernel invitado propio.
- **Frecuencia.** En modo `active`, `amd-pstate` deja la frecuencia al firmware CPPC con un EPP como pista. Cambiar el EPP o el gobernador pide escribir en sysfs como root ([amd-pstate](https://www.kernel.org/doc/html/latest/admin-guide/pm/amd-pstate.html)).
- **Aislamiento.** `isolcpus=domain` es irreversible y la documentación recomienda cpusets para hacerlo en caliente. `nohz` equivale a `nohz_full` ([kernel-parameters.txt](https://raw.githubusercontent.com/torvalds/linux/master/Documentation/admin-guide/kernel-parameters.txt)). La guía de LLVM pide reservar núcleos (`cset shield`), apagar el hermano SMT, usar el gobernador `performance`, apagar el turbo y «Run the benchmark multiple times to be able to recognize noise» ([LLVM Benchmarking](https://llvm.org/docs/Benchmarking.html)).

## 3. Hipótesis para K8, sin medir

1. **H1, el costo es por cruce y no por byte.** Un `open` de K1 cuesta unos 4,4 ms más que en L1, y cada publicación unos 2,6 ms más. Con unas 40 llamadas a K4 por caso y tres o cuatro flushes por publicación, eso da del orden de cientos de µs por petición a `virtio-blk` o por despertar, muy por encima de una IPC del kernel (`ipc.lineage` de K6 mide de 1,8 a 2,0 µs nativos por llamada). Si se confirma, lo que importa es cuántas idas y vueltas hay y cuánto tarda en despertar quien espera, no la copia de datos.
2. **H2, despertar a un vCPU parado.** Entre un mensaje y el siguiente, el vCPU de `k4disk` o de `k5link` ejecuta `HLT`. Con `powersave` y EPP `balance_performance` en el anfitrión, cada despertar paga la salida, el halt polling de 200 µs (o el sueño si se excede) y la subida de frecuencia. ADR-026 ya vio entradas lentas después de un vCPU ocioso, sin leer la frecuencia. El sondeo breve antes de `HLT` en el invitado (como `cpuidle-haltpoll`) y la afinidad de las interrupciones atacan esto.
3. **H3, commit en un solo flush.** Si los registros de K4 llevan suma de verificación y la recuperación descarta un commit cuyos registros no verifican, `prepare`, los objetos y `commit` pueden salir juntos, seguidos de un solo flush, como el `async_commit` de jbd2. El outbox puede ir en el mismo grupo o en el siguiente commit. Por publicación serían 1 flush en lugar de 3 o 4. La puerta K4, con escrituras retenidas hasta el flush y cortes en diez puntos, es la que dice si sigue siendo correcto.
4. **H4, lotes en las colas.** Varias peticiones por notificación a `virtio-blk`, una sola interrupción por lote (`EVENT_IDX`), y en `virtio-serial` menos mensajes por transacción. Es lo que hicieron sDDF y el FIFO de Fuchsia.
5. **H5, del lado del emulador.** Un `iothread` para `virtio-blk` y `virtio-serial` y `aio=io_uring` bajan la latencia de QEMU. Es configuración de la máquina del brazo K1, no del kernel. Si se usa, se declara, y nunca con `cache=unsafe` ni `cache.no-flush`.

Que alguna de estas hipótesis baste para que K1 le gane a L1 está **sin demostrar**. L1 no paga ninguna salida de VM. K1 paga al menos una ida y vuelta de `virtio-serial` por cada mensaje de Thalyx. Para ganar, K1 tiene que hacer su camino propio en menos tiempo que el `fsync` sobre `tmpfs` y el loopback de L1.

## 4. Cómo se mide K8, y por qué así

- **Contra L1 en la misma campaña.** K7 fijaba umbrales en el límite más favorable de dos réplicas del baseline con desigualdad estricta, y por eso criterios en el piso del ruido o de la cuantización (`tx_request`, `tx_validate`, `tx_observe` y `entry.version` en pasos de 10 ns) no podían pasar. K8 compara K1 con L1 dentro de la misma campaña, con rondas pareadas y orden aleatorio por ronda (lo que ya hace EXP-13), y el umbral es cero. La deriva lenta del anfitrión afecta a los dos brazos.
- **Solo métricas en milisegundos.** El criterio es la latencia extremo a extremo, no las fases de microsegundos que corren en el anfitrión.
- **Sin pedir root.** El gobernador, el EPP, el boost, `halt_poll_ns`, la carga y la línea de arranque del kernel se registran en cada medida y no se cambian. Cambiarlos entre rondas cambiaría el experimento. Pinning de vCPU no se hace, porque los procesos QEMU los lanza el arnés de Thalyx y fijarlos favorecería a un brazo.
- **Sin L0.** `/mnt/exp13` necesita `sudo mount`. K8 no lo usa salvo que esté montado como btrfs. `K1 − L1` no depende de él (comprobado arriba).
- **La medida explica.** Al final de su salida, el checker de K8 imprime las fases por brazo, las medianas por caso y cada contador numérico de `observation.kernel`. kiln entrega los últimos 4000 caracteres a la ronda siguiente. Así una ronda, que no puede medir por la regla del mini sprint, recibe con qué atribuir.
- **Los registros de una puerta que falla se conservan.** K7 perdió dos veces el registro de la ejecución que falló (ADR-027). La medida de K8 copia los directorios de la corrida antes de que la siguiente los sobrescriba.

## 5. Lo que K8 no puede afirmar

Todo es QEMU/KVM en una sola máquina, con los dos almacenes en `tmpfs`. Un resultado de K8 no dice nada sobre hardware real ni sobre un medio con flush real. En un medio así, L1 pagaría dos `fsync` por objeto y uno por registro, y K4 pagaría sus flushes por publicación. Esa comparación es otro experimento.
