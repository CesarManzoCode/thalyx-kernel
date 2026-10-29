---
id: ADR-023
kind: decision
status: accepted
---
# La salida del kernel registra la retención por presupuesto igual que el tick, y el auditor de K6 deja de pagar consultas en cada vuelta con recibos

**Problema.** El tercer cierre de K7 (`build/k7/check-20261028T215528`, kernel en `a635d89`) dejó en `FAIL` los dos criterios de comportamiento, además de 25 métricas:

- `k7.behavior_gates` y `k7.behavior_tests` (307 contra 392). En la segunda de las tres ejecuciones TCG de K1–K5 cayó el criterio `budget` de K2, y con él, en cascada, K3, K4 y K5 y las autocomprobaciones de K2 y K3. Las 85 pruebas que faltan son exactamente esas autocomprobaciones (28 + 57).
- La puerta K6 quedó en 23/24. Falla `no_loss`: `LIMIT_EXHAUSTED` (−9) en el brazo nativo, en `scale.ipc` con 1 par (ronda 2) y con 2 pares (ronda 5).

**Evidencia.** Observado en los registros de `build/k7/` y `build/k6/campaigns/`, y en el código de `a635d89`:

- *K2 `budget`.* En `a635d89` cayó en 2 de 6 ejecuciones TCG (`check-20261028T174953`, ejecución 2; `check-20261028T215528`, ejecución 1). En `d6ac7f1` y `2f7e14f` no cayó en 21 ejecuciones, ni en las 6 del baseline. El registro serie de la ejecución que falla no se conserva (la siguiente lo sobrescribe), y el log de las puertas no guarda el detalle del criterio, así que no se leyó cuál de sus condiciones faltó.
- En la última ejecución que pasó (`build/run-k2/serial.log`), el criterio se sostuvo en 3 registros `sched.preempt` con `starved=1` de 6. Las otras condiciones tienen margen: 31 ventanas cerraron con exceso. Los registros `sched.preempt` solo se escribían en el tick (`on_tick`). Cuando una reserva se acaba dentro de una entrada, o en user y la siguiente entrada llega antes que el tick, el despacho termina en `kernel_exit` sin registro. El supervisor de K2 (hilo 8) llega a `preemptions=16` en su nota 18 sin un solo `sched.preempt`. El cliente de K2 pasa su espera final en `burn` más `now_ns`, una consulta de reloj por vuelta, y desde ADR-022 esa consulta sale por el camino propio. Que haya o no un `starved=1` depende de si el tick llega antes que la siguiente salida.
- *K6 `no_loss`.* El mismo −9 en `scale.ipc` aparece en el baseline (`base-a` r6 con 2 pares; `base-b` r6 con 4 pares), en `check-20261028T151453` (r1, 2 pares), en `check-20261028T174953` (r5, 4 pares) y en este cierre. En la ronda 2 el cliente informa 200 trozos rechazados (`0x6008`, cantidad 1) y en la ronda 5, 727. Son ráfagas, no un rechazo sostenido: el corte es a los 100 000 seguidos.
- El auditor es el supervisor, en el ámbito `supervisor`, con media ventana (5 ms de cada 10). En la ronda 2 cierra con `dispatch_refusals=24978` y `overruns=1034`. Drenó 1 470 673 recibos en 288 643 lecturas, 122 089 de ellas vacías, con 1 446 901 entradas al kernel en toda la ronda. Una vuelta del bucle, que acaba en una lectura vacía, drena unos 12 recibos y hace unas 7 entradas: el reloj, la lectura que espera, el acuse, la lectura vacía que lo confirma, `signal_query` del relevo, `domain_query` del cliente y otra lectura del reloj para el plazo.
- El máximo observado del log (`AUDIT_HIGH_WATER`, entre 3 y 5) no contradice que se haya llenado: se observa cada 64 vueltas, después de drenar.

**Elección.**

*La salida registra la retención como el tick.* Cuando `kernel_exit` encuentra la reserva acabada y las trazas están activas, `starved_at_exit` hace la pregunta de `on_tick`: con la ventana al día y el cargo hecho bajo el cerrojo de la cola, si el despacho estaba vivo y el ámbito no tiene nada disponible. Si es así, y el hilo no agotó sus `PREEMPT_RECORD_LIMIT` registros, se escribe el mismo `sched.preempt`, con `trigger=exit`, el `vector` de la entrada y `rip`, `cs` y `cpl` del marco de esa entrada. Sin trazas no se pregunta nada. Para eso `on_kernel_exit` y `on_kernel_exit_with` reciben el marco. Además, el procesador que la salida le quita a un hilo de user se cuenta en `CPUS[].preemptions`, donde el tick cuenta los suyos. Antes no se contaba, y `k1.summary` informaba menos cambios forzados de los que hubo.

*El criterio lo lee así.* `budget` de K2 cuenta como retenido un `sched.preempt` con `starved=1`, `voluntary=0` y `trigger` igual a `timer` o `exit`. Dos daños nuevos de la autocomprobación lo prueban: un registro voluntario y uno con otro disparador ya no cuentan.

*Una vuelta del auditor con recibos vuelve al log.* En el bucle de `k6super`, si la vuelta drenó algo, no consulta el relevo, el estado del cliente ni el plazo, salvo cada 64 vueltas. Los recibos vienen de una entrada en curso. El cliente solo pide un relevo y solo termina entre entradas, y desde ese momento no escribe nada. Así, la vuelta siguiente encuentra el log vacío, espera hasta `AUDIT_SLEEP_NS` y entonces mira. En `entries::drain_timed`, una tanda de menos de 16 recibos vació el log al leerse, y ya no se relee para comprobarlo. Esa lectura se sigue haciendo mientras `audit.drain` toma sus 4096 muestras, así que las tandas que mide son las mismas de antes.

**Autocomprobación.** `budget` de K2, en cada una de las tres ejecuciones TCG, y sus 30 daños. `no_loss` de K6, que lee cada error de cada entrada de cada ronda. `audit` de K6, que sigue exigiendo cero recibos perdidos.

**Diseñado y no medido.** Este cambio se escribió sin correr puertas ni campañas, por la regla del mini sprint. Hay dos hipótesis:

- Que la condición de `budget` que faltó sea la de `starved`. No se leyó en la ejecución que falló.
- Que el −9 de `scale.ipc` sea el log de control lleno mientras el auditor está retenido por su ventana. Es la hipótesis que ADR-020 dejó abierta para `scale.ipc`, y la imagen de medida no escribe qué recurso rechazó. Que quitar unas 4 de las 7 entradas por vuelta con recibos baste para dejar margen es una estimación por cuenta de entradas, no por ciclos.

**Alternativas descartadas.**

- *Subir el presupuesto del ámbito `supervisor`:* lo descartó ADR-020 por las mismas razones.
- *Un auditor en un dominio propio con más presupuesto:* cambia quién audita y cómo se reparte el relevo, y no se puede probar en esta ronda.
- *Dormir el auditor entre tandas para que se junten:* un plazo se cumple en el tick, cada 500 µs, y en ese tiempo `scale.ipc` llena las 248 celdas ordinarias.
- *Contar en `budget` cualquier `starved=1`, sin mirar el disparador:* un registro mal formado pasaría.

**Consecuencias.** En las imágenes con trazas aparecen registros `sched.preempt` con `trigger=exit`, a lo sumo `PREEMPT_RECORD_LIMIT` por hilo contando los del tick. El criterio `preemption` de K1 sigue leyendo solo los de `trigger=timer`. La imagen de medida no escribe trazas, y en ella el cambio del kernel se reduce a pasar un puntero y a un contador por procesador. `AUDIT_POLLS` informa menos lecturas vacías. Un relevo puede empezar hasta `AUDIT_SLEEP_NS` más tarde, y ninguna entrada mide esa espera.

**Revisión.** Si `budget` de K2 vuelve a caer, hay que conservar el registro serie de la ejecución que falla y leer qué condición faltó. Si `no_loss` vuelve a caer en `scale.ipc`, hay que medir con trazas qué recurso rechazó y cuánto gasta el auditor por ventana bajo esa carga.

**Referencias.** [ADR-020](ADR-020-auditor-settles-after-handoff-and-scope-walks-out-of-line.md), [ADR-022](ADR-022-short-entries-quick-exit-and-kernel-image-pages.md), [Observabilidad](../architecture/observability.md), [Recursos](../architecture/resources.md), [estado actual](../roadmap/current-state.md).
