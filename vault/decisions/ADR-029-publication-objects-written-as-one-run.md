---
id: ADR-029
kind: decision
status: accepted
---
# Los objetos de una publicación van al medio en una sola petición

**Problema.** La verificación de kiln de la ronda 1 de K8 (`build/k8/check-20261030T122541`, kernel `979363c`, Thalyx `3ac575f`, KVM, 6 rondas × 16 casos) mide `K1 − L1` en **+0,861 ms [+0,702, +0,972]**. Pasan `k8.same_experiment`, `k8.durability` y `k8.behavior`. Por fase, `open` da +0,689 ms y `candidate` −0,125 ms. Las demás fases difieren en centésimas.

**Evidencia.** Son medianas por caso de los 96 registros de K1 de esa medida (`observation.kernel`, los contadores de ADR-028):

- *El transporte ya no pierde.* En los diez casos en los que `settle` no publica, `settle` de K1 tarda entre 0,44 y 0,73 ms, y el de L1 entre 0,53 y 0,72 ms. K1 es más rápido en nueve de los diez. `candidate` también es más rápido en K1. Fuera de `PUBLISH`, el link pasa entre 0,53 y 0,79 ms dentro de `serve` en 31 a 36 tramas por caso. Son unos 17 a 24 µs por trama. Una ida y vuelta por `virtio-serial` no es lo que separa a K1 de L1.
- *Lo que queda es `PUBLISH`.* `publish_ns` es 1,48 a 1,87 ms en los casos con dos publicaciones del link y 2,14 a 2,79 ms en los que tienen tres. Cada publicación de más en `settle` cuesta 0,45 a 0,9 ms dentro de K4, y `settle` de K1 en esos casos es 0,59 a 0,95 ms más lento que el de L1. Las dos publicaciones de los casos cortos son la del `seed` (en `open`) y la de la evidencia. Thalyx guarda la evidencia después de `carry_out` (`crates/thalyx-cli/src/exec.rs`, `platform.evidence().record`), así que esa publicación queda fuera de `whole_ns`. Si la de la evidencia cuesta lo que una de `settle`, la del `seed` cuesta en K4 unos 1,1 a 1,3 ms. Esa es la parte de `open` que L1 no paga.
- *Una publicación es una cadena de idas y vueltas al disco.* `Store::publish` (`user/k4store/src/state.rs`) escribe `PREPARE` y hace flush, después escribe cada objeto que falta y el recibo y hace flush, y al final escribe `COMMIT` y hace flush. Cada registro es una petición a `k4disk` y una petición `virtio-blk` que espera su interrupción. Una publicación del link mete en K4, además de los archivos de Thalyx, el árbol `O`, el índice `X`, la historia `H`, la evidencia `E`, el árbol de cada línea, la raíz que hace `FREEZE` y el recibo de K4. Eso da del orden de 12 a 16 objetos en el `seed` y de 5 a 8 en una publicación de `settle`. Son entre 10 y 20 peticiones de 50 a 80 µs cada una. Con `--k4-io-notes off` ninguna nota serie queda en este camino.
- *Lo que no se midió.* Cuántas peticiones hace K4 por publicación ni cuánto tarda cada una. El `stats` del link no lo veía. La cifra de 50 a 80 µs sale de dividir y es una inferencia.

**Elección.**

1. *Una corrida de registros por petición* (`user/k4store/src/state.rs`). Entre el flush del `PREPARE` y el del grupo de objetos, cada registro de objeto, recibo incluido, se enmarca en una ranura propia del búfer compartido y se encola. El log avanza en memoria como antes, con la misma secuencia, el mismo `prev_digest` y el mismo bloque. La corrida se escribe con una sola petición `WRITE` de `count` bloques consecutivos (`Disk::write_run`) antes del flush. Si la corrida se llena (16 registros), se escribe y empieza otra. Si el medio rechaza la corrida, el log vuelve adonde empezó, como cuando `append` no avanza tras un rechazo. Los objetos quedan durables y liberan su ranura de staging solo después de que la petición vuelve bien.
2. *El arnés corta igual que antes.* `fault_due` dice, sin aplicarla, si la directiva toca en este objeto. Si toca, la corrida pendiente se escribe antes de que la directiva actúe, ya sea para tirar, romper, rechazar o retener la escritura, parar, matar el servicio o llenar el plano de control. El registro afectado sale solo, por `append`, con su modo. El dispositivo ve, antes de cada fallo inyectado, el mismo conjunto de escrituras que cuando cada registro era su propia petición. Solo cambia cuántas peticiones hacen falta para llevarlas.
3. *El búfer compartido pasa de 4 a 20 páginas* (`IOBUF_PAGES`, `user/k4fmt/src/pkg.rs`). Las ranuras 0 a 3 siguen como estaban: la de lectura (0), la de escritura (1), la 2, que nadie usa, y la del directivo de los supervisores (3). La corrida usa de la 4 a la 19 (`IOBUF_RUN_SLOT`). `k4disk` ya aceptaba escrituras de varios bloques dentro del búfer. Ni el protocolo de bloque ni el formato del medio cambian.
4. *Un contador más en `stats`.* El link responde `disk_writes`, que son las peticiones de escritura que `k4disk` emitió desde que el link quedó listo (el `STATS` de K4, mitad baja de `reserved0`). Se pregunta con un cliente aparte, así que no suma a `store_calls`.

**Argumento de recuperación.** No cambia ningún punto de flush. Entre dos flushes, `virtio-blk` puede quedarse con cualquier subconjunto de las escrituras completadas y en cualquier orden. Solo un `VIRTIO_BLK_T_FLUSH` enviado después de que se completan las vuelve estables (virtio 1.2, 5.2.6.2). Una escritura de varios bloques que se corta deja en el medio un subconjunto de sus bloques, o un bloque roto. Ese mismo estado lo puede dejar el medio con escrituras de un bloque sin flush, y la recuperación ya lo resuelve. Cada registro sigue verificándose por su digest y su enlace con el anterior, y una cadena que no enlaza termina el log. Un `COMMIT` cuyos objetos no están todos no se adopta. `COMMIT` sigue saliendo después del flush de los objetos, y la respuesta después del flush del `COMMIT`. El contrato de [persistencia](../architecture/persistence.md), en sus pasos 2 a 4, se cumple igual. La puerta K4, con sus cortes en cada punto y las escrituras que retiene el driver hasta el flush, es la que lo comprueba. Con `fault_due`, cada corte de `BEFORE_OBJECT` deja el mismo medio que antes.

**Autocomprobación.** El medio queda igual byte a byte, en los mismos bloques, con o sin la corrida. Ningún criterio de K4 ni de K5 lee `DISK_WRITE` ni cuenta escrituras por registro. `RECORD_WRITTEN` se sigue escribiendo una vez por registro, cuando la corrida vuelve. Ninguna puerta debería cambiar. Las cifras sí se pueden contradecir, y con la siguiente medida:

- `disk_writes` por caso debería ser unas 3 por publicación del link, más una por cada 16 objetos de la corrida: unas 6 en los diez casos sin publicación en `settle` y 9 en los cinco que publican una vez. Sin esto serían varias veces más.
- `publish_ns` por caso debería bajar a la mitad o menos.

**Diseñado y no medido.** Por la regla del mini sprint no se corrió ninguna puerta, prueba ni medida. Se compiló el workspace para `x86_64-unknown-none`, y K4 también con `large-index,quiet-io`. Lo que se espera, en cifras:

- `open`: −0,6 a −0,8 ms en los 16 casos, por el `seed`.
- `settle`: −0,25 a −0,4 ms más en cada publicación de `settle`. Son los cinco casos que publican y las dos publicaciones de `two-transactions-in-a-row`.
- Extremo a extremo: unos −0,7 ms en los diez casos, −1,0 ms en los cinco y −1,3 ms en `two-transactions-in-a-row`. `k8.tx_e2e` bajaría de +0,86 ms a algo entre −0,1 y +0,3 ms. Puede que el límite superior del IC **no** quede por debajo de cero.

**Alternativas descartadas.**

- *Juntar `PREPARE`, objetos y `COMMIT` bajo un solo flush, como el `async_commit` de jbd2 (H3 de RES-003):* ahorra otros dos flushes por publicación. Pero cambia qué estados pueden quedar en el medio después de cada corte, y con eso lo que esperan los casos `cut-after-prepare` y `cut-after-objects` de la puerta K4. Necesita su propio argumento y una ronda en la que la puerta lo juzgue sola.
- *`aio=io_uring` o un `iothread` para el disco del brazo K1 (H5):* es configuración de QEMU y no del kernel, y abarata cada petición sin cambiar cuántas hay. Queda para cuando `disk_writes` y `publish_ns` digan cuánto cuesta cada una.
- *Sondear antes de `HLT` en el invitado (H2):* ataca el despertar, y no se sabe cuánto pesa en cada petición.
- *Corridas solo cuando no hay arnés:* el camino que mide EXP-13 no sería el que cortan las puertas.

**Consecuencias.** Cada supervisor que arma K4 reserva 16 páginas más para el búfer compartido, y `k4disk` concede esas 16 páginas más al dispositivo. Una corrida de 16 bloques son 64 KiB en un solo descriptor. La compactación y los registros de `PREPARE`, `COMMIT`, `ABORT` y `OUTBOX` siguen saliendo de a uno.

**Revisión.** Si `disk_writes` no baja, la corrida no se está formando, y hay que mirarlo antes que nada. Si baja y `publish_ns` no baja en proporción, el costo no es el número de peticiones sino el flush o el despertar. En ese caso tocan H5 o H2. Si `publish_ns` baja y `tx_e2e` sigue arriba de cero, lo siguiente es H3 con su ADR, o quitar las llamadas de `commit_root` que no escriben (`FORK`, `WRITE`, `FREEZE`, `DISCARD`). Si una puerta K4 falla, la causa está en `build/gate-failures/` y se lee antes de cambiar nada.

**Referencias.** [ADR-028](ADR-028-link-notes-after-serving-and-kept-gate-failures.md), [RES-003](../research/k8-transactions.md), [Persistencia](../architecture/persistence.md), [estado actual](../roadmap/current-state.md).
