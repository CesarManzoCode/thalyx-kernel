---
id: EVD-006
kind: evidence
status: observed
---
# K3 — SMP y dispositivos: qué se ejecutó y qué demuestra

Esta nota registra la primera ejecución del kernel en más de un procesador y la primera con un dispositivo que escribe memoria por su cuenta. K2 mostró qué puede hacer un programa sin permisos ambientales; esto muestra qué queda en pie cuando la suposición que sostenía a K2 —un solo núcleo, ningún DMA— deja de ser cierta.

La tentación aquí no es la de K2. En K2 los mecanismos eran numerosos y una ejecución silenciosa invitaba a darlos por buenos. En K3 la tentación es afirmar aislamiento: el vocabulario del contrato de hardware permite escribir «el dispositivo está contenido» sin que nada en esta máquina lo contenga. Esta ejecución **no** tiene unidad de remapeo programada, y todo lo que sigue está escrito para que ese hecho sea imposible de perder de vista.

## Qué se ejecutó

Una tercera imagen UEFI construida desde las mismas fuentes con el mismo script y **el mismo binario de kernel** que las imágenes K1 y K2. Lo único que cambia es el paquete de arranque. Que el kernel sea el mismo es lo que permite decir «K1 y K2 siguen pasando» como afirmación sobre un kernel y no sobre tres.

| Artefacto | Comando | Resultado |
|---|---|---|
| Imagen | `python3 tools/build_image.py --phase k3` | `build/thalyx-k3.img`, manifiesto con digest de cada artefacto y herramienta |
| Ejecución | `python3 tools/run_k3.py` | `exit_status=33` (`k1.terminal status=complete`), 1222 registros, ~2.9 s, `user_faults=2` (los dos provocados) |
| Veredicto | `python3 tools/check_k3.py` | `K3 GATE PASSED: 28 of 28 criteria met` |
| Autocomprobación | `python3 tools/check_k3.py --self-test` | `57 of 57 damaged runs were caught` |
| Control de plataforma | `python3 tools/run_k3.py --profile dmar` | Unidad DMAR **descrita y no programada**: `28 of 28` con `remapping_unit_described=1 remapping_programmed=0` |
| Cobertura | `k2.coverage` en el propio registro | `operations_assigned=59 operations_reached=37`, las 26 que K3 necesita entre las alcanzadas |
| Controles negativos | notas de los cuatro programas | 14 rechazos esperados, ninguno inesperado y ninguno ausente |

Plataforma exacta de la ejecución registrada: QEMU `q35` con acelerador `tcg`, **cuatro procesadores** (`-smp 4`), CPU `qemu64,+smep,+smap,+pdpe1gb,+x2apic`, 512 MiB, OVMF, y una función `virtio-blk-pci` con `disable-legacy=on,disable-modern=off` sobre un disco raw de 4 MiB. Sin `intel-iommu` en el perfil por defecto; con él, y sin programarlo, en el perfil de control.

El runner registra la máquina, el acelerador, el transporte, el respaldo del disco y la presencia de unidad de remapeo en `run.json`, y la puerta compara el número de procesadores configurados contra los que arrancaron: una puerta multiprocesador leída de una ejecución uniprocesador es el error que conviene hacer imposible.

Digests de la ejecución registrada:

```text
image   4f4746ec81bf20daa3dfe7322c17ddc01286ebffce34c796b04019f076d2f263
kernel  b7bfd6de05984b1161848cce2ee8c64c048b2fc7f1c071e32b701363f5a00246
loader  669c530b353b8aba9dfa58147167c6879765b3cbaeb109204c41764a1f7bea83
package f9b2539627e8187555a9b254570a8fc8c0ad5f5dbc6e06dc4652d95acb21840b
```

Los contadores temporales —ticks, preempciones, vueltas de espera, migraciones— **no** son reproducibles, y en SMP lo son todavía menos que en K1 y K2: dependen del reparto que el emulador hace de sus hilos. Ningún criterio de la puerta es una cifra exacta de esas. Lo que sí se compara son relaciones: acuses contra procesadores en línea, comprometido contra presupuesto, exceso contra latencia de preempción medida.

## Qué observó la ejecución

### Cuatro procesadores, y uno que no existe

El firmware describió cuatro procesadores y cuatro quedaron en línea: tres arrancados con INIT/SIPI/SIPI desde un trampolín bajo 1 MiB, uno el de arranque. Cada uno confirmó **la identidad con la que se le llamó** —`identity_match=1`— en lugar de que el handshake se tomara como prueba de identidad: una tabla mal interpretada podría arrancar dos veces el mismo procesador y contarlo como dos. Cada uno calibró su propio timer y subió con SMEP y SMAP puestos.

Después, deliberadamente, se pidió arrancar un procesador que no existe (APIC id 240). No contestó, el número en línea no cambió, no quedó ranura, y la pila de kernel que se le había preparado **no se liberó**: un procesador que contestara tarde correría sobre memoria de otro. Sin este control, el camino de arranque solo se habría ejecutado contra procesadores que responden.

### Trabajo repartido, no por turnos

Los cuatro procesadores tomaron sus propias interrupciones de timer y despacharon hilos de usuario; el menos cargado acumuló su propio tiempo de usuario. Cuatro hilos llegaron a estar despachados **en el mismo instante**, y ningún ámbito superó su propio techo de simultaneidad.

Esa cifra se toma donde el contador cambia y no donde se pide el informe. Un informe solo puede ver el instante en que corre, y en cuatro procesadores ese instante casi nunca es el interesante: la primera versión de esta medida leía valores ya asentados y reportaba cero simultaneidad en una ejecución con cuatro hilos corriendo.

Los hilos migraron 346 veces entre doce pares distintos de procesadores, cada migración llevando consigo el estado de coma flotante.

### Un solo reloj

34 638 lecturas del reloj desde los cuatro procesadores, cero regresiones. Los 1222 registros del log llevan además marcas de tiempo no decrecientes, que es la mitad independiente de la misma afirmación: si el reloj fuera por procesador, la secuencia no estaría ordenada.

### El presupuesto agregado, y qué acota exactamente

Esta es la parte donde es más fácil afirmar de más, así que conviene separar tres números que no dicen lo mismo.

**Lo comprometido nunca excedió el presupuesto.** Cada ámbito informa lo máximo que llegó a tener prometido a la vez —lo ya gastado más lo reservado y no gastado—, y en los siete ámbitos ese máximo es exactamente el presupuesto y nunca más. La reserva se toma antes del despacho, en el ámbito y en todos sus ancestros, bajo una sola sección crítica.

**El techo rechazó cosas.** 45 220 reservas de despacho rechazadas por falta de saldo frente a 626 concedidas. Un techo contra el que nunca se rechazó nada es un número, no un límite, y el contador existe para distinguir los dos casos.

**Lo ejecutado sí excedió la ventana, y eso está medido y atribuido.** Una ventana puede cerrar por encima del presupuesto por dos razones distintas que es fácil confundir: porque abrió ya endeudada, o porque lo que corrió dentro de ella se pasó. La primera no dice nada sobre la reserva. La segunda sí, y es la que se mide: lo cargado dentro de la ventana menos lo que la ventana podía admitir. El peor caso del ámbito `burn` es 3.3 ms.

La causa está medida, no supuesta: el intervalo más largo que cubrió un solo cobro de tiempo de usuario en esta ejecución es **5.0 ms**, contra un cuanto de 1 ms y un tick de 500 µs. La preempción es por tick, y en TCG un procesador virtual puede pasar milisegundos sin que el anfitrión lo ejecute. La puerta compara el exceso contra ese intervalo multiplicado por la simultaneidad del ámbito, de modo que un exceso mayor que la latencia de preempción real haría fallar el criterio en lugar de excusarse.

**El exceso se arrastró.** 279 ventanas cerraron por encima del presupuesto en siete ámbitos, todas dejando constancia de lo que arrastraban, y ninguna deuda decreció entre ventanas. `burn` terminó la ejecución debiendo 84 ms.

### Una traducción retirada mientras el espacio estaba vivo

El dominio `probe` tiene dos hilos y lee una página en bucle. El supervisor le quita el mapeo desde otro procesador. El registro dice cuatro cosas que hay que leer juntas:

```text
mem.unmapped domain=3 vaddr=0x20200000 pages=1 active_in_space=0
  space_cpu_mask=0xf space_cpus=4 invalidation_generation=3
  acknowledged_cpus=4 expected_cpus=4 acknowledged=1
```

`active_in_space=0` es la medida instantánea, y aquí salió cero: en el instante exacto de la llamada ninguno de los dos hilos estaba despachado. Esa medida no sirve para nada por sí sola, y por eso está la de al lado: `space_cpu_mask=0xf` son **todos los procesadores en los que ese espacio de direcciones ha llegado a ejecutarse**, que es el conjunto al que una invalidación tiene que llegar. Un procesador que corrió allí hace un microsegundo puede seguir teniendo la traducción.

La llamada no contestó hasta que los cuatro acusaron la generación de invalidación. Después, la siguiente lectura del `probe` falló con un fallo de página de **lectura** en esa misma dirección —error 0x4, no 0x6: lo que desapareció es la traducción, no el permiso de escritura— y el kernel sobrevivió. Antes de la retirada había leído la página 86 veces; después, ninguna.

### Un sello publicado contra un escritor vivo

El dominio `writer` escribe una página en bucle. El supervisor la sella desde otro procesador:

```text
mem.sealed object=36 label=sealme pages=1 writers_withdrawn=1 pages_unmapped=1
  remaining_maps=0 invalidation_generation=31 acknowledged_cpus=4 expected_cpus=4
  writer_cpus_at_withdrawal=1 writer_cpu_mask=0xf writer_cpus_ever=4
  perimeter=cpu_translations_retired dma=none
```

El sello retiró el mapeo escribible antes de prometer nada, y no publicó el estado sellado hasta que los cuatro procesadores acusaron. El escritor había llegado a ejecutarse en los cuatro. Su siguiente escritura falló —error 0x6— y el kernel sobrevivió.

Y la mitad que solo el llamante puede aportar: el supervisor leyó los mismos ocho bytes dos veces, con dos ventanas de tiempo real en medio durante las cuales el escritor seguía vivo y seguía intentándolo. Los dos valores coinciden.

El perímetro que el registro declara es `cpu_translations_retired` y el DMA que declara es `none`. Eso es exactamente lo que este sello cubre y no más: no hay dispositivo con concesión sobre ese objeto, y si lo hubiera el sello sería otra cosa. El kernel rechaza sellar un objeto con concesiones de DMA vivas en lugar de sellarlo y llamarlo igual.

### Marcos que no vuelven hasta que todos han invalidado

Ocho dominios se reclamaron a lo largo de la ejecución. Ninguno devolvió sus marcos al asignador en el momento: todos los pusieron en cuarentena sellada con la generación de invalidación vigente, y salieron de ella cuando todos los procesadores habían pasado por esa generación. Pico de 44 marcos retenidos a la vez, 185 liberados en total, cero retenidos al final y cero retenidos permanentemente. Ningún dominio terminó con marcos a su nombre.

El registro nombra la condición —`condition=all_processors_invalidated`— porque un marco para el que otro procesador todavía pueda tener traducción no está libre, y un contador que solo dijera «liberados» no distinguiría los dos casos.

### Una barrera mientras se admitían llamadas

Dos dominios llaman a un endpoint en bucle desde procesadores distintos. El supervisor contesta nueve llamadas y **entonces** cierra el ámbito de los llamantes. Los dos informan cuántas se les admitieron antes y con qué código se les rechazó después: `CANCELLED`, no un error genérico. El ámbito quedó quiescente y se retiró sin retener páginas ni metadatos.

### Un dispositivo, y qué se le dio a quién

El kernel enumeró PCI por ECAM, midió los BARs con la decodificación de memoria apagada, recorrió las capacidades con límite y detección de ciclos, y encontró una función virtio moderna. La asignó **en estado listo y sin capacidad de maestro de bus**: poder emitir transacciones es lo último que se concede, no lo primero.

Las cuatro estructuras del transporte —configuración común, notificación, ISR y configuración específica— se mapearon en el dominio del driver sin caché y sin ejecución, en direcciones que el driver recibe y no elige. Ninguna comparte región de memoria con la tabla MSI-X: la protección de página no puede separar dos estructuras que comparten página, y esa tabla lleva la dirección y el dato de una interrupción. El kernel rechaza en el descubrimiento una estructura que solapara con ella.

**La entrada de la tabla de interrupciones la escribió el kernel**, no el driver. El driver recibió `DEVICE_MAP`, `DEVICE_IRQ` y `DEVICE_DMA`; no recibió maestro de bus ni reset, y sus intentos de ambos fueron rechazados por falta de derechos. Tres interrupciones llegaron al vector enlazado y levantaron los bits de la señal enlazada; ninguna llegó sin binding.

### El driver, y lo que valida antes de creérselo

`k3driver` negocia el transporte completo —reset, ACKNOWLEDGE, DRIVER, características, FEATURES_OK releído, colas, DRIVER_OK—, publica un anillo dividido en memoria que le concedieron, lee un bloque, escribe uno que compone él, y hace un flush. Las tres peticiones completaron con estado de dispositivo 0, cada una detrás de una interrupción y cada una por una cadena de descriptores distinta.

Valida cada entrada del anillo de usados antes de creérsela: la diferencia de índices acotada por el tamaño de la cola, un identificador que él publicó, una longitud que no exceda la que ofreció. Y —esto es lo que hace que esas ramas no sean código que nadie ha ejecutado— corre el validador contra entradas que **fabrica él mismo**, en la cuarta página del objeto del anillo, que es justamente la que el supervisor **no** concedió al dispositivo. Tres entradas dañadas rechazadas por tres razones distintas, y una entrada plausible aceptada: un validador que rechazara todo pasaría las tres primeras por el motivo equivocado.

### Perfil de DMA: lo que se concede y lo que se niega

```text
device.dma_granted profile=1 profile_reason=no_remapping_unit_described
  enforced_by=nothing_driver_is_trusted
device.profile_refused required=2 available=1 reason=no_remapping_unit_described
  iommu_described=0 iommu_translating=0
```

La concesión es del perfil **débil** y lo dice en el registro con las palabras que corresponden: `enforced_by=nothing_driver_is_trusted`. Nada en esta máquina impide que el dispositivo escriba fuera de lo concedido. Lo que la concesión hace es acotar lo que el kernel programa y contabilizar las páginas para que no se reutilicen mientras el dispositivo puede alcanzarlas; no es una barrera de hardware y no se describe como tal.

El driver pidió después el perfil **fuerte**, y fue rechazado con `UNSUPPORTED_PROFILE` (−21) y no con un error genérico: un rechazo que llegue como otra cosa invita a reintentar hasta caer en una degradación silenciosa. Ese es INV-19 ejercido, no citado.

El perfil de control lo hace más explícito. Con `-device intel-iommu,intremap=off,caching-mode=on` el kernel encuentra y valida la tabla DMAR, informa `remapping_unit_described=1 remapping_programmed=0`, y el motivo del perfil pasa a ser `remapping_described_but_not_programmed`. **El perfil sigue siendo débil y el fuerte sigue rechazándose.** Una unidad descrita no es una unidad programada, y K3 no la programa.

### El dispositivo se detiene bajo su driver

El supervisor esperó a que el driver anunciara que había terminado y entonces, sin pedirle permiso: quitó el maestro de bus **primero** —entre la decisión de parar y la confirmación de que paró, el dispositivo no debe poder emitir nada—, retiró una ventana de registros por su nombre con acuse de los cuatro procesadores, y reseteó el transporte confirmando el resultado **leyendo el estado de vuelta** en lugar de suponerlo.

```text
device.reset status_after_reset=0x0 bus_master=0 irq_unbound=1 regions_unmapped=3
  dma_pages_revoked=3 new_session=2 acknowledged_cpus=4 expected_cpus=4
  confirmed_by=transport_status_zero
```

El driver, al que se le dijo que probara lo que recordaba, fue rechazado tres veces con `STATE_CONFLICT`: todo lo que nombra pertenece a una sesión que ya no existe. Al final de la ejecución no quedó ninguna concesión de DMA, ninguna ventana de registros y ningún binding de interrupción, y ninguno nombrando una sesión terminada.

## Cómo se decide la puerta

`tools/check_k3.py` decide **28** criterios por separado, arrastrando cada uno las líneas que lo decidieron.

En K3 el relato del acusado vale todavía menos que en K2. Un dominio no puede ver en qué procesador corrió, ni si una invalidación llegó a los demás, ni qué hizo un dispositivo con la memoria que se le concedió. Por eso los criterios se deciden desde los registros del kernel, y los **tres** que necesariamente leen notas de un programa lo dicen en su título: el estado que solo el llamante recibió, la comparación que solo el llamante pudo hacer, y el validador que solo el driver pudo ejecutar. Esa convención, que en K2 quedó a medias, aquí está aplicada entera.

Los criterios que serían fáciles de falsear son los escritos con más cuidado. Una retirada no se juzga por cuántos procesadores estaban dentro del espacio en ese instante, sino por la máscara de todos en los que el espacio ha corrido. Un presupuesto no se juzga por si una ventana cerró dentro de él —la deuda arrastrada lo hace imposible— sino por si la admisión prometió alguna vez más de lo que tenía y por si rechazó algo alguna vez; y lo que la ejecución se pasó se compara contra la latencia de preempción medida en esa misma ejecución.

`--self-test` daña la ejecución de **57** formas distintas, una cada vez, y un criterio nombrado tiene que notar cada una: un procesador ausente que contesta, una migración que deja atrás el estado de coma flotante, un sello cuyo escritor solo había corrido en un procesador, marcos liberados antes de que todos invalidaran, una entrada de tabla de interrupciones escrita por el driver, un validador que no rechaza nada, un reset confirmado solo por su propio retorno, una concesión débil descrita como impuesta por hardware, una unidad de remapeo que el kernel dice haber programado.

Las regresiones de K1 y K2 son criterios de esta puerta, y la plataforma también: el número de procesadores del `run.json` tiene que coincidir con los que arrancaron.

## Qué queda demostrado y qué no

| Experimento | Alcance cubierto por esta ejecución | Lo que falta |
|---|---|---|
| [EXP-02](../validation/experiments.md) | Reciclado de ranuras y rechazos bajo un ámbito cerrado con cuatro procesadores y llamadas concurrentes en vuelo. | Reinicio con handles persistidos, que es K4. |
| [EXP-03](../validation/experiments.md) | Cierre concurrente con llamadas de dos dominios en procesadores distintos; barrera separada de drenaje, quiescencia derivada de contadores. | Servidor muerto durante la retención y resultado posterior con estado durable, que es K4. |
| [EXP-04](../validation/experiments.md) | CPU en SMP: reserva agregada antes del despacho, simultaneidad acotada por ámbito, deuda arrastrada, y el exceso de ejecución medido y atribuido. | Fan-out real y mantenimiento; medición en hardware físico. |
| [EXP-05](../validation/experiments.md) | Alias y TLB remotos con acuse de todos los procesadores; sello solo después de la retirada; reclamación diferida por generación; perfil DMA declarado y el fuerte rechazado. | **DMA tardío con unidad de remapeo programada.** El reset se ejecuta y se confirma leyendo el transporte, pero nada impide físicamente una escritura del dispositivo en esta plataforma. |
| [EXP-06](../validation/experiments.md) | Agotamiento de tabla de capacidades y de cola con cuatro procesadores y varios llamantes. | Varios servidores concurrentes. |

Esta ejecución **no** demuestra:

- **Aislamiento de DMA.** No hay unidad de remapeo programada. El perfil débil es lo único que esta plataforma sostiene, el kernel lo dice en cada registro que lo menciona, y el perfil fuerte se rechaza en lugar de aproximarse. Un driver no confiable **no** está contenido aquí.
- **Hardware físico.** Todo lo anterior es QEMU con TCG. El inventario de CPU, firmware, dispositivos y grupos de aislamiento que [OQ-05](../roadmap/open-questions.md) pide sigue sin hacerse, y ninguna afirmación de esta nota debe leerse como si se hubiera hecho.
- **Escalabilidad ni rendimiento.** Cuatro procesadores emulados por hilos que el anfitrión reparte a su gusto. Los tiempos de esta ejecución miden el emulador tanto como el kernel, y la propia nota lo usa así: la latencia de preempción de 5 ms es un dato sobre la plataforma.
- **Más de un dispositivo, ni interrupciones legadas.** Una función virtio-blk moderna, MSI-X, un solo vector enlazado. No hay IOAPIC ni INTx, no hay hotplug, no hay virtio-net.
- **Cobertura de caminos.** 37 de 59 operaciones alcanzadas, cada una por un camino. Las 22 restantes pertenecen a caminos que este paquete no recorre y están nombradas una a una en el propio registro.

### Limitaciones de esta evidencia, explícitas

- **El perímetro del sello es de CPU, y lo dice.** `perimeter=cpu_translations_retired dma=none`. Un sello con un dispositivo capaz de escribir el objeto necesitaría un perímetro que esta plataforma no puede ofrecer; el kernel rechaza sellar con concesiones vivas en lugar de sellar y llamarlo igual.
- **`active_in_space` puede salir cero y sale cero a menudo.** Es una medida instantánea de algo que casi nunca es interesante en ese instante. La máscara acumulada es lo que decide el criterio; la medida instantánea se sigue publicando porque quitarla escondería la diferencia entre las dos preguntas.
- **El exceso de presupuesto es real.** La admisión está acotada exactamente; la ejecución no, y en esta plataforma se pasa por milisegundos. La puerta lo acota contra una latencia medida, no contra cero. En hardware con preempción puntual el número sería otro, y no se ha medido.
- **Un solo control de procesador ausente.** Se prueba un APIC id que no existe. No se prueba un procesador que arranca y falla a mitad del trampolín, ni uno que contesta tarde: el segundo caso es justamente por el que la pila no se libera, y esa decisión está tomada sin haberla observado ocurrir.
- **El validador del anillo se prueba contra daño sintético.** Ningún dispositivo real produjo esas entradas. Lo que se demuestra es que las ramas de rechazo funcionan y por el motivo correcto, no que un dispositivo hostil las active.
- **27 advertencias de clippy**, todas de estilo y ninguna arreglada. Diecisiete venían de antes; las diez restantes están en archivos que K3 escribió o reescribió —`tlb.rs` 3, `acpi.rs` 2, `api/devops.rs` 2, `arch/x86_64/lapic.rs` 2, `pci.rs` 1, `device.rs` 1—. Son de tres familias: implementar a mano `is_multiple_of`, `Range::contains` y la división comprobada, e indexar un array con la variable de un bucle. La afirmación de K2 de que ningún archivo de la fase tenía advertencias ya se corrigió allí; esta nota no la repite.

## Qué corrigió ejecutar los mecanismos

Compilar el SMP no encontró ninguno de estos seis defectos. Los tres primeros son fatales y los tres son de la misma familia: **suponer que el estado de un hilo basta para decir de quién es**. En un procesador esa suposición es cierta.

1. **Dos procesadores sobre un mismo hilo.** Elegir un hilo y marcarlo en ejecución ocurrían en dos secciones críticas distintas, así que dos procesadores podían elegir el mismo hilo `Ready` y ejecutarlo sobre una sola pila de kernel. El registro que lo delató fue una migración imposible; el fallo, un `#PF` con `rip=cr2=0xffffffffffffffff`. La selección, la reserva y la reclamación ocurren ahora bajo una sola toma del cerrojo.
2. **Reclamar lo que otro procesador todavía usa.** La reclamación comparaba solo el `CR3` del procesador que reclamaba, de modo que liberaba la pila de kernel o el espacio de direcciones sobre el que otro procesador seguía ejecutando. El síntoma fue un triple fault inmediatamente después de terminar un dominio. Ahora se consultan el hilo actual **y el saliente** de todos los procesadores en línea.
3. **Despachar un hilo que todavía no ha guardado su contexto.** Un hilo que se bloquea voluntariamente escribe su estado bajo el cerrojo, lo suelta, y solo después llega al cambio de contexto que guarda dónde continuar. Hasta esta ejecución nada podía despertarlo dentro de esa ventana, porque lo único que podía estaba en el mismo procesador. Con cuatro, el llamante de un endpoint publica su mensaje, el receptor corre en otro procesador, contesta, y lo marca `Ready` mientras el procesador en el que sigue ejecutando no ha guardado nada. Otro procesador lo tomó y saltó a la dirección cero. `current` y `previous` cubren esa ventana sin hueco, y un hilo `Ready` que otro procesador nombre por cualquiera de los dos deja de ser candidato.

Los tres siguientes son de contabilidad, y los encontró la ejecución de la misma manera: pidiendo algo y viendo que la respuesta no llegaba.

4. **Una reserva que no acotaba la ejecución.** El despacho recibía lo que quedara de la ventana de su ámbito y después corría un cuanto entero de todos modos. Dos hilos compartiendo presupuesto podían admitirse cada uno por una fracción de milisegundo y correr cada uno uno entero. El cuanto sale ahora de lo concedido, y lo único que queda de holgura es que la preempción es por tick.
5. **Hilos que un dominio nunca devolvía.** El recuento de hilos que un dominio devuelve a su ámbito se leía **después** de vaciar sus ranuras, de modo que siempre era cero y el suelo de la fórmula devolvía exactamente uno. El dominio `probe`, con dos hilos, dejaba su ámbito sosteniendo permanentemente un hilo vivo que no existía: el ámbito no podía quedar quiescente y la retirada esperaba segundo y medio antes de rendirse con `DRAIN_INCOMPLETE`. En un procesador nada tenía dos hilos.
6. **Un mapeo que soltaba la concesión que nombraba.** Un registro de mapeo nombra por índice de tabla la concesión que lo autorizó, y no retenía ninguna referencia sobre ella. Un dominio que cerrara su último handle sobre un objeto mapeado devolvía la concesión al asignador mientras el mapeo seguía nombrándola. No aparecía en un procesador porque nada cerraba un handle sobre algo que todavía tenía mapeado; el supervisor de K3 tiene que hacerlo, porque su tabla es de treinta y dos entradas y construye once cosas.

Además, dos defectos de la evidencia misma, encontrados al escribir la puerta y no al escribir el kernel: la simultaneidad máxima se leía de valores ya asentados y reportaba cero en una ejecución con cuatro hilos corriendo; y las interrupciones entregadas se sumaban sobre los bindings vivos, de modo que una ejecución que entregó tres y después reseteó el dispositivo informaba de que no había entregado ninguna. Una medida que se toma donde no pasa nada no mide nada.

Y dos del propio paquete K3, que la ejecución encontró antes que cualquier lectura: la petición de flush publicaba un descriptor de longitud cero y una cadena que se salía de la tabla de descriptores, y el dispositivo no completaba lo que no puede aceptar; y el caso del autotest del validador que debía ser **aceptado** nombraba un descriptor que nunca estaba en vuelo, de modo que se rechazaba, y el control que existía para impedir que el validador rechazara todo no lo impedía.

## Cómo repetirlo

```sh
python3 tools/build_image.py --phase k3   # imagen K3 + manifiesto de digests
python3 tools/run_k3.py                   # cuatro procesadores, virtio-blk moderno
python3 tools/check_k3.py                 # veredicto por criterio
python3 tools/check_k3.py --self-test     # el comprobador contra ejecuciones dañadas
python3 tools/run_k3.py --profile dmar --out build/run-k3-dmar   # unidad descrita, no programada
python3 tools/check_k3.py --run build/run-k3-dmar
```

Las regresiones de K1 y K2 son parte de la puerta y se producen con sus propias herramientas:

```sh
python3 tools/build_image.py && python3 tools/run_k1.py && python3 tools/check_k1.py --json build/k1-gate.json
python3 tools/build_image.py --phase k2 && python3 tools/run_k2.py && python3 tools/check_k2.py --json build/k2-gate.json
```

Si QEMU, OVMF o mtools no están instalados en el sistema, `THALYX_TOOL_PREFIX` apunta a un árbol que los contenga; `python3 tools/toolchain.py` informa de qué binario resolvió cada uno.
