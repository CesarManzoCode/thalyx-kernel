---
id: ADR-014
kind: decision
status: accepted
---
# Una reserva de ámbito solo se rechaza contra lo que el contador tiene de verdad, y la recorrida de un linaje no vuelve a preguntar por un ámbito que ya vio abierto

**Problema.** En el baseline de K7 la puerta K6 da 23/24 en las dos réplicas (`build/k7/base-a|b/k6-gate.json`). Solo cae `no_loss`: el brazo nativo recibió `LIMIT_EXHAUSTED` (−9) en `scale.ipc` con 2 pares (réplica A, ronda 6), `ipc.caps` con 1 capacidad (B, ronda 3) y `scale.ipc` con 4 pares (B, ronda 6). [OQ-04](../roadmap/open-questions.md) pide abaratar la contabilidad de ámbito sin permitir agotamiento falso, y la [campaña de rendimiento](../evidence/k6-performance-campaign.md) dejó medido que esa contabilidad, recorrida de la hoja a la raíz sobre líneas que comparten todos los procesadores, es lo que queda del costo del IPC. Además, `ipc.lineage` crece con la profundidad del linaje: 2 065 ns con 0 derivaciones y 2 330 ns con 28 en el baseline.

**Evidencia.** Observado en el código, en el árbol de `b6b8017`:

- `bounded_add` en `kernel/src/scope.rs` sumaba con `fetch_add` y, si el resultado pasaba el techo, restaba lo sumado. Ninguna reserva aceptada pasaba el techo, pero una reserva que iba a fallar dejaba el contador por encima de él mientras se deshacía. Otro procesador que leyera el contador en ese intervalo se rechazaba por un espacio que estaba libre. Es el agotamiento falso que OQ-04 excluye.
- Las imágenes de medida no escriben `k2.admission_exhausted` ni `scope.limit_refused`, así que las campañas del baseline no dicen qué recurso rechazó. Que estos −9 vengan de esa ventana es una hipótesis compatible con los datos: ningún techo de `k6cli`, `k6srv`, `system` ni la raíz queda cerca de lo que el banco ocupa por sí solo (64 KiB de bytes de cola contra a lo sumo 48 mensajes de 64 a 320 bytes), y el rechazo aparece de forma esporádica, en rondas distintas, bajo concurrencia.
- `walk_lineage` en `kernel/src/api/mod.rs` llamaba a `scope::is_open` dos veces por nodo del linaje, para el ámbito de vida y para el patrocinador. Cada llamada construía la cadena de ámbitos hasta la raíz en un arreglo y la recorría después. En un linaje derivado en un solo dominio, el patrocinador es el mismo ámbito en todos los nodos.
- `resolve_entry` tomaba el cerrojo del nodo hoja para leer objeto, derechos y faceta, y `walk_lineage` lo volvía a tomar para leer el mismo registro.
- `Scope` no tenía disposición fija. El compilador agrupaba al final los campos pequeños, así que `state` y `parent`, que lee toda reserva y toda comprobación de linaje en cada ancestro, quedaban en la misma línea que `running` y `parallelism_used`, que el planificador escribe en cada despacho.

**Elección.**

*Suma acotada exacta.* `bounded_add` lee el contador y lo mueve con `compare_exchange` solo a un valor que no pasa el techo. Un rechazo se decide contra un valor que el contador tuvo de verdad, y ningún contador muestra a otro procesador la carga de una reserva que ya estaba fallando. La regla de la cadena no cambia: se carga de la hoja a la raíz, y un nivel que no cabe deshace los de abajo. La misma función sirve a las reservas de ejecución (`reserve_cpu`), que con esto dejan de rechazar un despacho por la carga transitoria de otro.

*Recorridas sin arreglo.* `is_open`, `reserve` y `release` siguen los enlaces al padre directamente, con el mismo límite de pasos, en lugar de construir un `Chain`. `is_open` para en el primer ámbito cerrado sin leer la raíz.

*Memoria por recorrida.* `scope::OpenMemo` guarda en la pila de una recorrida hasta cuatro ámbitos ya vistos abiertos. `walk_lineage` pregunta por cada uno una sola vez. Los estados solo avanzan, así que un ámbito abierto en la primera lectura estaba abierto en todo instante anterior. La recorrida nunca afirmó «abierto en un mismo instante» para todos los nodos, y reusar la lectura solo adelanta unos nanosegundos el instante de los nodos siguientes.

*Una lectura del nodo hoja.* `resolve_entry` copia el nodo hoja en una sola toma de su cerrojo y se la pasa a `walk_lineage_from`, que la usa como primer nodo.

*Disposición por línea.* `Scope` pasa a `repr(C, align(64))`, con un campo de tamaño cero alineado a 64 (`CacheLine`) entre clases. La primera línea tiene la estructura (estado, padre, profundidad, generación, techos), y una aserción en tiempo de compilación exige que quepa en ella. Siguen la identidad y las estadísticas de ventana, los contadores de reserva y los de despacho, cada clase en su propia línea.

*Entrada.* `on_kernel_exit` lee el índice del procesador una vez. La salida de `syscall` comprueba la parada con el hilo que la entrada ya encontró (`leave_if_dead_after_entry`) en lugar de volver a preguntar por el procesador y su hilo actual.

**Autocomprobación.** No hay un registro nuevo. `no_loss` en la puerta K6 ya lee lo que este cambio debe dejar en cero. `scope.limit_refused` y `k2.admission_exhausted` siguen nombrando el ámbito y el recurso cuando la traza está activa. La aserción de la disposición falla al compilar si la estructura deja de caber en una línea.

**Diseñado, no medido.** Se busca que la puerta K6 vuelva a 24/24 sin aflojar ningún techo, y bajar `k7.k6_ipc_lineage_*`, `k7.k6_ipc_call_*`, `k7.k6_ipc_caps_*` y, en menor medida, `k7.k6_entry_version_0` y `k7.k6_entry_clock_0`. Que los −9 del baseline vengan de la ventana de deshacer es una hipótesis. Si vienen de otro recurso (celdas de mensaje, invocaciones, recibos o ranuras de capacidad del receptor), este cambio no los quita, y el cierre de K7 lo verá en `no_loss`. Queda una ventana residual que este cambio no cierra: si un ancestro rechaza, los niveles de abajo quedan cargados mientras se deshacen, y una reserva más pequeña con la misma hoja puede rechazarse en esos nanosegundos. Para eso hace falta que el ancestro esté de verdad en su techo. La lectura antes del intercambio puede costar una transferencia de línea más cuando hay contención que el `fetch_add`. La separación por líneas busca compensarlo, y el balance lo mide el cierre. Las cifras de K7 no están en este registro.

**Alternativas descartadas.** *Reservas por procesador* de bytes de cola y metadatos: quitan los atómicos compartidos, pero un procesador puede rechazar a un hermano mientras a otro le sobra cupo, y eso es justo lo que OQ-04 excluye. *Comprobar toda la cadena antes de cargar:* añade una lectura por nivel y no cierra la carrera, solo la estrecha. *No cobrar bytes de cola en la entrega directa:* cambia lo que el contrato de recursos cuenta, y ninguna puerta lo podría verificar dentro de un mini sprint. *Saltar ancestros cuyo techo cubre la suma de los de sus hijos:* esa condición no se puede mantener sin cerrojo cuando los techos y el árbol cambian.

**Consecuencias.** El registro `Scope` crece hasta cuatro líneas de caché por ranura. Ninguna interfaz cambia, y ningún techo, estado ni orden de comprobación cambia. Una reserva bajo contención puede reintentar el intercambio, siempre un número finito de veces mientras otros avanzan.

**Revisión.** Si `no_loss` sigue fallando en el cierre de K7: entonces hay que medir con la traza de rechazos en la imagen de medida qué recurso rechaza. Si la lectura previa al intercambio sube `scale.ipc` con cuatro pares más de lo que la separación de líneas baja. Si un consumidor deriva linajes que cruzan más de cuatro ámbitos, en cuyo caso la memoria deja de cubrirlos, aunque sigue siendo correcta.

**Referencias.** [Concurrencia](../architecture/concurrency.md), [Recursos](../architecture/resources.md), [ADR-004](ADR-004-work-accounting.md), [ADR-010](ADR-010-k6-parameters-and-wake-policy.md), [OQ-04](../roadmap/open-questions.md), [la campaña de rendimiento](../evidence/k6-performance-campaign.md), [estado actual](../roadmap/current-state.md).
