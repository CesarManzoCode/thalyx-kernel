---
id: ADR-019
kind: decision
status: accepted
---
# El broker de K4 se detiene cuando el supervisor le avisa que no queda ningún cliente, y `release` solo genera la depuración que alguien lee

**Problema.** En el baseline de K7 (`vault/private/baseline-k7.json`), `tools/run_gates.py --platform tcg` tarda 281,1 [275,8, 305] s en la réplica A y 295,5 [292,2, 335,9] s en la B. El build limpio en release tarda 13,74 [13,675, 13,832] s y 13,494 [13,36, 13,648] s.

**Evidencia.** Observado en `build/k7/base-a/logs/gates-0.log` y en `build/k4-cases/` de la última ejecución de las puertas, en el árbol de `ab742e6`:

- `k4-run` se lleva entre 172,8 y 189,0 s de unos 280. Le siguen `k5-cases` (48,7 s), `k5-self-test` (22,3 s) y `k5-stages` (20,2 s). Cada paso de build de una imagen tarda 0,5 s.
- Los 15 casos de K4 suman 25 tramos. Un tramo sin corte tarda unos 6 s de reloj y su último registro está en 1,1 a 1,2 s de reloj del invitado. Los tramos cortados en un punto tardan unos 10 s, y `reorder-commit`, 17,9 s.
- En el tramo de `baseline`, el trabajo de los clientes termina antes de 0,7·10⁹ ciclos. Desde ahí hasta 4,53·10⁹ ciclos el único registro es el `SIGNAL_WAIT` vencido del supervisor, cada 100 ms. El tramo termina cuando el broker sale con `refusals=4`: `run_broker` (`user/k4client/src/main.rs`) espera cuatro `ENDPOINT_RECEIVE` de 1 s seguidos sin intención antes de detenerse. Bajo TCG, esa espera es más de la mitad del tramo.
- El servicio solo llama al broker desde `resolve_outbox`, dentro de la publicación que un cliente está esperando (`user/k4store/src/main.rs`). Ningún otro camino del servicio, tampoco la recuperación, le manda una intención.
- `SIGNAL_WAIT` consume solo los bits de su máscara (`kernel/src/api/evtops.rs`), y `SIGNAL_QUERY` lee los bits sin consumirlos con el derecho `INSPECT`, que el broker ya tiene sobre la señal `DONE`.
- `release` llevaba `debug = true` en todo el espacio de trabajo. El loader se enlaza con `/DEBUG:NONE`, que tira su depuración. Los programas entran al paquete sin depuración ([ADR-018](ADR-018-image-size-stripped-modules-folded-kernel-small-loader.md)), y ningún checker ni herramienta lee DWARF de la copia completa. Los únicos paquetes de fuera del espacio de trabajo son las dependencias del loader y sus macros.

**Elección.**

*El broker se detiene cuando se lo dicen.* `bit::BROKER_STOP` (`user/k4fmt/src/pkg.rs`) es un bit nuevo de la señal `DONE`. El supervisor (`user/k4super/src/main.rs`) lo levanta una vez, en cuanto todos los clientes que no son el broker están muertos o fallados, o antes de salir cuando el servicio pide el corte con `CRASH`, porque el servicio se detiene justo después de pedirlo. El broker espera cada intención en ventanas de 100 ms. Cuando una ventana vence con el endpoint vacío, consulta la señal y, si el bit está, sale como siempre: levanta `CLIENT_DONE`, escribe su `DONE` y termina de forma voluntaria. Sin el bit, sale tras 4 s sin intención, el mismo total que antes. Cuatro rechazos seguidos que no sean un vencimiento lo detienen, como antes. No cambia qué contesta, ni a quién, ni ningún registro del servicio ni del medio.

*Depuración donde se lee.* `release` pasa a `debug = "line-tables-only"`. `thalyx-kernel` conserva `debug = true`. `thalyx-boot-uefi` y todo paquete de fuera del espacio de trabajo se compilan con `debug = false`. La depuración no cambia el código generado ni los segmentos cargables. Al paquete los programas llegan sin depuración igual que antes.

**Autocomprobación.** No hay registro ni checker nuevos. El `domain.terminated` del broker, que la puerta K4 ya lee en `faults`, sigue diciendo `reason=voluntary`. Su `refusals` cuenta ahora las ventanas de 100 ms que vencieron. Un broker que saliera antes de tiempo dejaría sin contestar una intención: el servicio registraría `UNKNOWN` donde `outbox` espera `DELIVERED`, si le tocara a un caso que esa comprobación lee. Si el supervisor no pudiera levantar el bit, escribiría `report::UNEXPECTED` y el tramo volvería a terminar por la espera de 4 s.

**Diseñado, no medido.** Se busca bajar `k7.gates_seconds`, quitando de cada tramo de K4 que termina sin corte casi toda la espera ociosa del broker: unos 3 a 4 s de reloj por tramo bajo TCG. Los tramos donde un cliente queda bloqueado en una llamada al servicio detenido (`cut-after-prepare`, `cut-after-checkpoint`, `tear-object`, `abort-visible`, `kill-service`, `control-lost` y `reorder-commit` en su primer tramo) terminan cuando esa llamada vence a los 8 s, y este cambio no los acorta. Se busca bajar `k7.build_seconds` con menos depuración que generar y enlazar. Cuánto pesa la depuración en el build es una hipótesis: el camino crítico puede ser el kernel, que la conserva. Costos conocidos: el broker hace ahora una llamada al kernel cada 100 ms mientras espera, y deja un `k2.refused` por cada una, así que el tramo escribe algunos registros más por el puerto serie. La copia completa de cada programa trae ahora solo la tabla de líneas. Las cifras de K7 las mide el cierre.
