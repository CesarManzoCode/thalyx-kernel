---
id: ADR-027
kind: decision
status: accepted
---
# Un fallo se registra después del retiro que lo causó; la respuesta puede cerrar su ticket; la puerta K4 dice qué estado tenía el outbox

**Problema.** La verificación de kiln al cierre de la ronda `k7-r04` (`build/k7/check-20261029T172017`, kernel en `a630f6b`) dejó 20 criterios en `FAIL`. Dos son de comportamiento y valen por varios. `k7.behavior_gates` cayó porque la primera ejecución TCG de K1–K5 terminó en `IndexError` en la puerta K4 y en su autocomprobación. `k7.behavior_tests` dio 346 contra 392: los 46 daños de K4 no se contaron. El `check_k3.py` de kiln, corrido aparte, falló además `probe` (27/28), y con él la autocomprobación de K3. Entre las métricas, `ipc.call`, `ipc.caps`, `ipc.lineage:0` y `scale.ipc:1` necesitan entre un 1 % y un 9 % menos por llamada.

**Evidencia.** Observado en los registros de `172017`, en `vault/private/evidence/k7-acceptance.log` y en el código de `a630f6b`:

- *K3, la carrera.* `domainops::unmap` quita las entradas con la máquina tomada, la suelta, espera los acuses (`tlb::shootdown_space`) y solo entonces escribe `mem.unmapped`, porque el registro lleva lo que la espera observó. `memops::seal` hace lo mismo con `mem.sealed`. Un hilo del probe en un procesador que ya confirmó, o que nunca tuvo la entrada en caché, falla en ese intervalo. `terminate_on_fault` toma la máquina, que ya está libre, y escribe `user.fault` antes que el registro del retiro. `check_probe` lee eso como «el probe falló antes del retiro, no por él», y `check_seal` como «el escritor nunca falló después del sello». En la última ejecución que pasó (`build/run-k3/serial.log`), el retiro dice `active_in_space=0 loaded_cpus=0`: el probe no estaba corriendo, y su fallo llegó 8 ms después. La carrera solo se abre cuando un hilo del probe está corriendo en el momento del retiro, y eso explica que falle pocas veces. En los registros conservados, `probe` cayó dos veces: en la tercera ejecución de `check-20261028T151453` y en el `check_k3.py` de este cierre. Hay 48 registros de puertas en `build/k7/`. El registro serie de la ejecución que falló se sobrescribió, así que la condición exacta que faltó no se leyó. Lo que se ve es que el código permite la carrera.
- *K4, el `IndexError`.* `check_outbox` filtra los registros de outbox de `cut-after-checkpoint/1` por `DELIVERED` y después indexa `entries[0]` sin comprobar que quedara alguno. Es el único índice sin guarda que se encontró en `tools/check_k4.py`. El servicio escribe `UNKNOWN` cuando su llamada al broker vuelve con error, con un plazo de 2 s (`resolve_outbox`). Cuál fue el error no se sabe: el medio de esa ejecución también se sobrescribió. En la ejecución conservada el registro dice `DELIVERED`.
- *IPC, el servidor.* El servidor nativo de K6 (`serve` en `tests/k6/plat_native.c`) hace tres entradas por llamada: recibir, responder y cerrar el ticket. La respuesta despierta al cliente con `WakeHint::Sync` en el mismo procesador, y el cliente corre recién cuando el servidor vuelve a bloquearse en su recepción. El cierre es entonces una entrada entera en el camino crítico de cada viaje de ida y vuelta. Además, el servidor copiaba el mensaje cuatro veces y ponía a cero unos 1,2 KB que nunca se envían.
- *El texto del kernel.* En `a630f6b`, `.text` terminaba 17 bytes antes del borde de página. Cualquier código nuevo agrega una página a `k7.size_kernel_load`, que no tiene margen (569 344 contra < 573 440).

**Elección.**

*Un fallo espera el registro del retiro que lo pudo causar.* `tlb::UNRECORDED` cuenta los retiros que ya quitaron sus entradas y todavía no escribieron su registro. `Unrecorded::hold` lo sube en la misma toma de la máquina que quita las entradas, tanto en `unmap` como en `seal`. Su `Drop` lo baja después del registro o de la salida con error. `terminate_on_fault` toma la máquina. Si encuentra un retiro pendiente, la suelta, espera en `await_recorded` sin cerrojo y sirviendo los acuses, y la vuelve a tomar. Lo hace una sola vez, con el límite de una espera de acuse. Leída con la máquina tomada, la cuenta es exacta: un retiro que no aparece ahí no quita nada hasta que el fallo suelte la máquina. Es una cuenta para toda la máquina y no una por espacio. Un fallo en un espacio que nadie retira puede esperar de más, como mucho una espera de acuse, y un fallo de usuario termina su dominio, así que no está en ningún camino frecuente.

*La respuesta puede cerrar su ticket.* El ABI pasa a 0.5 con la bandera `CLOSE_AFTER` (bit 1). En `INVOCATION_REPLY` cierra el handle de la invocación como lo haría `CAP_CLOSE`, en la misma toma compartida y después de una respuesta aceptada. Una respuesta rechazada no cierra nada. Un cierre que falla después no deshace la respuesta. En cualquier otra operación, la bandera se rechaza con `INVALID_ARGUMENT` antes de tener efecto. El runtime nativo agrega `th_reply_close`. El servidor de K6 responde con la bandera y cierra él mismo el ticket solo si la respuesta fue rechazada. Además, copia el payload una sola vez, del descriptor en que llegó al de la respuesta. `bench.c` y el brazo Linux no cambian.

*La puerta K4 dice lo que encontró.* Si no queda ningún registro `DELIVERED`, `check_outbox` falla con los estados que había en el medio y con lo que el servicio dijo haber registrado.

*El texto vuelve bajo la página.* Los brazos de `api::simple` que `api::shared` siempre atiende (`CAP_INSPECT`, `CAP_DERIVE`, `CAP_COPY`, `ENDPOINT_SEND`, `INVOCATION_REPLY`) nunca se alcanzaban, y se quitan, como ya se hizo con los del log en ADR-024. Con eso el compilador rehízo el inlining y el texto bajó 1 351 bytes netos. La carga del kernel de la imagen de medida sigue en 569 344 bytes, ahora con unos 1,6 KB de margen en `.text`.

**Autocomprobación.** `probe`, `seal` y `shootdown` de K3 con sus daños, sin criterios nuevos. En K2, `k2client` agrega un control negativo: `MEMORY_QUERY` con `CLOSE_AFTER` sobre su propio búfer, que debe rechazarse con `INVALID_ARGUMENT`. El cliente sigue usando ese búfer justo después (`derive`), así que un rechazo que lo hubiera cerrado se vería en `controls`. En K6, `no_loss`, `audit` y el error de cada entrada de IPC: un servidor que no respondiera o no soltara los tickets agotaría el registro de invocaciones y se vería ahí.

**Diseñado y no medido.** Por la regla del mini sprint, nada de esto se ejecutó. Se compilaron el workspace, la imagen de medida (`--phase k5 --stage thalyx --trace off --k4-io-notes off`) y el runtime nativo con `nbench`, y se midió la carga del kernel. Quedan tres hipótesis:

- Que el `probe` que falló sea el «falló antes del retiro». Puede ser otro: un hilo del probe que leyó antes del retiro y escribió su nota después del fallo del otro hilo sigue siendo posible, aunque es menos probable.
- Que el `IndexError` venga de `check_outbox`. Si fue así, la próxima vez la puerta dirá qué estado escribió el servicio, pero el criterio seguirá fallando: la causa de fondo no se corrigió.
- Que quitar una entrada por viaje baje `ipc.call`, `ipc.caps`, `ipc.lineage` y suba `scale.ipc`. Por `entry.version`, una entrada cuesta unos 50 ns. A eso se suma el trabajo del cierre y las copias que el servidor dejó de hacer. Cuánto suma no se midió. `ipc.call:0` necesita unos 190 ns, y no se espera que pase solo con esto.

**Alternativas descartadas.**

- *Escribir el registro del retiro antes de esperar los acuses:* el registro dejaría de decir lo que se observó, y `shootdown` lo lee.
- *Esperar los acuses con la máquina tomada:* rompe la regla de [Concurrencia](../architecture/concurrency.md) de no esperar un acuse con un cerrojo tomado.
- *Una cuenta por espacio:* es más exacta, pero desenrollada en dieciséis dominios ocupaba unos 850 bytes de texto, más de lo que había hasta la página siguiente.
- *Que la respuesta cierre siempre el ticket:* cambia el contrato para K4, K5 y el link, que consultan o resuelven el ticket después de responder.
- *Una operación nueva de responder y recibir:* ahorraría otra entrada, pero es una operación nueva del ABI con su propio contrato de espera, demasiado para una ronda sin medir.

**Consecuencias.** Un fallo de usuario puede tardar hasta una espera de acuse más en registrarse. El ABI 0.5 acepta el bit 1 de las banderas. Un programa compilado para 0.4 no lo usa y no ve ningún cambio. El servidor de K6 hace dos entradas por llamada sin capacidades, en lugar de tres.

**Revisión.** Si `probe` vuelve a caer, hay que conservar el registro serie de la ejecución que falla y leer qué condición faltó. Si la puerta K4 vuelve a fallar en `outbox`, hay que leer el estado que ahora reporta y el error de `endpoint_call` en `resolve_outbox`. Si las métricas de IPC no se mueven, hay que medir las entradas del servidor por llamada en una campaña.

**Referencias.** [ADR-024](ADR-024-log-reads-shared-auditor-settles-until-dispatched-probe-spread.md), [ADR-026](ADR-026-k6-client-rewarms-after-handoff.md), [Concurrencia](../architecture/concurrency.md), [IPC](../architecture/ipc.md), [estado actual](../roadmap/current-state.md).
