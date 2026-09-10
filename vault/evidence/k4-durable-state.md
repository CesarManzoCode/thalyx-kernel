---
id: EVD-007
kind: evidence
status: observed
---
# K4 — Estado durable: qué se ejecutó y qué demuestra

Esta nota registra la primera ejecución en la que algo sobrevive a la ejecución que lo hizo. K3 tenía derecho a una suposición que K4 tiene que romper: que todo lo que importa cabe en memoria y desaparece al terminar. Aquí un servicio publica versiones sobre un medio de bloques real, la ejecución se corta en cada punto donde una publicación puede cortarse, y la siguiente arranca sobre lo que el corte dejó.

La tentación aquí no es la de K3. En K3 el vocabulario permitía escribir «el dispositivo está contenido» sin que nada lo contuviera. En K4 permite escribir «esto es durable» cuando lo único demostrado es que unos bytes quedaron en un archivo del anfitrión después de que un programa dijera que los había escrito. Esta ejecución **no** prueba durabilidad frente a un corte de energía, y todo lo que sigue está escrito para que ese hecho sea imposible de perder de vista.

## Qué se ejecutó

Una cuarta imagen UEFI construida desde las mismas fuentes con el mismo script y **el mismo binario de kernel** que las imágenes K1, K2 y K3. Lo único que cambia es el paquete de arranque.

| Artefacto | Comando | Resultado |
|---|---|---|
| Imagen | `python3 tools/build_image.py --phase k4` | `build/thalyx-k4.img`, manifiesto con digest de cada artefacto y herramienta |
| Matriz | `python3 tools/run_k4_cases.py` | 15 casos, 25 tramos, todos con `exit_status=33` (`k1.terminal status=complete`) |
| Veredicto | `python3 tools/check_k4.py` | `K4 GATE PASSED: 31 of 31 criteria met` |
| Autocomprobación | `python3 tools/check_k4.py --self-test` | `48 of 48 damaged runs were caught` |
| Formato | `python3 tools/check_k4_format.py` | `12 of 12`, nueve de ellos daños aplicados y rechazados |
| Cobertura | `k2.coverage` en el propio registro | 33 de 59 alcanzadas; las 23 que K4 necesita entre ellas y las 26 restantes nombradas una a una |

Plataforma exacta: QEMU `q35` con acelerador `tcg`, cuatro procesadores, CPU `qemu64,+smep,+smap,+pdpe1gb,+x2apic`, 512 MiB, OVMF, y una función `virtio-blk-pci` con `disable-legacy=on,disable-modern=off` sobre un **medio raw de 8 MiB con `cache=writeback`**, creado en blanco al empezar cada caso y arrastrado de un tramo al siguiente.

Digests de la ejecución registrada:

```text
image   62ce3fa61a3586c4330f0ef3f8a1f7a2f873a05f9bc208a3d572c0183e35b5fb
kernel  eb7b45a40e29f44369f9a0ae94c76e0c0de58aa6dd4089849a40e4bd386d747f
loader  669c530b353b8aba9dfa58147167c6879765b3cbaeb109204c41764a1f7bea83
package fde53336f0d1365acd35d9a9b5f2f03cf8885b392084ae8c4062d8a116592f7e
```

El digest del kernel es el mismo en los cuatro manifiestos de fase. Eso es lo que permite decir «K1, K2 y K3 siguen pasando» como afirmación sobre un kernel y no sobre cuatro.

## Qué es un caso, y por qué la matriz tiene quince

Un caso es un escenario, una directiva de fallos y un número de tramos sobre **un solo medio**, creado en blanco una vez y arrastrado de un tramo al siguiente. La directiva vive en un bloque **fuera** del almacén, con su propia magia, y ninguna estructura del formato la nombra. El anfitrión no escribe nada más en el medio: lo que un medio contiene es lo que el invitado puso ahí.

Un tramo se corta; los demás son la recuperación, y reciben una directiva que no nombra ningún punto. La recuperación nunca consulta la directiva. Un caso puede pedir que se corte un tramo posterior —`abort-visible` lo hace— y eso es lo único que permite mirar lo que la recuperación escribió antes de que una compactación reescriba la arena por encima.

| Caso | Corte | Qué pone a prueba |
|---|---|---|
| `baseline` | ninguno | La vertical entera sin quitarle nada. |
| `cut-after-prepare` | `AFTER_PREPARE` / `STOP` | Un prepare durable cuyo commit nunca ocurrió. |
| `abort-visible` | `AFTER_PREPARE` / `STOP`, y el tramo 2 en `BEFORE_PREPARE` | Lo mismo, con el segundo tramo cortado para poder leer el abort en el medio. |
| `drop-commit` | `BEFORE_COMMIT` / `DROP_WRITE` | Una escritura que el servicio cree hecha y que nunca se emitió. |
| `cut-after-commit` | `AFTER_COMMIT` / `STOP` | Un commit durable sin el checkpoint que lo nombraría. |
| `cut-after-checkpoint` | `AFTER_CHECKPOINT` / `STOP` | Checkpoint escrito, superblock todavía apuntando al anterior. |
| `cut-after-superblock` | `AFTER_SUPERBLOCK` / `LOSE_RESPONSE` | Todo durable y el llamante nunca enterado. |
| `tear-object` | `BEFORE_OBJECT` / `TEAR_WRITE`, ordinal 5 | Un registro desgarrado: llega su primer sector y nada más. |
| `io-error-commit` | `BEFORE_COMMIT` / `IO_ERROR` | El medio niega la escritura. |
| `reorder-commit` | `BEFORE_COMMIT` / `REORDER` | Una escritura retenida y emitida después de la siguiente. |
| `cut-after-outbox` | `AFTER_OUTBOX_SEND` / `STOP` | El broker contestó y el registro de lo que dijo no llegó a ser durable. |
| `kill-service` | `AFTER_PREPARE` / `KILL_SERVICE` | El servicio se reemplaza dentro de una ejecución. |
| `rival` | escenario 2 | Dos publicadores pidiendo la misma transición. |
| `broker-unknown` | escenario 3 | Un broker que no puede decir qué pasó. |
| `control-lost` | escenario 5 | El auditor deja de leer y el plano de control se llena. |

El corte lo aplica el **driver del invitado**, no el emulador. QEMU ve las escrituras que se emitieron y ninguna otra, así que lo que el medio contiene después de un tramo es exactamente lo que el driver puso ahí. Es una afirmación más estrecha que un corte de energía contra una controladora real, y esta nota la escribe así en lugar de insinuar que el arnés prueba algo sobre hardware que nunca tocó.

## Qué observó la ejecución

### Un medio en blanco, y un almacén que decodifica desde el anfitrión

El servicio no encontró almacén y escribió uno. Lo que quedó en el medio lo decodifica `tools/k4_format.py`, generado desde el esquema, sin enlazar nada del invitado: dos superblocks válidos, el más nuevo en generación 2, publicando la generación 3, con la arena 1 activa y un prefijo de 10 registros. Los 235 registros que la matriz dejó entre todos sus medios verifican bajo su propio digest, encadenan con el anterior y su secuencia sube exactamente de uno; aparecen los seis tipos que el formato define.

El invitado reconstruyó los 22 vectores dorados con el SHA-256 escrito en este repositorio antes de admitir nada, en los 26 servicios que arrancaron a lo largo de la matriz, y no falló ninguno. Generar los dos lados desde el esquema no los hace estar de acuerdo: comparten desplazamientos, y por supuesto coinciden en eso. Lo que los somete al mismo juicio son los vectores.

### Tres publicaciones, y una repetida

El publicador hace la secuencia ordinaria —preparar contenido, bifurcar sobre la versión publicada, enlazar nombres, congelar un candidato y publicar contra la generación de la que bifurcó— tres veces. El medio publica la generación 3, su raíz alcanza los 5 objetos que nombra y todos están ahí, sus primeros ocho bytes son los que el servicio dijo, y ningún commit del medio nombra una generación que nadie afirmó.

Después repite **la misma petición byte a byte**: la misma secuencia, la misma expectativa y los mismos tres digests. La respuesta es la generación que ya era durable, no una segunda. En los ocho registros de commit que la matriz deja entre todos sus medios hay ocho identidades de petición distintas y ninguna con dos generaciones.

### Las negativas que un CAS tiene que hacer

Cuatro, provocadas a propósito y cada una distinguida de las otras:

- **Identidad reutilizada con otros inputs.** La misma secuencia, contenido distinto: `CONFLICT`. Una petición no cambia de significado después del hecho.
- **Hueco de secuencia.** Una secuencia por delante de la que toca: `SEQUENCE_GAP`.
- **Expectativa vieja.** Publicar contra una versión que ya no es la publicada: `GENERATION_STALE`.
- **Evidencia que nombra otros inputs.** `VALIDATION_MISMATCH`, y llega **antes** de mirar la expectativa: el orden es estructura, después autoridad, después efecto, y una evidencia mal formada no llega a discutir el CAS. Ese orden es lo que hizo que el control de expectativa vieja tuviera que ofrecer evidencia que nombrara la expectativa vieja para llegar a lo que apuntaba.

Un lector alcanza el servicio, lee de él, y es rechazado con `FORBIDDEN` al publicar: puede preparar contenido y no puede publicarlo, y no posee ningún commit en el medio.

En el caso `rival`, dos principales piden convertirse en la generación 1. El servicio la afirma una sola vez, uno de los dos es informado de que la tiene, el otro es rechazado, y ningún medio del caso tiene dos commits que la nombren.

### El kernel admite el efecto antes de que la publicación empiece

Tres publicaciones, tres admisiones en los registros del kernel, tres recibos de efecto. El supervisor —que es quien tiene el log con derecho a acusar recibo, y no el servicio auditado— leyó y acusó 182 recibos en el caso base, contó los tres efectos desde el log y no desde el servicio, y no encontró ni un hueco en la secuencia ni un recibo perdido en ninguno de los 25 tramos.

Ese plano tiene un límite y la matriz lo ejerce. Con el auditor parado, el log se llena hasta sus 56 celdas ordinarias y el kernel **rechaza las admisiones que esos recibos cubrirían** en vez de perder ninguno. El servicio no puede entonces ni alcanzar al driver, se declara `UNAVAILABLE` —no `INTEGRITY_FAILED`, porque el almacén está intacto y decirlo dañado mandaría al llamante a desconfiar de algo que está bien— y le dice al llamante `UNKNOWN` en lugar de un resultado. Ninguna versión llegó al medio que el servicio no afirmara.

### Compactación

Nueve objetos copiados a la arena 1, que abre con un checkpoint en la secuencia 25. La raíz publicada sigue alcanzando los cinco objetos que nombra y todos están en la arena nueva. Una versión superada no se conserva: eso es lo que la compactación es, y por eso los criterios que hablan de commits los buscan donde un corte impidió la compactación en lugar de exigirlos donde por diseño ya no están.

### Outbox

El broker contestó `DELIVERED` y `cut-after-checkpoint` lo tiene en el medio, en la secuencia 17, para el principal 1 y su petición 2. En el caso donde el broker **no puede decir**, lo que queda registrado es `UNKNOWN`: un intento que ni tuvo éxito ni se sabe que fracasara es un resultado, y convertirlo en cualquiera de los dos sería el servicio inventándose uno.

### Once cortes, y lo que cada uno dejó

Cada corte ocurrió en el punto y en el modo que su directiva nombraba, y ningún tramo de recuperación aplicó ninguno. En los cuatro casos donde el driver retuvo una escritura, el prefijo válido del medio **termina** en lugar de llegar al final de la arena, y termina en el bloque que el driver dijo haber retenido o antes.

- **Prepare sin resolver.** El corte dejó un prepare y ningún commit. La recuperación escribió un registro de abort que nombra ese prepare con razón `RECOVERED_UNRESOLVED`, adoptó la generación 0, y **no** convirtió el prepare en una versión. Un prepare no es una promesa.
- **Commit durable sin checkpoint.** El corte dejó un commit para la generación 1 bajo un superblock que seguía publicando la 0. El arranque siguiente leyó el registro y adoptó la 1.
- **Registro desgarrado.** El bloque 72 lleva la magia de un registro y no verifica. El anfitrión camina 6 registros antes de él y el invitado escaneó exactamente 6. El ordinal del corte nombra el contenido de la versión y no su manifiesto: un registro que cabe en un sector queda entero o ausente, y no puede observarse siendo desgarrado.
- **Respuesta perdida.** La ejecución se cortó sin contestar. En el arranque siguiente el cliente preguntó qué había sido de sus propias peticiones, encontró 3 identidades ya gastadas, la última como `COMMITTED`, y siguió después de ella. Adivinar no estaba disponible: una identidad con resultado durable se contesta con ese resultado para siempre, y reutilizarla con otros inputs es un conflicto y no un reintento.
- **Servicio reemplazado.** El servicio pidió ser reemplazado, el supervisor lo terminó y construyó la instancia 2, y el reemplazo recuperó el medio y volvió a admitir peticiones. El reemplazo recibe un **endpoint nuevo**: un endpoint cuyo receptor murió deja de admitir y el kernel se niega a entregárselo a otro. Lo que el reemplazo hereda es el medio, que es lo único que tenía que sobrevivir.

### El área de preparación se recupera en vez de agotarse

Cinco barridos en cuatro tramos liberaron 40 objetos preparados entre ellos, en ningún tramo de ningún caso un barrido liberó nada ni un fork se quedó sin workspace, y la raíz publicada sigue alcanzando los cinco objetos que nombra. Un candidato que nadie publica —que es lo que **cada control negativo** deja atrás— retenía su preparación para el resto de la ejecución hasta que esto existió.

## Cómo se decide la puerta

`tools/check_k4.py` decide **31** criterios por separado, arrastrando cada uno las líneas y los bloques que lo decidieron.

En K4 el relato del acusado vale todavía menos que en K3. El relato de un servicio sobre lo que escribió no es evidencia de que se escribiera nada, y su relato sobre lo que recuperó no es evidencia de que un medio lo dijera. Por eso los criterios se deciden desde dos fuentes que el invitado no narra: los registros del kernel y los bytes del medio, decodificados aquí por el módulo que genera el esquema. Los criterios que necesariamente leen notas de un programa lo dicen en su título.

`--self-test` daña la evidencia de **48** formas distintas, una cada vez, y un criterio nombrado tiene que notar cada una. Treinta y seis son reescrituras del registro de un tramo; nueve son ediciones de bytes de un medio, porque la mitad de lo que esta puerta lee no es un log; y tres se aplican a **todos** los tramos de todos los casos, porque hay criterios cuya afirmación es sobre la matriz entera y dañar un solo tramo no los rompería —lo cual informaría de éxito para una comprobación que nunca se hizo—. Los daños al medio localizan el registro que el criterio trata —el commit que el arranque siguiente adoptó, el abort que la recuperación escribió, el registro de outbox— en lugar de nombrar un número de bloque: un daño escrito con un bloque literal deja de dañar nada en cuanto una ejecución coloca sus registros de otra manera, y entonces informa de éxito para una comprobación que nunca hizo.

Las regresiones de K1, K2 y K3 son criterios de esta puerta.

## Qué queda demostrado y qué no

| Experimento | Alcance cubierto por esta ejecución | Lo que falta |
|---|---|---|
| [EXP-03](../validation/experiments.md) | Servicio muerto durante la retención: un servicio cortado deja invocaciones sin contestar, el llamante recibe `UNKNOWN` o `PEER_DEAD`, y el resultado durable contesta el reintento. | Nada de K4; el resto es K5. |
| [EXP-07](../validation/experiments.md) | Validación ligada a versión, conflicto correcto, hueco de secuencia, expectativa vieja, un lector que prepara y no publica, y dos publicadores compitiendo. | Herramientas reales que cambian inputs, que es K5. |
| [EXP-08](../validation/experiments.md) | Once cortes, uno por punto de escritura, con arranque posterior sobre lo que dejaron; reintento sin doble efecto; compactación con la raíz intacta. | Corte de energía real y comportamiento de flush de una controladora física. |
| [EXP-09](../validation/experiments.md) | Evidencia incompleta rechazada, remoto incierto registrado como incierto, saturación del área de preparación, y plano de control perdido con el límite nombrado. | Servicio de control distribuido y efecto remoto real, que son K5. |

Esta ejecución **no** demuestra:

- **Durabilidad frente a un corte de energía.** La supresión la hace el driver del invitado. QEMU ve las escrituras emitidas y las escribe en un archivo del anfitrión con `cache=writeback`; nada aquí prueba qué habría sobrevivido a que se fuera la luz. Lo que se demuestra es que el almacén sobrevive a que **el escritor** desaparezca en cualquier punto, que es una propiedad distinta y más estrecha.
- **Comportamiento de flush de un dispositivo real.** El driver acusa 14 flushes y el dispositivo los completa; qué hace un flush de virtio-blk emulado con el caché del anfitrión no es lo que hace una controladora con su caché de escritura. Sin conocer eso, no se declara durabilidad, igual que sin IOMMU programada no se declara aislamiento.
- **Aislamiento de DMA.** Igual que en K3: no hay unidad de remapeo programada, `remapping_programmed=0`, y el driver de bloque pertenece al TCB de memoria. El servicio de estado **no** tiene capacidad de dispositivo alguna, que es lo que hace posible cortarlo en una escritura nombrada, pero eso acota lo que el servicio puede hacer, no lo que el driver puede hacer.
- **Hardware físico, escalabilidad ni rendimiento.** Todo es QEMU con TCG. Los tiempos miden el emulador tanto como el kernel.
- **Un almacén grande.** Dos arenas de 256 bloques, 40 entradas de objeto, 24 ranuras de preparación, cinco workspaces. Los límites se ejercen y se rechazan con estados propios; no se ha ejecutado nada que se parezca a un almacén de trabajo.
- **Cobertura de caminos.** 33 de 59 operaciones alcanzadas, cada una por un camino. Las 26 restantes están nombradas una a una en el propio registro.

### Limitaciones de esta evidencia, explícitas

- **El corte es del escritor, no del medio.** Un `STOP` termina la ejecución donde el servicio lo pide; un `DROP_WRITE` hace que una escritura no se emita y le dice al servicio que sí. Ninguno de los dos es una controladora perdiendo su caché.
- **`drop-commit` necesita terminar en el flush siguiente para verse.** Una escritura suprimida que el servicio cree hecha queda enmascarada en cuanto una compactación posterior reescribe la arena desde memoria. El caso pide terminar donde habría flush precisamente por eso, y esa dependencia está en la propia lista de casos.
- **La recuperación se ejecuta desde un arranque nuevo, no desde una caída de VM.** El proceso QEMU termina limpiamente y el siguiente empieza con el archivo tal como quedó. Distinguir caída de proceso, caída de VM y pérdida de energía es lo que [los criterios de evidencia](../validation/experiments.md) piden, y aquí solo la primera está cubierta.
- **El outbox es un broker de prueba dentro de la misma máquina.** Lo que demuestra es que el servicio registra lo que un broker contestó, `UNKNOWN` incluido, y que no reintenta hacia una segunda entrega. No demuestra nada sobre un efecto remoto real.
- **`control-lost` para al auditor a propósito.** Es andamiaje y está marcado como tal: un escenario que el supervisor lee de la directiva. Lo que muestra es la respuesta del mecanismo a un plano de control lleno, no que un auditor real fuera a pararse.
- **34 advertencias de clippy en el workspace**, todas de estilo y ninguna en un archivo que esta fase escribiera. Los cinco paquetes K4 están limpios; el resto viene de fases anteriores y sigue sin arreglarse.

## Qué corrigió ejecutar los mecanismos

Compilar los cinco programas no encontró ninguno de estos. Los tres primeros son de la misma familia —**dar por hecho que una cosa que se guarda no cuesta nada**— y ninguno era visible sin ejecutar.

1. **La tabla de invocaciones se agotaba.** Una invocación la descarga el llamante al recoger su respuesta y la referencia el ticket que el receptor recibió, y el registro solo se reclamaba en la descarga. Un servicio que guarda sus tickets sostenía así un registro por cada petición que ya había contestado. Ahora los dos caminos terminan en un mismo colector y reclama el que ocurra segundo, que es la única regla independiente del orden. Los bucles de servicio cierran además su ticket: que el kernel no tenga fugas no autoriza a un dominio a guardar un asa que no volverá a nombrar.
2. **El supervisor guardaba todas las asas que había creado.** Un asa es una concesión y las concesiones son una tabla global fija, así que la ejecución agotaba la tabla y lo primero en fallar era lo que necesitara una a continuación —que no es donde estaba el error—. Cierra en el último uso, y `grant_alloc` dice qué recurso se acabó.
3. **El área de preparación no devolvía nada.** Un objeto vivía ahí desde la llamada que lo preparó hasta la publicación que lo escribía, y un candidato que nadie publica —que es lo que cada control negativo deja— retenía su ranura para el resto de la ejecución. Tres bastaban para quedarse con el servicio. Hay un barrido, y con él tres reglas que hicieron falta: lo que la llamada en curso acaba de preparar no puede barrerse dentro de esa misma llamada; lo que un principal preparó y todavía no ha nombrado tampoco, porque una política y una evidencia se preparan en una llamada y se nombran en otra; y el área tiene que ser **más grande** que lo máximo que se puede pedir conservar, porque un conjunto protegido mayor que el área significa que un barrido no libera nada y el servicio rechaza trabajo que no tenía motivo para rechazar. El caso con tres clientes preparando candidatos a la vez es el que encontró esa tercera.

Los tres siguientes son sobre decir la verdad en la respuesta.

4. **Una publicación con éxito no contestaba nada.** El servicio contestaba y después resolvía la invocación, y resolver la descarga: al llamante lo despertaban con `OK` y una carga vacía, enterándose de que algo se había publicado pero no de qué versión. Cada cliente leía generación cero de una respuesta a ceros y solo parecía correcto porque cero también es `OK`. Contestar **es** el commit —el kernel marca el resultado cuando un receptor responde—, así que contestar es la descarga, y es la única que lleva la respuesta.
5. **Una escritura que no se pudo pedir se informaba como almacén dañado.** `INTEGRITY_FAILED` manda al llamante a desconfiar de un almacén intacto. Que el medio niegue una escritura y que no se pueda ni preguntar son respuestas distintas, y ahora el cliente del driver sabe cuál de las dos tiene.
6. **Dos controles negativos no llegaban a lo que apuntaban.** El de expectativa vieja ofrecía evidencia que nombraba la versión actual mientras reclamaba la anterior, así que lo rechazaban por la evidencia; y el de petición repetida intentaba reconstruir su candidato bifurcando sobre una versión que ya no era la publicada, lo cual se rechaza por sí solo, de modo que el control no se ejecutaba nunca. Una petición repetida byte a byte es la que se guardó, no una reconstruida desde un estado que ya se movió.

Y tres más que solo aparecen bajo un corte.

7. **Un servicio cortado que se queda esperando no deja terminar la máquina.** Un dominio en una espera con plazo es un dominio que progresa, así que nada decidía nunca que la ejecución había terminado y el tramo había que matarlo desde fuera. Un servicio al que se le dice que pare, para: sus escrituras ya están cerradas, así que nada de lo que pudiera seguir haciendo llegaría al medio.
8. **Un servicio que pedía ser reemplazado se comía su propia petición.** Levantaba `RESTART` en la señal del supervisor y a continuación esperaba en ese mismo bit. Espera en un bit que nadie levanta.
9. **La cadena de descriptores del driver decía que una transferencia había ocurrido sin mover un byte.** La cabecera de la petición enlazaba directamente con el descriptor de estado cuando había carga. No apareció en K3 porque el driver de K3 construye sus propias cadenas; el protocolo de servicio es lo que convirtió la cabecera en código compartido.

## Cómo repetirlo

```sh
python3 tools/build_image.py --phase k4   # imagen K4 + manifiesto de digests
python3 tools/run_k4_cases.py             # los quince casos, cada uno sobre su propio medio
python3 tools/check_k4.py                 # veredicto por criterio
python3 tools/check_k4.py --self-test     # la puerta contra evidencia dañada
python3 tools/check_k4_format.py          # el formato y sus controles negativos
```

Un caso suelto, para mirar uno de cerca:

```sh
python3 tools/run_k4.py --out build/run-k4 --legs 2 --cut 1:AFTER_COMMIT:STOP
python3 tools/check_k4.py --cases build/k4-cases
```

Las regresiones de K1, K2 y K3 son parte de la puerta y se producen con sus propias herramientas:

```sh
python3 tools/build_image.py && python3 tools/run_k1.py && python3 tools/check_k1.py --json build/k1-gate.json
python3 tools/build_image.py --phase k2 && python3 tools/run_k2.py && python3 tools/check_k2.py --json build/k2-gate.json
python3 tools/build_image.py --phase k3 && python3 tools/run_k3.py && python3 tools/check_k3.py --json build/k3-gate.json
```

Si QEMU, OVMF o mtools no están instalados en el sistema, `THALYX_TOOL_PREFIX` apunta a un árbol que los contenga; `python3 tools/toolchain.py` informa de qué binario resolvió cada uno.
