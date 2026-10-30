---
id: ADR-030
kind: decision
status: accepted
---
# Una llamada busca su respuesta después de registrar su espera, el medio de K1 se atiende en un hilo de E/S con io_uring, y `stats` cuenta el tiempo del disco

**Problema.** La verificación de kiln de la ronda 2 de K8 (`build/k8/check-20261030T150404`, kernel `7c3e7eb`, Thalyx `3ac575f`, KVM, 6 rondas × 16 casos) mide `K1 − L1` en **+0,045 ms [−0,046, +0,101]**. Pasan `k8.same_experiment`, `k8.durability` y `k8.behavior`. En la misma verificación, el `check_k4.py` de kiln falló `outbox` (30 de 31: `cut-after-checkpoint/1 holds outbox records with status ['UNKNOWN'] and none DELIVERED; the service recorded [3]`), y su corrida quedó en `build/gate-failures/20261030T213030-tcg/`. El `check_k6.py` falló `no_loss` (23 de 24, `native r5 bench 10:3 status=[-9]`).

**Evidencia.**

*La falla de `outbox` es una espera perdida del kernel.* Observado en `cut-after-checkpoint/serial-1.log` de la corrida conservada:

- A los 323,66 ms, `k4pub` (hilo 13) llama a `k4store` (invocación 156). La admisión queda en cola (`queued=1`), porque `k4store` está respondiendo la 154 del lector. `k4store` responde la 154 a los 324,03 ms, recibe la 156 a los 324,28 ms y la responde a los 325,00 ms (`ipc.replied invocation=156 ... result=0x0`).
- `k4pub` no vuelve a correr hasta los 8 323,5 ms, 8 s después de la llamada. Es exactamente `CALL_DEADLINE_NS` de `k4client`. En ese intervalo el procesador 3, donde corrió por última vez, tuvo 30 despachos y 60 rechazos de presupuesto en toda la corrida, y ningún aviso. Nadie intentó correr el hilo, así que el hilo no estaba en una cola: seguía bloqueado.
- El broker esperó 40 recepciones de 100 ms sin recibir nada y salió a los 4 261 ms (`BROKER_IDLE_NS`, 4 s sin intención). La publicación con intención de outbox llegó a los 8,3 s, cuando ya no había broker. El servicio registró `UNKNOWN` (3), que es lo correcto si nadie contesta. La puerta pide `DELIVERED`.
- La corrida que pasa del mismo caso (`build/k4-cases/cut-after-checkpoint`, que reescribió el `check_k5.py` siguiente) termina el tramo a los 0,6 s.

*El mecanismo.* `ipcops::call` admite el mensaje, que desde ese momento está en la cola del endpoint, y después registra la espera (`prepare_wait_offering`, el hilo pasa a `Blocked`). Lo hace dentro de una toma **compartida** de la máquina. `ENDPOINT_RECEIVE` e `INVOCATION_REPLY` también la toman compartida desde la versión 0.8.0 ([concurrencia](../architecture/concurrency.md), [ADR-024](ADR-024-log-reads-shared-auditor-settles-until-dispatched-probe-spread.md)). La respuesta despierta al llamador solo si lo encuentra `Blocked` (`thread::claim_handoff`). Si un receptor que ya corre en otro procesador toma el mensaje y responde antes de que el llamador registre su espera, la respuesta queda guardada en el registro de la invocación y no despierta a nadie. El llamador se bloquea después y duerme hasta su plazo. Al vencer, `call` encuentra `replied` y devuelve la respuesta como si nada, y por eso ningún criterio salvo `outbox`, que depende de los 4 s del broker, vio la demora. El comentario de `prepare_wait` ya pedía volver a mirar la condición bajo el cerrojo del objeto. `call` no lo hacía.

Para que ocurra, el procesador del llamador tiene que tardar entre la admisión y el registro más de lo que tarda el receptor en recibir y responder. La admisión escribe `ipc.admitted` y `ctrl.receipt` por el puerto serie antes de registrar, y bajo TCG eso son cientos de µs. Que esta misma carrera sea la causa del `IndexError` de `cut-after-checkpoint` en K7 y del `outbox` del cierre 1 de K8 es una hipótesis. Esas corridas se perdieron.

*Lo que queda de `K1 − L1` es el disco de K4.* Son medianas por caso de los 96 registros de K1 de esa medida. `publish_ns` da 0,60 a 0,69 ms con dos publicaciones del link, 0,86 a 1,07 ms con tres y 1,70 ms con cinco, unos 0,25 a 0,33 ms por publicación, con `disk_writes` igual a 3 por publicación. Antes de [ADR-029](ADR-029-publication-objects-written-as-one-run.md), una publicación costaba 0,75 a 0,9 ms con 10 a 20 escrituras. Bajar 0,45 a 0,55 ms con 8 a 14 peticiones menos da unos **40 a 60 µs por petición** (inferido, no medido). Una publicación son hoy tres escrituras y tres flushes en serie (`PREPARE`, la corrida de objetos y `COMMIT`, cada una con su flush), así que seis peticiones de ~50 µs explican casi todo `publish_ns`. Por fase, `open` da +0,077 ms, y `settle` de K1 es 0,2 a 0,6 ms más lento que el de L1 en los cinco casos que publican.

*De dónde puede salir el tiempo de una petición, sin medir.* Con `aio=threads`, que es el valor por defecto, y el disco en el bucle principal de QEMU, cada petición pasa por tres despertares de hilos del anfitrión. El `ioeventfd` del vCPU despierta al bucle principal, el bucle despierta a un hilo del pool para el `pwrite` o el `fdatasync` sobre tmpfs, y la terminación vuelve al bucle principal, que inyecta la interrupción por `irqfd`. El anfitrión corre con `powersave` y EPP `balance_performance` ([RES-003](../research/k8-transactions.md)). Del lado del invitado, el procesador que deja `k4disk` sondea 200 µs antes de `HLT` (`sched::IDLE_POLL_NS`), así que el despertar del driver debería ser barato. Esa repartición es la hipótesis H5 de RES-003 y no está medida. El `stats` del link no separa el tiempo del dispositivo del de los cruces.

**Elección.**

1. *Una llamada busca su respuesta después de registrar su espera* (`kernel/src/api/ipcops.rs`, `thread::withdraw_wait`). Después de `prepare_wait_offering`, sin soltar la toma de la máquina, `call` vuelve a leer la invocación bajo su cerrojo, que es donde la respuesta pone `replied`. Si la respuesta ya está, o la invocación ya se resolvió, retira su espera. Si en cambio un despertador la consumió primero, se bloquea, porque su despertar está en camino. Hay dos órdenes posibles:
   - *La respuesta toma el cerrojo de la invocación primero.* Si encontró al llamador todavía sin `Blocked`, guardó la respuesta y no consumió nada, así que el llamador retira su espera, no se bloquea y lee la respuesta guardada. Si lo encontró `Blocked`, consumió la espera y difirió el despertar, así que `withdraw_wait` no encuentra la espera y el llamador se bloquea para recibirlo, como antes.
   - *El llamador lee primero.* La espera ya estaba registrada y la respuesta, que llega después, la encuentra `Blocked` y despierta al hilo, como antes.

   Un plazo que vence entre el registro y la retirada consume la espera. El llamador se bloquea, el despertador espera a que el hilo deje el procesador (`on_cpu`) y la llamada termina con la respuesta guardada. Si no se bloquea, los despertares diferidos de la admisión salen en el momento (`flush_wakes`), como habrían salido al bloquearse. Ningún criterio, plazo ni espera de usuario cambia.
2. *`stats` cuenta el tiempo del disco.* `DiskReply` crece de 48 a 64 bytes con `device_requests` (peticiones de cualquier tipo que el dispositivo completó) y `device_ns` (el tiempo de `k4disk` entre notificar al dispositivo y tomar la terminación, interrupción y despertar del driver incluidos). `k4store` mide `call_ns`, su tiempo bloqueado en llamadas al driver que mueven el medio (lecturas, escrituras y flushes, sin `STATS`). Su `STATS` lleva en `digest1` cuatro `u64` little-endian (flushes, peticiones, `device_ns` y `call_ns`) en lugar de la política publicada, que nadie leía de ahí. El link responde, como diferencias desde que quedó listo, `disk_flushes`, `disk_requests`, `disk_device_ns` y `disk_call_ns` junto a `disk_writes`. Se leen así:
   - `disk_device_ns / disk_requests` es la latencia de una petición vista desde el driver.
   - `disk_call_ns − disk_device_ns` es lo que cuestan los cruces entre `k4store` y `k4disk`.
   - `publish_ns − disk_call_ns` es el trabajo de `k4store` y el cruce del link.
3. *El medio de K1 se atiende en un hilo de E/S con io_uring, solo bajo KVM* (H5, `tools/run_k5.py`). Con `THALYX_ACCEL=kvm`, la máquina lleva `-object iothread,id=k5io,poll-max-ns=200000`, el disco `aio=io_uring` y `virtio-blk-pci` `iothread=k5io`. El hilo sondea la cola y las terminaciones hasta 200 µs antes de dormir, el mismo plazo que el sondeo ocioso del invitado. Una escritura sobre tmpfs se completa al enviarse, sin pasar por el pool. El QEMU 11.1.1 de la cadena de herramientas enlaza `liburing.so.2`, y `kernel.io_uring_disabled` es 0 en el anfitrión. Bajo TCG nada cambia. Que QEMU arranque un hilo de E/S de virtio-blk sin KVM no se comprobó, y las puertas que deciden las corridas TCG no miden el tiempo del medio.

**Argumento de durabilidad del punto 3.** El modo de caché sigue siendo `cache=writeback`, sin `cache.no-flush`, y el dispositivo sigue siendo `virtio-blk`. `aio=` elige con qué mecanismo QEMU lleva una petición al archivo, no si respeta el flush. Un `VIRTIO_BLK_T_FLUSH` del invitado sigue completándose solo después del `fdatasync` del archivo, que con io_uring es un `IORING_OP_FSYNC` con `IORING_FSYNC_DATASYNC`. El hilo de E/S cambia qué hilo del anfitrión atiende la cola, no el orden. `k4disk` tiene una sola petición en vuelo y espera su terminación antes de la siguiente. El código del invitado no cambia, así que el argumento de recuperación de K4 ([persistencia](../architecture/persistence.md), ADR-029) sigue igual. `k8.durability` lee el modo de caché y la presencia de `virtio-blk` en la línea de QEMU, y los dos siguen ahí.

**Autocomprobación.**

- *La corrección.* Con la siguiente verificación, `cut-after-checkpoint` de la puerta K4 debería terminar su primer tramo en menos de un segundo bajo TCG, y `outbox` debería pasar en las tres corridas y en el `check_k4.py` de kiln. Si una llamada vuelve a dormir hasta su plazo, en el registro serie se ve como una invocación con `ipc.replied` y un llamador que no corre hasta `CALL_DEADLINE_NS` después, y la causa es otra.
- *Los contadores.* `disk_flushes` debería ser 3 por publicación del link: 6 en los diez casos sin publicación en `settle` y 9 en los cinco que publican. `disk_requests` debería ser `disk_writes + disk_flushes` más las lecturas, que en la transacción deberían ser pocas o ninguna.

**Diseñado y no medido.** Por la regla del mini sprint no se corrió ninguna puerta, prueba ni medida. Se compiló el workspace para `x86_64-unknown-none` en debug y en release, y K4 también con `quiet-io`. Se formateó. Lo que se espera, en cifras:

- Por petición al disco, entre 15 y 30 µs menos, de ~50 µs. Por publicación (seis peticiones), entre 0,09 y 0,18 ms menos.
- `open`: −0,1 a −0,2 ms en los 16 casos, por la publicación del `seed`.
- `settle`: otros −0,1 a −0,2 ms por cada publicación de `settle`, en los cinco casos que publican y en las dos de `two-transactions-in-a-row`.
- Extremo a extremo: unos −0,1 a −0,15 ms en los diez casos, −0,2 a −0,35 ms en los cinco y −0,3 a −0,5 ms en `two-transactions-in-a-row`. `k8.tx_e2e` bajaría de +0,045 ms a algo entre −0,15 y −0,05 ms. Es probable, pero no seguro, que el límite superior del IC quede debajo de cero.
- La corrección de la llamada no debería mover la latencia. Agrega una toma del cerrojo de la invocación por llamada, del orden de decenas de ns.

**Alternativas descartadas.**

- *Tomar la máquina en exclusiva para registrar la espera, o la respuesta:* devuelve la serialización que la 0.8.0 quitó del camino de IPC.
- *Registrar la espera antes de admitir:* la espera nombra la invocación y su generación, que solo existen después de admitir. Además, una admisión rechazada tendría que deshacer una espera ajena a su error.
- *Subir `BROKER_IDLE_NS` o `CALL_DEADLINE_NS`:* esconde la causa, y la regla de K8 lo prohíbe.
- *Sondear el anillo usado en `k4disk` antes de dormir en la interrupción (H2):* ataca el despertar del invitado, que ya sondea 200 µs. No se elige sin los contadores. Si `disk_device_ns / disk_requests` sigue alto con el hilo de E/S, es lo siguiente.
- *`PREPARE`, objetos y `COMMIT` bajo un solo flush (H3):* sigue cambiando lo que la puerta K4 espera de cada corte (ADR-029).
- *El hilo de E/S también bajo TCG:* no se sabe si arranca, y una QEMU que no arranca rompe todas las puertas.

**Consecuencias.** Bajo KVM, el QEMU de K1 y de las puertas K5 tiene un hilo más, que gira hasta 200 µs después de cada petición o terminación y consume CPU del anfitrión mientras tanto. La máquina de las puertas TCG y la de la medida difieren en esto, y se declara. Toda `ENDPOINT_CALL` hace una lectura más de su invocación. El `STATS` de K4 ya no lleva la política publicada en `digest1`.

**Revisión.**

- Si `disk_device_ns / disk_requests` queda en 15 µs o menos, el dispositivo no es el costo, y el reparto entre `disk_call_ns − disk_device_ns` y `publish_ns − disk_call_ns` elige el cruce siguiente.
- Si queda en 40 µs o más y `tx_e2e` no se movió, el hilo de E/S y io_uring no ayudaron. Se quitan, porque son configuración del emulador y no deben quedarse sin efecto, y toca H2 o H3.
- Si `outbox` vuelve a fallar, la corrida está en `build/gate-failures/`.
- El −9 de K6 sigue sin causa.

**Referencias.** [ADR-024](ADR-024-log-reads-shared-auditor-settles-until-dispatched-probe-spread.md), [ADR-028](ADR-028-link-notes-after-serving-and-kept-gate-failures.md), [ADR-029](ADR-029-publication-objects-written-as-one-run.md), [RES-003](../research/k8-transactions.md), [Concurrencia](../architecture/concurrency.md), [Persistencia](../architecture/persistence.md), [estado actual](../roadmap/current-state.md).
