---
id: ADR-024
kind: decision
status: accepted
---
# La lectura y el acuse del log de control se toman compartidos; el auditor de K6 espera a que su deuda esté pagada; el supervisor de K3 espera a que el probe haya corrido en todos los procesadores

**Problema.** La verificación de kiln al cierre de la ronda `k7-r01` (`build/k7/check-20261029T134632`, kernel en `61f27ad`) dejó en `FAIL` 34 criterios. Los que esta decisión ataca:

- `k7.behavior_gates` y `k7.behavior_tests` (364 contra 392). En la tercera de las tres ejecuciones TCG de K1–K5 falló el criterio `shootdown` de K3. Con él cayeron la autocomprobación de K3 (57 daños que no se cuentan) y, en cascada, K4 y K5. La campaña K6 de ese cierre lee esas mismas puertas y quedó en 21/24.
- `check_k6.py`, en la campaña `kiln-k6` del mismo día: 23/24, con `no_loss` en `FAIL`. Hay −9 (`LIMIT_EXHAUSTED`) en el brazo nativo, en `scale.ipc:1` de la ronda 3 (15 561 trozos rechazados) y en `scale.ipc:4` de las rondas 4 (307) y 6 (10 120).
- Las métricas de IPC y de escala del brazo nativo (`ipc.call`, `ipc.caps`, `ipc.lineage`, `scale.ipc`, `audit.drain`), que siguen sin cruzar su umbral.

**Evidencia.** Observado en `build/k7/` y en `build/k6/campaigns/kiln-k6/`, y en el código de `61f27ad`:

- *K3.* Desde `2f7e14f` hubo 18 ejecuciones TCG de K1–K5 en las verificaciones de `build/k7/`. K3 falló en 3: `probe` (`check-20261028T151453`), `k2` en cascada desde un `budget` de K2 (`check-20261028T215528`, el fallo que trató ADR-023) y `shootdown` (este cierre). No se conserva el registro serie de ninguna de las tres. `shootdown` exige que el retiro del probe venga de un espacio que corrió en todos los procesadores (`space_cpus` igual al número de procesadores en línea). El supervisor esperaba esa condición para el escritor del sello, pero no para el probe. En la última ejecución que pasó (`build/run-k3/serial.log`), el escritor tardó 108 ventanas en extenderse (`SPREAD_OBSERVED`, `b=0x6c0000000f`), y el probe se retiró con `space_cpu_mask=0xf`.
- *El auditor y la deuda.* En la ronda 3 nativa de `kiln-k6`, el ámbito `supervisor` (5 ms de cada ventana de 10) cierra con `overruns=1052`, `dispatch_refusals=25727` y una deuda acumulada de 1,75 s. En el orden del plan de esa ronda, `scale.ipc:1` va dos entradas después de `closure.unit`, una entrada relevada que termina escribiendo sus muestras como notas seriales síncronas. ADR-020 fijó un reposo de tres ventanas después de cada relevo, y tres ventanas solo pagan hasta dos presupuestos de exceso.
- *El auditor y el cerrojo.* En `61f27ad`, `LOG_READ` tomaba el cerrojo de control en exclusiva, y `LOG_ACK` y `LOG_QUERY` iban por el camino exclusivo general. El viaje de IPC lo toma compartido, y cada admisión escribe su recibo bajo el cerrojo del log. El lector se despierta cada 16 recibos (`RECEIPT_BATCH`), y cada despertar trae una lectura y un acuse. Son dos tomas exclusivas por cada 16 admisiones. Cada una espera a que salgan los lectores de todos los procesadores y no deja entrar a ninguno mientras tanto. En esa misma ronda, `lock.control` informa `contended_fraction_ppm=349315`.
- En `scale.ipc:4` de las rondas 4 y 6 no hubo un relevo en las cinco entradas anteriores, así que la deuda no explica sola esos −9.

**Elección.**

*El log se lee, se acusa y se consulta bajo su propio cerrojo.* `logops::read` toma la máquina compartida. Dentro del cerrojo del log lee la tanda, decide si responde y, si espera, deja registrado al lector. `wake_reader` y `receipt` deciden el despertar bajo ese mismo cerrojo, así que comprobar la cuenta y registrarse siguen siendo un solo paso para ellos. Con el cerrojo del log tomado se toma el registro de espera del hilo: es el orden que ya declara [Concurrencia](../architecture/concurrency.md) (log de control → registro de espera de un hilo). `LOG_ACK` y `LOG_QUERY` pasan a la lista de operaciones compartidas de `api::shared`. Sus brazos del camino exclusivo se quitan, porque ya no se alcanzan. Con ellos, la carga del kernel quedaba una página por encima del umbral de `k7.size_kernel_load`, y sin ellos vuelve a 569 344 bytes.

*El reposo dura hasta que la deuda está pagada.* `entries::settle` ya no espera un número fijo de ventanas. Pide que lo despierten al comienzo de la ventana siguiente y mira cuándo despertó. Un hilo cuyo ámbito arrastra un presupuesto o más no se despacha en esa ventana y despierta una o más ventanas tarde. Si despierta dentro de media ventana desde lo que pidió, `SETTLE_WINDOWS` (ahora 2) veces seguidas, el exceso arrastrado ya bajó de un presupuesto y quedó pagado. `SETTLE_LIMIT` lo acota a 400 ventanas.

*El supervisor de K3 espera también al probe.* El bucle que esperaba a que la máscara del escritor cubriera todos los procesadores ahora exige lo mismo de la máscara del probe (`domain_query(probe).space_cpu_mask`), con el mismo límite de `SPREAD_WINDOWS`. El probe se detiene solo si llega a 1 000 000 de vueltas por hilo. En la ejecución que pasó, sus dos hilos sumaron 502 notas, una cada 64 vueltas, unas 32 000 vueltas en 1,4 s, y la espera añade a lo sumo 200 ventanas.

**Autocomprobación.** `shootdown`, `probe` y `seal` de K3 y sus daños, en las tres ejecuciones TCG. `no_loss` y `audit` de K6, que leen cada error de cada entrada y exigen cero recibos perdidos. Los criterios de la puerta no cambian.

**Diseñado y no medido.** Por la regla del mini sprint, este cambio se escribió sin correr puertas ni campañas. Solo se compiló el workspace y se midió la carga del kernel. Quedan tres hipótesis:

- Que el `shootdown` que falló sea el de `space_cpus`. En la ejecución que falló no se leyó qué condición faltó.
- Que la deuda del auditor explique el −9 de `scale.ipc:1` en la ronda 3, y que la toma exclusiva del log explique al menos parte del de `scale.ipc:4`. La imagen de medida no escribe qué reserva rechazó.
- Que quitar la toma exclusiva mueva las latencias de IPC del brazo nativo y `audit.drain`. La espera de los lectores sí desaparece; cuánto pesa en cada métrica no se midió.

**Alternativas descartadas.**

- *Subir el presupuesto del ámbito `supervisor` o mover el auditor a otro dominio:* las descartaron ADR-020 y ADR-023 por las mismas razones.
- *Más ventanas fijas de reposo:* una cifra que paga la deuda de la entrada más cara alarga cada relevo, y la siguiente entrada más cara la vuelve a superar.
- *Que el supervisor consulte la deuda de su propio ámbito:* no tiene capacidad sobre él (`SELF_SCOPE` nombra el ámbito `system`), y dársela cambia los huecos de arranque que K2 comprueba.
- *Despertar al lector con más recibos por tanda:* reduce las tomas, pero deja menos margen en un log de 248 celdas ordinarias, y la toma exclusiva sigue ahí.

**Consecuencias.** Las operaciones del log, salvo `LOG_APPEND`, ya no excluyen el viaje de IPC. Un relevo puede tardar menos que antes, si el auditor no debe nada (dos comienzos de ventana en lugar de tres), o hasta 400 ventanas si debe mucho. Ninguna entrada mide esa espera. K3 puede tardar más en llegar al retiro si el probe tarda en extenderse.

**Revisión.** Si `no_loss` vuelve a caer en `scale.ipc`, hay que medir con trazas qué reserva rechazó y cuánto gasta el auditor por ventana bajo esa carga, como ya pedía ADR-023. Si `shootdown` o `probe` de K3 vuelven a caer, hay que conservar el registro serie de la ejecución que falla y leer la condición.

**Referencias.** [ADR-020](ADR-020-auditor-settles-after-handoff-and-scope-walks-out-of-line.md), [ADR-023](ADR-023-exit-records-starvation-and-auditor-turns.md), [Concurrencia](../architecture/concurrency.md), [Observabilidad](../architecture/observability.md), [estado actual](../roadmap/current-state.md).
