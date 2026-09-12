---
id: EVD-009
kind: evidence
status: observed
---
# K6 — Comparación, endurecimiento y primera referencia: qué se midió y qué dice

K6 es la primera fase que mide. Todo lo anterior demostró que un mecanismo existe y hace lo que su contrato dice; esta nota dice cuánto cuesta, comparado con Linux sobre la misma máquina virtual y desde la misma fuente, y qué se encontró al medir. La disciplina es la de [EXP-12](../validation/experiments.md): muestras, condiciones e intervalos, y ninguna conclusión de velocidad sin equivalencia. Lo que una comparación puede concluir lo decide [el esquema](../../abi/schema/k6-bench-v1.json) antes de que nada corra, benchmark por benchmark: *equivalente* (el mismo contrato en ambos lados; una diferencia cuyo intervalo excluye el uno se enuncia), *comparable* (la misma pregunta contestada con garantías distintas, listadas; se informa el coste de cada primitiva con las suyas, nunca que un kernel es más rápido), *distinto* (dos mecanismos; ninguna razón se interpreta) y *solo nativo*.

**Medir no es demostrar.** Un número del invitado no prueba que el invitado hiciera nada. En el lado nativo cada muestra es una nota `user.note` que el kernel estampa con el dominio que la escribió; las entradas que un benchmark de latencia cronometró las cuenta el kernel en el contador de llamadas del hilo; los registros de traza que el kernel retuvo los declara su propio resumen. La puerta se decide desde eso.

## Estado

| Parte | Qué establece | Estado |
|---|---|---|
| Protocolos emparejados | Diecisiete benchmarks de una sola fuente C —`tests/k6/bench.c`— sobre dos backends: este kernel, con `plat_native.c`, un supervisor propio y el motor de K5; y Linux como invitado de la misma máquina, con `plat_linux.c` y el motor de Thalyx sin cambios. El plan de cada arranque, el orden de las entradas y las notas los fija el esquema, del que se generan la cabecera C de los dos lados y el módulo Rust del supervisor. | **Ejecutado** |
| Medidas reproducibles | Una campaña son rondas; una ronda es un arranque de cada brazo en orden aleatorio con el mismo plan, sobre la misma máquina QEMU —q35, `qemu64` con SMEP/SMAP/x2APIC, cuatro procesadores, 1 GiB, OVMF— en KVM, con el proceso confinado a un hilo por núcleo físico. Se conservan plan, digest de imagen o initramfs, línea de órdenes, registro crudo, inventario del anfitrión y revisión. | **Ejecutado**: seis rondas, tres brazos |
| EXP-12 | Rendimiento y escalabilidad con controles emparejados. La unidad de emparejamiento es la ronda; los intervalos son bootstrap sobre rondas; dentro de un arranque, la mediana, con p95 y p99 solo con muestras suficientes. | **Ejecutado**, resultado abajo |
| EXP-11, parte K6 | La misma pregunta, de una sola fuente, al motor sobre los dos backends, con la respuesta contrastada byte a byte contra la referencia de Thalyx en Linux en cada repetición. | **Ejecutado** |
| Cuellos y optimización | Ocho cuellos encontrados midiendo, cada uno arreglado y vuelto a medir, con la cifra de antes y de después. | **Ejecutado** |
| Revisión de parámetros V0 | Cinco parámetros medidos; cuatro cambiados y uno mantenido con su trade registrado. | **Ejecutado**: [ADR-010](../decisions/ADR-010-k6-parameters-and-wake-policy.md) |
| Hardware físico acotado | El inventario que [OQ-05](../roadmap/open-questions.md) pide, de la única máquina física que hay: la de desarrollo, bajo la que corre KVM. No se arrancó este kernel sobre ella. | **Inventario hecho; arranque no** |
| Puerta K6 | 19 criterios decididos por separado desde los artefactos de la campaña más las cinco regresiones, con autocomprobación de 27 daños. | **PASS** |

## Cómo se midió

**Un plan, dos backends.** `tools/run_k6.py` escribe para cada arranque un plan —qué entradas, en qué orden, cuántas muestras, cuánto calentamiento— con el orden sorteado por ronda desde la semilla de la campaña. El plan entra en el paquete nativo como módulo y en el initramfs de Linux como archivo; son los mismos bytes, empaquetados por la misma función de disposición que genera las dos cabeceras. Los brazos de Linux omiten lo que el esquema declara solo nativo. La calibración del contador de tiempo la hace cada backend contra su propio reloj monótono y la informa; en la campaña de referencia las dieciocho calibraciones difieren en menos de un 0,03 %.

**Tres brazos.** `native`, `linux` con las mitigaciones que ese kernel trae por defecto, y `linux-nomitig` con `mitigations=off`: este kernel no implementa ninguna mitigación de ejecución especulativa, y una comparación tiene que sobrevivir al mejor argumento del rival. El invitado Linux es el kernel del anfitrión, `7.2.4-arch1-2`, copiado y digerido, no construido aquí; su init es el mismo `bench.c` compilado con las banderas de máquina del target nativo y enlazado estáticamente contra glibc; su motor es `engine/thalyx-engine.cpp` de la revisión fijada de Thalyx, enlazado desde los mismos objetos con los que se construye la referencia de K5. Cada invitado informa su versión, su línea de órdenes y la vista de vulnerabilidades de su propio kernel, y la puerta comprueba que el brazo sin mitigaciones lee `Vulnerable` donde el otro lee `Mitigation`.

**Qué mide cada uno.** Siete primitivas —dos entradas, llamada IPC con 0/64/256 bytes y con 1/4 capacidades, mapear y desmapear 1/64 páginas, sellar, derivar—; el despertar de un hilo; la aplicación de un presupuesto del 25 %; el rendimiento de cómputo y de IPC con 1 a 4 hilos o pares; el cierre de una unidad de trabajo; la carga, la inferencia y la cancelación del motor; y, solo en el lado nativo, el coste de la profundidad de linaje en la admisión y el del auditor por lote de recibos. Lo que cada lado hace, y qué garantías tiene cada primitiva, está escrito en el esquema al lado del benchmark; es la única fuente de esa descripción.

**El lado nativo, desde dentro.** El supervisor `user/k6super` construye un dominio cliente que corre el plan, un dominio servidor con un hilo por cada uno de cuatro endpoints, y —cuando el plan pregunta al motor— el motor de K5 con el constructor de K5, con su presupuesto a un procesador entero y sin notas por petición. Mientras el plan corre, el supervisor es el auditor: cada admisión reserva un recibo y alguien tiene que leerlo, y ese coste es del perfil auditado y se mide (`audit.drain`) en vez de esconderse. Las entradas que el cliente no tiene autoridad para correr —un ámbito con presupuesto propio, un dominio construido para pararlo, el cierre de un trabajo mientras el motor calcula— se le entregan al supervisor por la página compartida, en el orden del plan, y sus muestras caen entre el BEGIN y el END de la entrada.

## Lo que medir encontró, en orden

Se midió antes de tocar nada. La primera sonda nativa —32 entradas, una décima del tamaño— tardó 58,9 s, de los que **31,5 s** fueron el plano de diagnóstico escribiendo 11 424 registros por el puerto serie: bajo KVM cada byte son dos salidas del invitado y un registro de 160 bytes cuesta 2,7 ms. Una llamada IPC escribía cuatro (`ipc.admitted`, `ipc.delivered`, `ipc.replied`, `ctrl.receipt`) y leía **20 ms**. Ese fue el primer cuello, y los siete siguientes solo aparecieron después de quitarlo. Cada uno con su medida de antes y de después, sobre KVM, en la misma sonda:

| Cuello | Cómo se vio | Qué se cambió | Antes → después |
|---|---|---|---|
| Registros de traza por operación | `diag.summary`: 31,5 s de 58,9 escribiendo | Dos clases de registro; `TRACE_OFF` en el módulo supervisor; retenidos y declarados (541 216 en la misma sonda) | `ipc.call` 20 ms → 2,5 µs; `cap.derive` 3,8 ms → 0,63 µs; `audit.drain` 8,1 ms → 1,2 µs |
| Ranuras de dominio y ámbito nunca recicladas | `closure.unit` y `quota.share` sin muestras; `scope_create_domain` con `LIMIT_EXHAUSTED` en el paso 2 tras 16 dominios; luego `scope_create_child` tras 22 ámbitos | Recolección perezosa cuando una creación no halla ranura vacía; las tramas en cuarentena pasan a la cuenta del kernel | 0 cierres → 600 por arranque sin rechazo |
| Presupuesto de raíz de una ventana en cuatro procesadores | `scale.compute` 13 160 → 18 610 de 1 a 4 hilos; `dispatch_refusals` en todos los ámbitos | Raíz y sistema a una ventana por procesador en línea; el cliente y el servidor K6 a cuatro ventanas | 4 hilos: 1,4× → 3,95× un hilo |
| Despertar sin interrupción | `sched.wake` 200 µs contra un tick de 500 µs | Despertar delegado tras 10 µs de gracia si quien despierta no cede; sondeo ocioso de 50 µs antes de parar; la ida y vuelta síncrona se queda en un procesador | `sched.wake` 200 → 12 µs con `ipc.call` en 2,6 µs; despertando siempre con interrupción, `ipc.call` subía a 4,7–8 µs |
| Auditor y log de 64 celdas | `LIMIT_EXHAUSTED` en `ipc.call`, `ipc.caps`, `ipc.lineage`; 949 500 rechazos en `scale.ipc:1`; el supervisor con 447 437 rechazos de despacho por su medio presupuesto | Log de 256 celdas, lote de 16, `LOG_READ` con plazo que espera un lote entero; el auditor duerme en el kernel | `scale.ipc:1` 936 → 2 334 por corte sin rechazos; `audit.drain` 1,2 µs por 4 recibos → 4,2 µs por 16 |
| Cerrojo sin turno y reclamación solo en ocioso | `closure.unit` 7,95 ms con 6 918 intentos de retirada; el hilo muerto seguía `current` en otro procesador | Cerrojo de turno; `SCOPE_RETIRE` y `SCOPE_DRAIN_STATUS` reclaman al entrar, `DOMAIN_TERMINATE` al salir; el dominio ocioso avisa antes de bloquearse | `closure.unit` 7,95 ms → 24,6 µs (barrera 3 µs, terminación 19, retirada 11 en un intento) |
| Reserva de tablas por mapeo nunca devuelta | `mem.map` con 141 rechazos de 250 en la segunda ronda del piloto, después de 200 mapeos | La parte de la reserva que el mapeo no tomó vuelve al instalarlo, como en la construcción de un dominio | 250 mapeos sin rechazo |
| Notas y presupuesto del motor K5 | `engine.infer` 40 ms contra 1,65 ms en Linux; el motor informaba 6,6 ms de tiempo propio | Modo silencioso del motor (`arg2`) para K6; presupuesto de un procesador | `engine.infer` 40 → 1,8 ms; `engine.cancel` 67 → 1,8 ms; `engine.load` 89 → 27 ms |

Dos cosas que este recorrido dice sobre el método. La primera: ninguno de los ocho se habría visto sin medir, y siete de los ocho estaban tapados por el primero. La segunda: dos de los cambios —la lectura del log que espera y el batch de dieciséis— son la revisión de parámetros V0 que la ruta pedía, hecha con la cifra que la justifica y no antes; y un tercero —el plazo de gracia— es un parámetro nuevo cuyo trade se midió en tres valores y quedó registrado con el peor lado enunciado. Está todo en [ADR-010](../decisions/ADR-010-k6-parameters-and-wake-policy.md).

Un cambio que se midió y **no** se conservó: quitar los dos cerrojos que cada llamada al kernel toma en la entrada y la salida, con lecturas sin cerrojo del estado por procesador. `entry.version` siguió en 80 ns. Se revirtió.

## EXP-12: la campaña de referencia

Seis rondas, tres brazos, dieciocho arranques, todos completos, ninguna muestra perdida ni operación rechazada en ninguno. Los tamaños de muestra salen de un piloto de dos rondas con el estimador robusto de `tools/k6_analysis.py`, nunca por debajo de los del conjunto. La mediana es la de todas las muestras del brazo; el intervalo es el bootstrap al 95 % de las medianas por ronda; la razón nativo/Linux es la media geométrica de las razones emparejadas por ronda con su intervalo bootstrap. p99 solo con al menos mil muestras.

Unidades: nanosegundos en las primitivas y el despertar; nanosegundos de ejecución recibida por ventana en `quota.share`; idas y vueltas o trozos completados por corte de 10 ms en `scale.*`; microsegundos en el motor. Entre paréntesis, p99 donde hay al menos mil muestras. La razón es nativo entre Linux, y para un recuento por corte, mayor es mejor.

| Benchmark | Equivalencia | Nativo | Linux | Linux sin mitigaciones | Razón nativo/Linux [IC 95 %] | Veredicto |
|---|---|---|---|---|---|---|
| `entry.version:0` | equivalente | 80 (p99 110) | 60 (p99 70) | 60 (p99 70) | 1.33 [1.33, 1.33]; sin mit. 1.33 [1.33, 1.33] | nativo más lento |
| `entry.clock:0` | equivalente | 90 (p99 110) | 110 (p99 140) | 100 (p99 120) | 0.796 [0.753, 0.818]; sin mit. 0.873 [0.822, 0.9] | nativo más rápido |
| `ipc.call:0` | comparable | 2 691 (p99 13 014) | 6 490 (p99 12 010) | 6 110 (p99 10 980) | 0.407 [0.389, 0.417]; sin mit. 0.428 [0.402, 0.442] | coste bajo garantías distintas |
| `ipc.call:64` | comparable | 2 721 (p99 12 623) | 6 630 (p99 11 380) | 6 290 (p99 12 080) | 0.411 [0.41, 0.412]; sin mit. 0.425 [0.411, 0.433] | coste bajo garantías distintas |
| `ipc.call:256` | comparable | 2 781 (p99 12 703) | 6 690 (p99 12 890) | 6 340 (p99 12 020) | 0.416 [0.413, 0.419]; sin mit. 0.439 [0.435, 0.441] | coste bajo garantías distintas |
| `ipc.caps:1` | comparable | 3 011 (p99 14 083) | 6 930 (p99 13 582) | 6 570 (p99 13 591) | 0.463 [0.436, 0.491]; sin mit. 0.487 [0.457, 0.519] | coste bajo garantías distintas |
| `ipc.caps:4` | comparable | 4 351 (p99 14 381) | 7 280 (p99 13 648) | 6 950 (p99 13 407) | 0.574 [0.532, 0.616]; sin mit. 0.592 [0.547, 0.642] | coste bajo garantías distintas |
| `mem.map:1` | comparable | 8 602 (p99 14 754) | 1 820 (p99 5 110) | 1 780 (p99 5 501) | 4.73 [4.67, 4.79]; sin mit. 4.81 [4.77, 4.86] | coste bajo garantías distintas |
| `mem.map:64` | comparable | 16 974 (p99 26 448) | 37 520 (p99 54 123) | 36 770 (p99 55 109) | 0.451 [0.447, 0.455]; sin mit. 0.459 [0.455, 0.464] | coste bajo garantías distintas |
| `mem.seal:16` | comparable | 17 924 (p99 25 035) | 23 170 (p99 41 960) | 22 040 (p99 33 782) | 0.776 [0.765, 0.79]; sin mit. 0.815 [0.807, 0.83] | coste bajo garantías distintas |
| `cap.derive:0` | comparable | 450 (p99 560) | 140 (p99 140) | 140 (p99 140) | 3.23 [3.21, 3.25]; sin mit. 3.23 [3.21, 3.25] | coste bajo garantías distintas |
| `sched.wake:0` | equivalente | 11 972 (p99 20 027) | 2 710 (p99 14 186) | 2 610 (p99 13 712) | 4.43 [4.41, 4.44]; sin mit. 4.58 [4.54, 4.61] | nativo más lento |
| `quota.share:25` | equivalente | 2 459 276 | 2 600 973 | 2 390 716 | 1.05 [0.95, 1.15]; sin mit. 1.02 [0.945, 1.09] | INCONCLUSIVE |
| `scale.ipc:1` | comparable | 2 352 | 1 516 | 1 608 | 1.55 [1.54, 1.56]; sin mit. 1.46 [1.46, 1.47] | coste bajo garantías distintas |
| `scale.ipc:2` | comparable | 1 701 | 3 031 | 3 211 | 0.562 [0.56, 0.563]; sin mit. 0.529 [0.527, 0.531] | coste bajo garantías distintas |
| `scale.ipc:3` | comparable | 1 597 | 7 410 | 8 498 | 0.227 [0.214, 0.255]; sin mit. 0.202 [0.186, 0.234] | coste bajo garantías distintas |
| `scale.ipc:4` | comparable | 1 806 | 11 912 | 13 726 | 0.151 [0.15, 0.153]; sin mit. 0.131 [0.129, 0.132] | coste bajo garantías distintas |
| `scale.compute:1` | equivalente | 27 834 | 27 973 | 27 976 | 0.995 [0.995, 0.995]; sin mit. 0.995 [0.995, 0.995] | nativo más lento |
| `scale.compute:2` | equivalente | 55 676 | 55 944 | 55 959 | 0.995 [0.995, 0.995]; sin mit. 0.995 [0.995, 0.995] | nativo más lento |
| `scale.compute:3` | equivalente | 83 506 | 83 921 | 83 940 | 0.995 [0.995, 0.996]; sin mit. 0.995 [0.994, 0.995] | nativo más lento |
| `scale.compute:4` | equivalente | 109 968 | 111 918 | 111 905 | 0.982 [0.98, 0.984]; sin mit. 0.982 [0.98, 0.984] | nativo más lento |
| `closure.unit:0` | distinto | 28 421 (p99 3 428 741) | 48 280 (p99 91 684) | 47 705 (p99 93 510) | 0.59 [0.57, 0.612]; sin mit. 0.595 [0.581, 0.613] | sin conclusión de velocidad |
| `engine.load:0` | comparable | 30 948 | 3 082 | 3 207 | 9.94 [9.2, 10.4]; sin mit. 9.9 [9.52, 10.3] | coste bajo garantías distintas |
| `engine.infer:0` | equivalente | 1 800 | 1 630 | 1 630 | 1.1 [1.1, 1.11]; sin mit. 1.1 [1.09, 1.11] | nativo más lento |
| `engine.infer:1` | equivalente | 1 440 | 1 306 | 1 306 | 1.1 [1.1, 1.11]; sin mit. 1.1 [1.1, 1.11] | nativo más lento |
| `engine.infer:4` | equivalente | 6 566 | 5 852 | 5 863 | 1.11 [1.09, 1.13]; sin mit. 1.11 [1.09, 1.12] | nativo más lento |
| `engine.cancel:0` | distinto | 7 298 | 4 690 | 4 688 | 1.57 [1.54, 1.6]; sin mit. 1.57 [1.53, 1.61] | sin conclusión de velocidad |
| `ipc.lineage:0` | solo nativo | 2 691 (p99 13 084) | — | — | — | — |
| `ipc.lineage:8` | solo nativo | 2 731 (p99 13 154) | — | — | — | — |
| `ipc.lineage:16` | solo nativo | 2 781 (p99 12 853) | — | — | — | — |
| `ipc.lineage:28` | solo nativo | 2 861 (p99 13 164) | — | — | — | — |
| `audit.drain:0` | solo nativo | 6 302 (p99 13 323) | — | — | — | — |

**Lo que la tabla permite decir, y solo eso.**

- *Entradas.* La entrada sin descriptor cuesta 80 ns contra 60 en Linux: nativo más lento, intervalo cerrado. La lectura de reloj por entrada, 90 contra 110: nativo más rápido. Las dos son equivalentes y las dos diferencias son reales y pequeñas.
- *IPC.* Una llamada con respuesta cuesta 2,7 µs con cero, 64 o 256 bytes, contra 6,5–6,7 en Linux por socket `SEQPACKET` con credenciales. Son *comparables*, no equivalentes: la nativa estampa origen, ámbito, faceta y padre causal, reserva y escribe un recibo que un auditor drena, y devuelve un objeto de respuesta de un solo uso; la de Linux adjunta pid, uid y gid. La razón 0,41 [0,39, 0,42] es el coste de la primera con sus garantías contra la segunda con las suyas, y la puerta rechaza leerla como «más rápido». Con capacidades, 3,0 y 4,4 µs por 1 y 4 contra 6,9 y 7,3.
- *Memoria.* Mapear una página cuesta 4,7 veces lo que `mmap` —crear el objeto con sus páginas puestas a cero y cobradas, mapear, desmapear con acuse de todos los procesadores, cerrar—, y mapear 64 cuesta 0,46 veces lo que `mmap` con toque de cada página. Sellar 16 páginas, 0,78. Derivar una capacidad cuesta 3,2 veces un `dup`: 450 ns contra 140. Todo comparable, con las diferencias en el esquema.
- *Despertar.* 12,0 µs contra 2,7: nativo más lento, 4,4×, equivalente. Es el plazo de gracia de [ADR-010](../decisions/ADR-010-k6-parameters-and-wake-policy.md), elegido para que la ida y vuelta síncrona no se parta; a 3 µs de gracia el despertar bajaba a 5,4 µs y la llamada subía a 6,9. Se enuncia como está.
- *Presupuesto.* Un cuarto de ventana entrega el 24,6 % de cada ventana en nativo, con un intervalo entre rondas casi nulo, y el 26,0 y el 23,9 % con `cpu.max` en los dos brazos de Linux, con más varianza; los intervalos de las razones incluyen el uno y el veredicto es INCONCLUSIVE, que es lo que un estadístico de cuota puede decir con seis rondas.
- *Escala.* El cómputo independiente escala igual en los dos: 3,95× con cuatro hilos en nativo y 4,0× en Linux, con nativo un 0,5–1,8 % por debajo, intervalo cerrado —el tick de 2 kHz y su contabilidad—. **El IPC no escala en nativo**: 2 352 idas y vueltas por corte de 10 ms con un par y 1 806 con cuatro, contra 1 516 y 11 912 en Linux. Con un par el nativo está por encima, 1,55×; con cuatro, a 0,15. Es el cerrojo único de la máquina que el esquema declara como diferencia, y es el límite de escalabilidad más importante que K6 mide. No se cambia en K6; [OQ-04](../roadmap/open-questions.md) lo recoge con la cifra.
- *Cierre.* 28 µs de mediana desde la barrera hasta la retirada aceptada, con la barrera alcanzando todo lo derivado bajo el ámbito, contra 48 µs de `SIGKILL` y `waitpid`. Distinto; sin conclusión. La cola nativa es larga: el p99 es 3,4 ms, uno de cada quince cierres, y no está diagnosticado.
- *Motor.* La misma pregunta, de la fixture de K5, con la respuesta idéntica byte a byte a la referencia de Thalyx en las 18 × 3 entradas y en cada repetición: 1,80, 1,44 y 6,57 ms contra 1,63, 1,31 y 5,85 en Linux, nativo un 10 % más lento con intervalo cerrado —la libc, el asignador y la biblioteca matemática son de aquí—. Cargar el motor cuesta 31 ms contra 3,1: comparable, porque uno copia los pesos por la interfaz a memoria que su ámbito paga y el otro los mapea. Cancelar cuesta 7,3 ms hasta la siguiente respuesta de otro llamante con el motor residente, contra 4,7 ms matando y volviendo a cargar; son mecanismos distintos y no hay conclusión, pero la petición cancelada es de verdad —cuatrocientos tokens bajo una gramática que nunca acepta el fin— y el que pregunta ve `CANCELLED`.
- *Solo nativo.* La admisión con una faceta derivada 0, 8, 16 y 28 pasos más allá cuesta 2,69, 2,73, 2,78 y 2,86 µs: unos 6 ns por paso de linaje. Drenar un lote de dieciséis recibos cuesta 6,3 µs al auditor. [OQ-01](../roadmap/open-questions.md) queda contestada en su parte medible.

**Lo que la campaña no dice.** El estimador recomienda 42 rondas para que las medianas de `scale.ipc` con tres y cuatro pares queden a un 5 % entre arranques; se corrieron seis, y los intervalos de esas dos filas lo reflejan. Ningún número aquí es una medida sobre hardware físico. Las dos ejecuciones de Linux difieren entre sí en las primitivas de entrada y en IPC en un 5–6 %, que es lo que las mitigaciones cuestan en este anfitrión; este kernel no paga nada equivalente porque no mitiga nada, y eso es una diferencia de garantías, no de velocidad.

## Segunda fase: partir el cerrojo (`perf/ipc-scalability`)

Lo que la campaña de referencia dejó dicho —que el IPC no escala con los pares
y que el cerrojo único de la máquina es la causa— se atacó después de ella. Lo
que sigue son medianas de tres rondas en el mismo anfitrión y la misma
configuración, sobre `8a7eeb1`, con las puertas K1–K5 en verde bajo TCG; el
brazo de Linux es el de la campaña de referencia. Tres rondas no son las 43 que
el estimador pide para las filas de IPC con más pares, y las cifras se dan con
su coeficiente de variación por ronda para que se vea lo que sí sostienen.

| Fila | Antes (`5aa86a2`) | Ahora | cv | Linux | Veredicto |
|---|---|---|---|---|---|
| `entry.clock` | — | 60 ns | 0,0 % | 110 | gana 1,83× |
| `ipc.call:0` | 2 352 idas/corte | 2 061 ns | 0,2 % | 6 980 | gana 3,39× |
| `ipc.caps:4` | — | 3 051 ns | 0,5 % | 7 400 | gana 2,43× |
| `mem.map:1` | — | 1 940 ns | 0,2 % | 1 830 | **pierde 0,94×** |
| `mem.map:64` | — | 10 043 ns | 0,0 % | 37 550 | gana 3,74× |
| `mem.seal:16` | — | 10 803 ns | 0,2 % | 23 110 | gana 2,14× |
| `cap.derive` | — | 440 ns | 0,0 % | 140 | **pierde 0,32×** |
| `sched.wake` | — | 1 175 ns | 1,1 % | 2 740 | gana 2,33× |
| `scale.ipc:1` | 2 352 | 4 608 | 0,1 % | 1 511 | gana 3,05× |
| `scale.ipc:2` | 1 701 | 6 362 | 0,5 % | 3 006 | gana 2,12× |
| `scale.ipc:3` | 1 597 | 7 797 | 0,2 % | 7 410 | gana 1,05× |
| `scale.ipc:4` | 1 806 | 8 518 | 0,1 % | 11 914 | **pierde 0,71×** |
| `closure.unit` | — | 12 543 ns | 1,2 % | 49 940 | gana 3,98× |
| `engine.load` | 30 890 µs | 6 343 µs | — | 3 499 | **pierde 0,55×** |
| `engine.infer:0` | — | 1 792 µs | — | 1 642 | **pierde 0,92×** |
| `engine.cancel` | 7 174 µs | 13 414 µs | 2,6 % | 4 936 | **pierde 0,37×** |
| `quota.share:25` | — | 2 483 404 ns | — | 2 443 152 | pierde 0,98× |
| `scale.compute:4` | — | 110 715 | — | 111 870 | pierde 0,99× |

**Lo que se hizo.** El cerrojo de la máquina es ahora un cerrojo de lectores por
procesador. Las cuatro operaciones del viaje de ida y vuelta —admitir, recibir,
responder, cerrar el ticket— lo toman compartido y se sirven de un cerrojo por
registro: canal, invocación, celda de mensaje, tabla de capacidades de un
dominio, nodo de grant, log. El plano de control lo sigue tomando en exclusiva.
`vault/architecture/concurrency.md` recoge el orden y las tres reglas que lo
hacen posible.

**Lo que costó llegar.** La primera versión del reparto medía *peor* que el
cerrojo único —6 411 idas y vueltas con cuatro pares contra 7 344—, con un 40 %
menos de espera. Un cerrojo se toma y se suelta con dos operaciones atómicas
sobre una línea que dos procesadores se pasan, y el viaje de ida y vuelta
llegó a tomar cuarenta. Lo que hizo que el reparto pagara fue bajar esa
cuenta —un recorrido de linaje en la misma toma que leyó el nodo, una toma por
liberación de invocación en vez de cinco, el recibo decidiendo en su propia
toma si alguien espera— y cambiar el turno por un intercambio en los cerrojos
cortos. Queda un coste que no se ha recuperado: las filas de un solo par pagan
entre un 5 y un 17 % —`ipc.call:0` 1 920 → 2 061 ns, `cap.derive` 370 → 440,
`mem.map:1` 1 810 → 1 940, que es la fila que el reparto convirtió en derrota.

**Dónde está ahora el límite, medido.** Con el reparto hecho, la espera por
cerrojos es el 4 % del tiempo de máquina. Lo que limita `scale.ipc:4` es la
**contabilidad de ámbito por la cadena de ancestros**: operaciones atómicas de
lectura-modificación-escritura sobre líneas que todos los procesadores
comparten, recorridas de la hoja a la raíz. Neutralizándola entera —sondas
desechadas, nunca integradas— las cuatro filas dan 4 931, 7 315, 9 536 y
**11 070**, contra 8 518 ahora y 11 914 en Linux. Por partes, cada una medida
sola con cuatro pares: las reservas y devoluciones de `QueueBytes` y
`Metadata` valen un 12,5 %; los contadores de simultaneidad por despacho un
11,4 %; el resto es el cargo de ejecución. **Aun con toda ella gratis la fila
seguiría por debajo de Linux**, y hacerla barata sin debilitar el contrato
—cada contador con techo comprobado necesitaría reservas por procesador, y una
reserva por procesador permite rechazar a un hermano mientras queda sitio— es
el problema de diseño que [OQ-04](../roadmap/open-questions.md) deja abierto.

**`engine.cancel`.** La fila pasó de 7 174 a 13 414 µs dentro de este sprint.
Bisecada: el cambio es `d96804e`, que hace que el runtime de K6 no narre su
propio arranque, y **ese commit no toca una línea del kernel**. La fila mide una
cancelación corriendo contra una inferencia; arrancar el motor más rápido
cambia cuál de las dos gana la carrera. No es una regresión del camino de
cancelación, y `engine.load` —la fila que ese mismo commit buscaba— bajó de
30 890 a 6 343 µs.

## Hardware físico acotado

`tools/inventory_host.py` registra la máquina física bajo KVM tal como se describe a sí misma, sin privilegios: procesador, banderas de interés, topología SMT, gobernador, vulnerabilidades del anfitrión, memoria, firmware y DMI, grupos IOMMU con sus dispositivos, dispositivos de bloque con su caché de escritura y FUA, módulo KVM y sus parámetros. La campaña lo guarda junto a sus resultados. Es el inventario que [OQ-05](../roadmap/open-questions.md) pide y no es un arranque: este kernel no ha corrido sobre esa máquina fuera de KVM, y ninguna cifra de esta nota dice lo contrario.

## Qué decide la puerta, y desde dónde

`tools/check_k6.py` lee la campaña —`campaign.json`, `samples.json`, `results.json`, los registros crudos de cada arranque, el inventario— y decide 19 criterios por separado más las cinco regresiones: que cada ronda arrancó cada brazo y cada plan llegó a su fin; que los dos lados corrieron el mismo plan menos lo solo nativo, con la misma semilla; que las muestras nativas son notas de dominios que el kernel construyó por el camino K2, con el módulo supervisor pidiendo solo resúmenes y `diag.summary` declarando lo retenido; que el kernel contó al menos una entrada por muestra en el hilo del cliente; que nada se perdió ni se rechazó; que el motor contestó la referencia en cada repetición y la referencia está etiquetada como ejecución del anfitrión; que los contadores coinciden; que cada invitado Linux es el kernel registrado con las mitigaciones que su brazo declara; que cada veredicto es uno que la equivalencia permite y una diferencia de velocidad tiene intervalo que excluye el uno; que cada comparación emparejó todas las rondas; que un solo kernel, un solo juego de módulos y una sola imagen de Linux corrieron la campaña, en KVM, con digests; que el anfitrión está inventariado; que cuatro procesadores rinden al menos 3,5× uno; que un cuarto de ventana entrega entre el 20 y el 30 %; que se cerraron más unidades que ranuras hay; que lo solo nativo no se comparó; que cada resultado es un benchmark declarado; y que el auditor no perdió recibos ni llenó el log. La autocomprobación aplica 27 daños —un arranque de menos, un orden distinto, la traza puesta, un digest ajeno, un veredicto de más, un reloj un 5 % desviado, un kernel de más, un inventario sin procesador…— y cada uno lo nota el criterio que lo nombra.

## Lo que K6 **no** demuestra

- **Nada sobre hardware físico.** KVM ejecuta las instrucciones del invitado en el procesador físico, pero los dispositivos, el firmware, los controladores de interrupciones y cada salida del invitado son del emulador y del anfitrión. Una interrupción entre procesadores cuesta aquí lo que cuesta salir a KVM; en silicio costaría otra cosa y el plazo de gracia se revisaría.
- **Nada sobre escalabilidad del IPC.** Se midió que no escala y por qué; no se arregló, porque arreglarlo es fragmentar el cerrojo de la máquina y eso es un rediseño con su propia evidencia, no un ajuste.
- **Ninguna superioridad.** Doce de los diecisiete benchmarks son comparables o distintos por construcción. Donde hay equivalencia, el nativo es más rápido en una lectura de reloj y más lento en la entrada desnuda, en el despertar, en el cómputo por décimas de porcentaje y en la inferencia por un 10 %; y lo dice.
- **Ni el tamaño de muestra que el propio estimador pide** para las dos filas de IPC con más pares.
- **Ni un perfil de Linux ejecutado**: lo que corre de Linux es su kernel y su motor como invitados, etiquetados como el otro lado de una comparación. Los límites de K3, K4 y K5 se heredan enteros.

## Digests de la campaña de referencia

```text
kernel.elf                     c98498727d663623e89cac7ab7b546dea704f12c251fc23059d7ba082e88aa9a
k6super.elf                    10083adc3f3fa69dff8d48455a9665287cd290db899ecae44281a3637a9cfaf3
nbench.elf                     b2a6d48f327cc8e36e20a78ef715f51c43851706ac444301baf779f2557b2163
nengine.elf                    615ea6f04933bd2050df9afeb502480a9921be3f172f232eb488807362c10645
tiny.gguf (ambos lados)        1b726566329f3aaca2f6d89ee0ec4efc31986936628ca6d68591077c857d3267
vmlinuz 7.2.4-arch1-2          ffcb6e464b3d4c4182fac41be54e7c6ec4487eabd73a0d2133756cbb38d3a85f
lbench (init del invitado)     b7a14d146ec79d65919cd194aee052bb7dcd62d7530bc8c028bc5734a53b598b
thalyx-engine (estático)       7e725bec9a84196d73423a366a6a95cbf7c2a4080fbb88bd3c95fa075dadac4d
base.cpio                      f86820ecf0356fcb81b915e8bc73b64f305127279f0efe3c750120c0bbd2ab4d
```

El anfitrión es un AMD Ryzen 5 5600G, seis núcleos con SMT, KVM AMD con NPT y sin AVIC, QEMU 11.1.1, gcc 16.2.1 y glibc 2.44 para el lado Linux. Cada arranque nativo construye su imagen con el plan de su ronda como módulo; `campaign.json` lleva el digest de cada imagen, y `log-digests.json` el de cada registro crudo. La campaña corrió sobre la revisión `9a83204` con los cambios del commit que la sigue ya aplicados; el kernel y los módulos se reconstruyen byte a byte desde ese commit y sus digests son los de arriba.

Los artefactos de la campaña —`campaign.json` con la línea de órdenes y los digests de cada arranque, `results.json`, `sizes.json`, `host-inventory.json`, el manifiesto de Linux y `log-digests.json`— están copiados en [`k6/baseline/`](k6/baseline/). Los registros crudos de los dieciocho arranques, 15 MB, quedan en `build/k6/campaigns/baseline/` y su digest en `log-digests.json`.
