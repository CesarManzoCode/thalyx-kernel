---
id: ADR-022
kind: decision
status: accepted
---
# Las consultas de versión y de reloj tienen un camino propio con una sola lectura del contador, y la carga del kernel vuelve a caber en las páginas del baseline

**Problema.** El segundo cierre de K7 (`build/k7/check-20261028T155106`, kernel en `2f7e14f`, Thalyx en `3ac575f`) dejó 27 criterios en `FAIL`. Dos tienen un mecanismo en el kernel que este cambio puede atacar sin tocar ningún parámetro:

- `k7.k6_entry_version_0`: la razón nativo / `linux-nomitig` es 0,996 [0,913, 1,080]. El brazo nativo midió 60 ns en cinco de seis rondas. En el baseline midió 50 ns en diez de doce.
- `k7.size_kernel_load`: 577 536 bytes, y el criterio pide menos de 573 440. Faltan dos páginas.

**Evidencia.** Observado en las muestras crudas (`samples.json`) y en el ELF del árbol medido:

- Cada muestra es una diferencia de `rdtsc` cuantizada en pasos de 39 ciclos, 10 ns a 3,899 GHz. Lo que mueve la mediana de una ronda es qué fracción de muestras cae en el paso de 195 ciclos (50 ns) y cuál en el de 234 (60 ns). En `2f7e14f` (`build/k6/campaigns/fx-ref`, dos rondas nativas), la consulta de versión cae un 49,5 % en 195 y un 50 % en 234. En `k7-base-a` caía entre un 54 % y un 63 % en 195 en cinco de las seis rondas.
- El camino de la consulta de versión no tiene trabajo propio. Toma el hilo, lo cuenta, deja dos registros, mira la salida y vuelve. La salida (`on_kernel_exit`) siempre leía el contador para comparar la reserva en curso con su fin.
- En `.text` de `2f7e14f`, `api::begin_response` aparece copiada en línea en 21 llamadores, 4 341 bytes en total (DWARF, `DW_TAG_inlined_subroutine`). `sync::STATS` ocupa 0x1400 bytes: 80 bloques de 32 bytes de contadores, cada uno en su propia línea de 64. El texto usaba hasta `0x4549f` y los datos hasta `0x8c819`. Para perder una página cada uno había que quitar 1 183 bytes de texto y 2 073 de datos.

Medido con campañas K6 solo del brazo nativo, seis rondas de `entry.version` y `entry.clock`, con los tamaños de muestra del baseline. Las campañas están en `build/k6/campaigns/fx-e*`. Cada fila añade su cambio a los anteriores:

| Campaña | Cambio | Versión, rondas en 50 ns | Reloj, fracción en 195 ciclos |
| --- | --- | --- | --- |
| `fx-e1b` | La versión no lee el contador a la salida; el reloj compara su propia lectura | 6/6 (≈27 % ya en 156) | ≈0,19–0,34 |
| `fx-e2` | Camino propio, sin la llamada al despachador general | 6/6 (≈48 % en 156) | ≈0,15–0,42 |
| `fx-e3` | El reloj lee el contador antes de nada | 6/6 | ≈0,48–0,52 |
| `fx-e4` | Sin `cld` en la entrada de `syscall` | 6/6 | ≈0,49–0,52 (50 ns en 4/6) |
| `fx-e5` | *Descartado:* el contador se lee en el stub de ensamblador, antes de `swapgs` | 6/6 (una en 40) | ≈0,38–0,45 (60 ns en 6/6) |

**Elección.**

- *Un camino propio para las dos consultas.* `syscall::handle` despacha `VERSION_QUERY` y `CLOCK_QUERY` a `quick`, que está en línea en `syscall_dispatch`. El resto de entradas va a `handle_entry`, fuera de línea, que es el manejador de siempre. `quick` hace la misma contabilidad que antes: el hilo actual, la confirmación de anillo 3, `syscalls` y la parada. `note_entry_of` mira en línea si la confirmación ya está hecha. El anuncio de la primera entrada pasa a una función fría.
- *La salida sabe de dónde sale el contador.* `sched::on_kernel_exit_with(cpu, ExitClock)` hace en línea las cuatro lecturas de la salida que no tiene nada que hacer, y llama a `kernel_exit` (fuera de línea, el cuerpo de siempre y en el mismo orden) solo si alguna tiene trabajo. `ExitClock::Read` lee el contador como antes y es lo que usan todas las demás entradas, las interrupciones y las excepciones. `ExitClock::At(c)` compara con una lectura que la entrada ya tomó: la consulta de reloj usa la que responde (`time::monotonic_ns_and_counter`, que tomó como primera instrucción del camino). `ExitClock::Tick` no mira: lo usa la consulta de versión. Una reserva que se agota durante ella termina en el siguiente tick, igual que con código que no entra al kernel.
- *Sin `cld` en la entrada de `syscall`.* `IA32_FMASK` ya limpia DF (`SYSCALL_FLAG_MASK`). La entrada de interrupciones lo conserva, porque una puerta de interrupción no lo limpia.
- *`begin_response` fuera de línea.* Cada operación que responde la llama una vez, y la llamada queda al lado de un bucle que limpia el cuerpo.
- *Las estadísticas de cerrojos, por procesador.* Cada procesador tiene sus diez bloques en líneas propias (`CpuLockStats`, `align(64)`), y dos clases del mismo procesador pueden compartir línea. Solo ese procesador escribe sus bloques, así que esa línea nunca pasa a otro. El motivo de separarlos, que no se pase una línea entre procesadores en cada adquisición, se mantiene.

**Autocomprobación.** `k7.size_kernel_load` mide la carga sobre el ELF de la imagen de medida. Compilado aquí da 569 344 bytes: texto `0x45000` (usado hasta `0x448af`), datos y `.bss` `0x3b000`. La puerta K6 compara la respuesta de cada entrada en cada ronda, y `k7.behavior_gates` exige K1–K5 tres veces. Esas puertas verían una consulta que respondiera otra cosa o una salida que dejara de honrar una parada.

**Diseñado y comprobado solo en parte.**

- Las campañas `fx-e*` son del brazo nativo solo y de dos entradas, y no son el cierre. La razón con Linux la calcula el cierre.
- `entry.clock` queda en el borde. Con `fx-e4`, cada ronda reparte sus muestras casi a medias entre 195 y 234 ciclos. `k7.k6_entry_clock_0` pide las seis rondas en 50 ns, porque una sola en 60 sube el extremo alto del intervalo por encima de 0,5479. Este cambio no lo garantiza.
- La diferencia restante entre las dos consultas es de unos 40 ciclos, del orden de lo que cuesta un `rdtsc`. Que adelantar la lectura dentro de `quick` ayude y adelantarla al stub perjudique es un hecho medido sin explicación.
- `fx-e5` se descartó por esa medida. Lo mismo con `fx-e6` (`quick` fuera de línea) y `fx-e7` (`leave_if_stopped` fría): ninguno movió las fracciones más allá de la variación entre arranques. Con este tamaño de efecto, la disposición del código pesa tanto como las instrucciones.

**Medido en el cierre.** `build/k7/check-20261028T170958`, con este cambio sobre `2f7e14f` y Thalyx en `3ac575f`:

- Las puertas K1–K5 pasan tres veces, la puerta K6 da 24/24 con autocomprobación 27/27, y las 7 pruebas de EXP-13 pasan.
- `k7.size_kernel_load` pasa, con 569 344 bytes.
- La consulta de versión cae en 50 ns en las seis rondas. En cinco, entre un 41 % y un 46 % de sus muestras cae en 40 ns. En la otra, ninguna cae en 40 y un 46 % cae en 60. Linux cae en 60 en las seis. La razón es 0,83352 [0,83347, 0,83358], y `k7.k6_entry_version_0` no pasa. Su umbral, 0,833494, es exactamente 50,01 / 60, lo que da una ronda de 195 ciclos contra 60 ns, y la desigualdad es estricta. Pasar pide al menos cinco de seis rondas con más de la mitad de las muestras en 156 ciclos, unos cinco ciclos menos que ahora.
- La consulta de reloj cae en 60 ns en cinco rondas, con entre un 47 % y un 49 % de las muestras en 195 ciclos, y en 70 en la sexta. `k7.k6_entry_clock_0` no pasa.

**Alternativas descartadas.**

- *Un reloj sin entrada, al estilo vDSO:* el protocolo emparejado de `entry.clock` es una entrada al kernel, y el brazo Linux hace `syscall(SYS_clock_gettime)`. Cambiaría lo que se compara.
- *Dejar de mirar la reserva en todas las salidas:* las entradas que copian, bloquean o escriben registros pueden durar milisegundos. K3 midió cargos de 4,5 ms sin esa mirada.
- *Achicar `REGIONS` (12 KiB) o una tabla de `Machine`:* son capacidades del protocolo de arranque y del contrato de recursos, no relleno.

**Consecuencias.** Una consulta de versión ya no termina una reserva agotada a la salida. La termina el tick, como en el cómputo puro. Ningún programa la llama en un bucle: solo `user/rt/src/lib.rs` y `user/native/src/boot.c`, al arrancar. El exceso se sigue midiendo y descontando, como dice [Recursos](../architecture/resources.md). Una consulta de reloj compara la reserva con el instante que responde, unas decenas de nanosegundos antes de la salida. Ningún parámetro, techo, interfaz ni registro cambia. `begin_response` es una llamada.

**Revisión.** Si `k7.k6_entry_version_0` vuelve a 60 ns en el cierre con este código, la diferencia no está en el camino de la entrada. Hay que mirar la disposición: `CpuState` mide 0x2f0 bytes, y sus campos calientes caen en líneas distintas según el procesador. Si la carga del kernel vuelve a pasar de 569 344 bytes, la holgura de los datos es de unos 480 bytes y la del texto de unos 1 870.

**Referencias.** [ADR-014](ADR-014-exact-scope-reservations-and-lineage-walks.md), [ADR-018](ADR-018-image-size-stripped-modules-folded-kernel-small-loader.md), [ADR-020](ADR-020-auditor-settles-after-handoff-and-scope-walks-out-of-line.md), [Recursos](../architecture/resources.md), [ABI](../architecture/abi.md), [estado actual](../roadmap/current-state.md).
