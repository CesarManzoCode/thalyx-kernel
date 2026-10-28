---
id: ADR-020
kind: decision
status: accepted
---
# El auditor de K6 deja empezar ventanas nuevas antes de devolver una entrada relevada, y las recorridas de ámbito quedan fuera de línea

**Problema.** El primer cierre de K7 (`build/k7/check-20261028T122146`, kernel en `d6ac7f1`) dejó la puerta K6 en 23/24, igual que las dos réplicas del baseline. Solo cae `no_loss`: `LIMIT_EXHAUSTED` (−9) en el brazo nativo, en `ipc.caps` con 1 capacidad, ronda 3. [ADR-014](ADR-014-exact-scope-reservations-and-lineage-walks.md) lo atribuyó como hipótesis a la ventana en que `bounded_add` deshacía una suma que se pasaba del techo. Su condición de revisión se cumplió: la suma acotada exacta no lo quitó. En el mismo cierre, `k7.size_kernel_load` midió 618 496 bytes contra 573 440 en el baseline.

**Evidencia.** Observado en los registros serie de las campañas `k7-base-a`, `k7-base-b` y `k7-check-20261028T122146`, y en el código de `d6ac7f1`:

- El −9 de `ipc.caps` cae en el mismo punto del plan en `k7-base-b` y en el cierre: la entrada justo después de `closure.unit`, que corre el supervisor relevado (nota `0x6005` con `note_seq=6` del cliente). En `k7-base-a`, con el mismo orden, no cayó.
- Entre `BEGIN` de `ipc.caps` y la nota de error, el cliente hizo 251 entradas al kernel: una creación de memoria, 248 llamadas admitidas, la rechazada y la nota. El log de control tiene 256 celdas y 8 reservadas al cierre de autoridad, así que el tráfico ordinario cabe en 248. Mientras duró la entrada, el auditor no drenó ningún recibo.
- El auditor es el propio supervisor (`user/k6super`), en el ámbito `supervisor`, con medio ventana de ejecución (`CPU_WINDOW_NS / 2`, 5 ms de cada 10). `closure.unit` termina escribiendo sus muestras como notas síncronas: en el registro, cada nota llega unos 2,4 ms después de la anterior, y las ráfagas empiezan exactamente en cada frontera de ventana (…0,27 ms de cada múltiplo de 10 ms). Son dos o tres notas por ventana y luego el despacho se rechaza hasta la siguiente. `roll_window` arrastra el exceso a la ventana siguiente; el supervisor cierra la campaña con `overruns=1042`, `max_overrun_ns=3664949` y `dispatch_refusals=25356`.
- Al terminar la entrada relevada, el supervisor levantaba `HANDOFF_DONE` en la misma ventana en que había gastado su presupuesto. El cliente empezaba la entrada siguiente y el auditor no podía despacharse hasta la frontera. A ritmo de `ipc.caps`, 248 llamadas caben de sobra en lo que queda de esa ventana.
- Los −9 de `scale.ipc` con 2 y 4 pares del baseline (ronda 6 de las dos réplicas) no siguen a una entrada relevada, y el cierre no los repitió. Con el mismo mecanismo bastaría que el auditor agotara su medio ventana drenando o compitiendo por procesadores. Es una hipótesis: las imágenes de medida no escriben qué recurso rechazó.
- El texto del kernel pasó de `0x45000` a `0x4f000` en `2ce6cda` y a `0x50000` en `bc2ba73` (kernel compilado commit por commit). En `2ce6cda`, `scope::release` y `scope::is_open` dejaron de tener símbolo propio: el bucle contado de hasta diecisiete pasos, con su `compare_exchange`, quedó copiado y desenrollado en cada uno de los 67 llamadores de `scope::reserve`, `scope::release` y `scope::is_open`. Las funciones que más crecieron son las que reservan y liberan: `domain::create_in` (+6 027 bytes), `scopeops::retire` (+4 719) y `domainops::map` (+4 173).

**Elección.**

*El auditor se repone antes de devolver una entrada.* Después de una entrada relevada, y antes de levantar `HANDOFF_DONE`, `entries::settle` duerme en el kernel, drenando el log si llega algo, hasta que empiezan `SETTLE_WINDOWS` (3) ventanas nuevas. Dormir no gasta ejecución, así que cada frontera devuelve hasta un presupuesto. Tres fronteras cubren un exceso de hasta dos presupuestos. El mayor medido en las tres campañas es de 5,44 ms (`k7-base-a`), poco más de uno. Las muestras de la entrada relevada ya están tomadas y las de la siguiente todavía no empiezan, así que la espera no entra en ninguna medida. Cuesta hasta 30 ms por entrada relevada.

*Las recorridas de ámbito, fuera de línea.* `scope::is_open`, `scope::reserve` y `scope::release` llevan `#[inline(never)]`. La recorrida, la regla de cadena y la suma acotada exacta de ADR-014 no cambian, solo dónde vive el código. Compilado aparte, el texto vuelve a `0x46000`.

*Salvo en el camino de IPC.* Una medida completa con el árbol de este cambio (`build/k7/check-20261028T130936`) pasó la puerta K6 24/24, pero la llamada se vio en el viaje de ida y vuelta: `k7.k6_ipc_lineage_*` subió unos 35 ns a toda profundidad (con 8 pasos, [2 140, 2 161] ns contra [2 110, 2 125] en el cierre anterior), y tres de esas cuatro métricas dejaron de pasar. Es la condición de revisión de abajo. El cuerpo de cada recorrida es ahora una sola función `#[inline(always)]` (`is_open_inline`, `reserve_inline`, `release_inline`). Las formas sin sufijo la llaman fuera de línea. Las usan, en línea, solo la admisión (`QueueBytes`), la liberación de una invocación, la instalación y la liberación de una capacidad (`Metadata`), `OpenMemo` en la recorrida de linaje y `begin_effect`. El rechazo de una reserva, que deshace y deja la traza, pasa a una función fría fuera de línea en todas las copias. Así las seis copias cuestan unos 900 bytes y el texto sigue en `0x46000`.

**Autocomprobación.** `no_loss` de la puerta K6 es lo que este cambio debe dejar en cero, y la puerta lee cada error de cada entrada de cada ronda. El tamaño lo mide `k7.size_kernel_load` sobre el ELF de la imagen de medida.

**Diseñado y comprobado solo en parte.** Que el −9 de `ipc.caps` venga del auditor sin presupuesto es una inferencia fuerte: la cuenta exacta de 248 admisiones y el patrón de notas por ventana. Pero no es una lectura directa del rechazo, que la imagen de medida no escribe. Una sola campaña sin pérdidas no prueba la ausencia de la carrera: `k7-base-a` tampoco perdió en ese punto. `scale.ipc` queda sin mecanismo dirigido. El texto en `0x46000` deja la carga del kernel en 577 536 bytes, una página por encima del baseline. `k7.size_kernel_load` pide bajar de él, y este cambio solo quita la regresión. Para pasar faltan dos páginas: unos 1 800 bytes de texto (fin en `0x456df`) y unos 2 100 de datos (fin de `.bss` en `0x8c819`). Con las copias en línea, la medida `build/k7/check-20261028T144837` dio medianas de `ipc.lineage` de 2 045, 2 085, 2 125 y 2 205 ns con 0, 8, 16 y 28 pasos. Son más bajas que las del cierre anterior (2 070, 2 110, 2 155 y 2 230) y que las de todo fuera de línea (2 116, 2 146, 2 196 y 2 271). Aun así, solo `k7.k6_ipc_lineage_8` pasa su umbral: los intervalos de las otras tres tienen un extremo alto de entre 2 255 y 2 640 ns.

**Alternativas descartadas.**

- *Subir el presupuesto del ámbito `supervisor`:* cambia un parámetro de arranque del kernel que K2–K5 comparten, y cambia las condiciones en que se miden `closure.unit` y `quota.share`.
- *Reintentar en el cliente un −9:* es lo que `no_loss` existe para ver. Un log lleno rechaza, y el contrato de observabilidad dice que ese rechazo es visible.
- *Agrandar el log:* cambia el ABI (`CONTROL_LOG_CAPACITY`). Un log mayor solo alarga lo que tarda en llenarse mientras el auditor no puede correr.
- *Sacar de línea también `Chain::of`:* quitaría unos 3,4 KB más, pero está en el despacho (`charge_cpu`, `available_ns`, `reserve_cpu`), y el cambio de costo no se midió.

**Consecuencias.** Una ronda nativa de K6 tarda hasta unos 150 ms más, con cinco entradas relevadas. Ningún parámetro, techo, interfaz ni registro del kernel cambia. Una reserva o liberación de ámbito es ahora una llamada en lugar de código en línea.

**Revisión.** Si `no_loss` vuelve a caer después de una entrada relevada, hay que medir con la traza de rechazos qué recurso rechazó. Si cae en `scale.ipc`, hay que medir el consumo del auditor por ventana bajo esa carga. Si el costo de la llamada se ve en `k7.k6_ipc_call_*` o en `k7.k6_ipc_lineage_*`, hay que volver a meter en línea solo los llamadores del camino de IPC.

**Referencias.** [ADR-014](ADR-014-exact-scope-reservations-and-lineage-walks.md), [ADR-010](ADR-010-k6-parameters-and-wake-policy.md), [Observabilidad](../architecture/observability.md), [Recursos](../architecture/resources.md), [estado actual](../roadmap/current-state.md).
