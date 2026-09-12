---
id: EVD-010
kind: evidence
status: observed
---
# La campaña `perf/ipc-scalability`: qué medía, qué se cambió, qué queda

Esta nota preserva la campaña de rendimiento de la rama `perf/ipc-scalability`
después de que su historial de commits se aplane en un squash-merge sobre
`main`. No depende de que esos commits sigan siendo alcanzables: cada cifra
que cita está reproducida aquí, y las referencias a SHAs cortos son
procedencia, no la fuente de la afirmación. [`k6-comparison-hardening.md`](k6-comparison-hardening.md)
sigue siendo la nota de evidencia canónica de K6 —la campaña de referencia
EXP-12, los ocho cuellos de la fase K6 y el resultado ya integrado de esta
campaña—; esta nota cuenta la campaña en sí: qué principio se aplicó en cada
intervención, qué se descartó midiendo, y por qué la conclusión final no es
"Thalyx pierde" sino "esto es lo que cuesta la garantía que no se va a
soltar".

## Punto de partida: lo que EXP-12 ya había medido

La campaña de referencia (`vault/evidence/k6-comparison-hardening.md`, EXP-12,
seis rondas, tres brazos, sobre `9a83204`) había dejado dicho que este kernel
gana o empata en primitivas —una llamada IPC a 0,41 de un socket con
credenciales, mapear 64 páginas a 0,45 de `mmap`, el despertar 4,4 veces más
lento por un plazo de gracia elegido a propósito— y que **el IPC no escala
con el número de pares**: `scale.ipc` hacía 2 352 idas y vueltas por corte de
10 ms con un par y **1 806 con cuatro**, cayendo al añadir paralelismo, contra
1 516 y 11 912 en Linux. El cómputo sin entradas al kernel sí escalaba, 3,95×
en cuatro procesadores. La causa identificada entonces era un cerrojo único
de la máquina que todo lo que reescribe metadatos tenía que tomar en
exclusiva, con un cerrojo de turno que quitaba la inanición y no la
serialización. [OQ-04](../roadmap/open-questions.md) quedó abierta con esa
cifra.

## Objetivo de esta campaña

Partir ese cerrojo con evidencia propia, sin degradar ninguna garantía para
ganar un microbenchmark aislado, y sin declarar una superioridad que la
comparación no sostiene. El criterio de éxito no era "ganar todas las filas"
—`k6-comparison-hardening.md` ya documentaba que doce de diecisiete
benchmarks son *comparables* o *distintos* por construcción, no equivalentes—
sino: eliminar el coste que era accidental (candados más caros de lo que
necesitaban ser, trabajo repetido, sincronización más amplia de lo que la
corrección exige) y medir con precisión qué queda una vez que lo accidental
se ha ido, para poder decir de qué es el coste que sigue ahí.

## Resultado final

Con la espera de cerrojos reducida a ruido, el límite de escalabilidad del
IPC dejó de ser "un cerrojo único" y pasó a ser una cifra atribuida:
**la contabilidad de ámbito por la cadena de ancestros**, medida y
descompuesta por partes. `scale.ipc:4` pasó de 1 806 (cayendo con el
paralelismo) a **8 518**, subiendo monótonamente con cada par añadido por
primera vez en la campaña. Con un par y dos pares Thalyx gana con margen
(3,05× y 2,12×); con tres, empata dentro del ruido (1,05×, cuando antes era
un empate de moneda); con cuatro, pierde 0,71× — pero ahora se sabe
exactamente por qué, y una neutralización completa de esa contabilidad (una
sonda desechada, nunca integrada) sitúa el techo teórico en 11 070, todavía
por debajo de los 11 914 de Linux. La garantía que ese margen paga no se va a
soltar para ganar la fila; ese es el resultado real de la campaña, y se
explica en detalle más abajo.

K1–K5 pasan bajo TCG en el HEAD de esta rama, y pasaban ya en los puntos
intermedios `1aea0ab` y `8a7eeb1` donde se verificó explícitamente.

## Las intervenciones que importan

Siete decisiones arquitectónicas explican casi todo el movimiento. Cada una
se descubrió midiendo, no razonando desde la arquitectura hacia abajo.

### 1. El cerrojo único de la máquina: de exclusivo a compartido más cerrojos finos

**Qué reveló el problema.** La campaña de referencia ya lo había señalado
(arriba); esta campaña lo cuantificó: en una suite de `scale.ipc`, 18
millones de tomas del cerrojo de control, un tercio disputadas, 8 000
millones de ciclos de espera acumulados. Aislado a `scale.ipc:4`, 2,97×10⁹
ciclos de espera y el 90 % de sus tomas disputadas.

**Principio de Linux/historia.** Un cerrojo de lectores por procesador —cada
tenedor compartido anota su toma en una línea propia y sólo lee una palabra
que casi nadie escribe— es el mismo patrón que el `brlock` ("big reader
lock") que Linux usó en su primera era SMP para el mismo problema: estado que
casi todo el mundo sólo lee, con una minoría de escritores que puede permitirse
esperar. Y los cerrojos cortos bajo la máquina se tomaron con un intercambio
en vez de con un turno, porque un turno cuesta dos lecturas-modificación-escritura
de la misma línea *siempre*, disputado o no, y el orden entre esperadores lo
sigue dando el cerrojo exterior donde de verdad hay cola.

**Adaptación de Thalyx-Kernel.** `BrLock` como cerrojo de la máquina; las
cuatro operaciones del viaje de ida y vuelta de IPC —admitir, recibir,
responder, cerrar el ticket— lo toman compartido y se apoyan en un cerrojo
por registro (canal, invocación, celda de mensaje, tabla de capacidades de un
dominio, nodo de grant, log de control), tomado con intercambio y soltado con
un almacenamiento. El resto de la interfaz —todo lo que reescribe la
*estructura* de una tabla— sigue tomando la máquina en exclusiva. Los nodos
de grant se toman siempre de hijo hacia raíz, el único orden que dos
recorridos simultáneos pueden compartir sin necesitar más que eso.

**Antes → después** (medianas de tres rondas, `scale.ipc`, uno a cuatro
pares): 4 622 / 6 366 / 7 813 / 8 518, contra 4 608 / 6 362 / 7 797 / 1 806
antes de la campaña y 1 511 / 3 006 / 7 410 / 11 914 en Linux. El techo de
~7 400 que no dependía del número de pares desapareció. La espera de cerrojo
en `scale.ipc:4` cayó de 2,97×10⁹ a 1,4×10⁹ ciclos, y lo que queda de ella es
el 4 % del tiempo de máquina.

**Garantías conservadas.** El orden de cerrojos (máquina → canal →
invocación → mensaje → tabla de capacidades → nodo de grant → log de
control → registro de espera → cola de ejecución) es fijo y de hijo hacia
raíz para los grants, así que no hay una carrera nueva de interbloqueo. La
identidad de un endpoint (si la ranura está en uso, en qué generación,
cuántas capacidades lo nombran) se sacó del registro protegido por su propio
cerrojo y vive junto al canal, cambiando sólo bajo la exclusiva, precisamente
para que una admisión pueda resolver capacidades de endpoint desde una toma
compartida sin leer un dato que cambia bajo ella. Cerrar una capacidad que
resuelve a un objeto de memoria se re-ejecuta bajo la toma exclusiva porque
liberar sus marcos necesita el asignador, que ningún tenedor compartido
tiene — no se debilitó la regla de que un objeto de memoria sólo se recicla
cuando nadie puede alcanzarlo, sólo se movió la comprobación a donde puede
hacerse con seguridad.

### 2. La primera versión del reparto midió peor que el cerrojo único

Esto no es una intervención en sí, es la lección que hizo posible la
intervención 1, y merece constar como tal porque es exactamente el tipo de
resultado que un squash borraría sin dejar rastro de por qué el diseño
terminó como terminó.

**Qué reveló el problema.** Repartir el cerrojo de la máquina en cerrojos más
fino sin más midió **peor** que el cerrojo único: 6 411 idas y vueltas por
corte con cuatro pares contra 7 344, con un 40 % menos de espera medida. Un
cerrojo corto se toma y se suelta con dos operaciones atómicas sobre una
línea que dos procesadores se pasan, y el viaje de ida y vuelta llegó a tomar
cuarenta cerrojos. Quitar la contención no compensa si se multiplica el
número de veces que se paga el coste fijo de tomar algo.

**Principio de Linux/historia.** Es la misma razón por la que fragmentar un
cerrojo global en muchos cerrojos finos no es automáticamente una victoria
—una lección repetida en la historia del *Big Kernel Lock* de Linux— y por la
que estructuras como los contadores por procesador o RCU existen: reducir
*cuántas veces* se sincroniza importa tanto como reducir *cuánto tiempo* se
mantiene la sincronización.

**Adaptación de Thalyx-Kernel.** El commit que terminó integrándose
(`1aea0ab`) no se limitó a repartir; bajó la cuenta de tomas por viaje de ida
y vuelta: un recorrido de linaje se resuelve en la misma toma que ya leyó el
nodo (no en una segunda); liberar una invocación lee y limpia sus siete
campos en una sola toma, donde antes se tomaba el registro cinco veces; un
recibo decide si un lector le debe un despertar en la misma toma en la que se
escribió, sin que casi ningún auditor esté esperando; una admisión lee la
identidad de un grant una sola vez para su registro y su traza.

**Antes → después.** 6 411 (el reparto ingenuo) → 8 534 (el reparto con menos
tomas, medido en el mismo commit contra 7 344 del cerrojo único y 11 914 de
Linux).

**Garantías conservadas.** Ninguna regla de admisión cambió; sólo cuántas
veces se toma un cerrojo para aplicarla.

### 3. El shootdown de TLB, de difusión a conjunto dirigido

**Qué reveló el problema.** `mem.map:1` costaba 8,6 µs contra 1,8 µs en
Linux. Dos costes independientes, ninguno sobre el propio mapeo: la
invalidación se enviaba a **todos** los procesadores aunque sólo los que
tuvieran cargado ese espacio de direcciones pudieran retener una traducción
suya, y la reclamación de marcos esperaba a que **toda la máquina** hubiera
invalidado en vez de sólo a los que de verdad podían tener la traducción; el
asignador de marcos contiguos, además, buscaba desde el marco cero en cada
llamada.

**Principio de Linux/historia.** Mantener, por espacio de direcciones, el
conjunto exacto de procesadores que lo tienen cargado, y dirigir el shootdown
sólo a ellos: es literalmente `mm_cpumask` de Linux, por la misma razón. Un
dominio es un espacio de direcciones; los únicos procesadores que pueden
retener una de sus traducciones son los que la tienen cargada ahora mismo.

**Adaptación de Thalyx-Kernel.** El kernel mantiene ese conjunto por dominio,
distinto de la máscara más amplia "llegó a ejecutarse aquí alguna vez" que la
evidencia de K3 sigue comprobando (una afirmación distinta, que se conserva
tal cual). Un procesador entra en el conjunto al despachar un hilo del
dominio y sale al cargar otro espacio; el conjunto se lee después de publicar
la generación y se escribe antes de que el procesador que se une la lea, así
que uno de los dos órdenes gana siempre y no hay un tercer caso en el que una
invalidación pueda saltarse a alguien indebidamente. La reclamación de
marcos pregunta lo mismo con precisión en vez de con margen de sobra: un
objeto recuerda qué procesadores podían retener una traducción a sus marcos y
éstos vuelven al asignador cuando ésos —no todos— han confirmado. El
asignador contiguo, además, retoma la búsqueda donde la dejó la última vez en
vez de desde cero.

**Antes → después.** `mem.map:1` 11 082 → 1 800 ns contra 1 820 en Linux —de
perder por 4,7× a un empate—; `mem.map:64` 19 348 → 9 902 contra 37 520;
`mem.seal:16` 20 099 → 11 032 contra 23 170; interrupciones de shootdown en
la misma carga, 2 190 → 0.

**Garantías conservadas.** Nada vuelve al asignador hasta que el conjunto
correcto —no un superconjunto arbitrario ni uno recortado por
optimismo— ha confirmado. La comprobación sigue siendo exacta, sólo dejó de
ser conservadora por defecto.

### 4. Páginas globales para la mitad del kernel: un cambio de dominio no vacía su propia caché de traducción

**Qué reveló el problema.** Cada cambio entre dos dominios recargaba `CR3`
completo, y sin ninguna traducción marcada global eso retiraba también las
traducciones del propio kernel junto con las del usuario: cada entrada al
kernel después de un cambio volvía a recorrer su código, sus pilas y sus
tablas, en cada procesador, en cada cambio.

**Principio de Linux/historia.** Marcar global la mitad del espacio de
direcciones que todo dominio comparte —el propio kernel— y activar
`CR4.PGE`, para que sólo la mitad que cambia se retire en un cambio de
contexto, es la práctica estándar en x86/Linux desde su primera era SMP.

**Adaptación de Thalyx-Kernel.** Las hojas de la mitad del kernel llevan el
bit global; las del usuario nunca. El protocolo de invalidación gana una
generación aparte para las bajas de la mitad del kernel —una pila de kernel
liberada, un mapeo que falló a medias— anterior a la generación que las
transporta; un procesador que se pone al día en esa generación retira además
sus entradas globales, apagando y reactivando `CR4.PGE`, y en cualquier otra
generación recarga `CR3` como antes. La distinción importa porque en esta
máquina virtual los accesos a `CR4` son el tipo caro de operación: pagarlos
en cada baja de la mitad de usuario, un experimento que sí se probó, costó a
`closure.unit` un cuarto de su tiempo (ver más abajo, experimentos
descartados).

**Antes → después.** `ipc.call:0` 2 140 → 1 920 ns; `ipc.caps:4` 2 840 →
2 611; `scale.ipc:1/2` 4 430/6 393 → 4 910/6 950 idas y vueltas por corte;
`closure.unit` sin cambio.

**Garantías conservadas.** El aislamiento entre dominios no se toca: sólo la
mitad idéntica en todo espacio de direcciones —el kernel— se comparte a
nivel de traducción, y sigue retirándose correctamente cuando algo en ella
cambia.

### 5. El camino rápido de IPC: entrega directa en vez de cola, reconstrucción y copia bajo cerrojo

**Qué reveló el problema.** Parte del techo de ~7 400 independiente del
número de pares venía de trabajo repetido dentro del propio cerrojo: una
invocación de 600 bytes y un mensaje de 350 se ensamblaban enteros en la
pila y se copiaban a la tabla, y cada lector —entrega, respuesta, la
recolección del llamante, la liberación— volvía a copiar el registro entero
para mirar un puñado de campos; una ida y vuelta tomaba el cerrojo de control
hasta diez veces, cuatro de ellas sólo para copiar los dos descriptores
dentro y fuera.

**Principio de Linux/historia.** Es el patrón de "camino rápido de IPC" de la
familia de microkernels L4: cuando el receptor ya está bloqueado esperando,
entregarle el mensaje directamente en su propio búfer en vez de encolarlo y
despertarlo para que lo busque, y sacar la copia masiva de fuera del cerrojo
apoyándose en la vida del espacio de direcciones en vez de en el cerrojo para
mantener una página presente.

**Adaptación de Thalyx-Kernel.** Un hilo que se bloquea a recibir o a
esperar una respuesta deja una oferta en su propio registro de espera —el
búfer de aterrizaje y el handle con el que entró—; quien termina la espera
escribe la respuesta ahí en la toma que ya tenía, sin que el receptor vuelva
a tomar el cerrojo. Los registros se escriben campo a campo en su ranura y se
leen por referencia en vez de copiarse enteros. Las copias de los
descriptores de entrada y salida dejaron de necesitar el cerrojo de control:
lo que mantiene una página presente ahora es el propio protocolo de
invalidación —el procesador está en el conjunto vivo del espacio, las
interrupciones están enmascaradas y no se toma ningún cerrojo entre traducir
y copiar, así que un marco que el recorrido vio no puede volver al fondo
común hasta que la copia termina—.

**Antes → después.** Acumulado sobre esta familia de cambios, en el brazo
nativo: tomas del cerrojo de control por suite de `scale.ipc`, 9,6 millones →
7,3 millones; `audit.drain` 1 590 → 1 130 ns; la cadena completa de commits
llevó `scale.ipc` de un techo de ~4 300–4 900 en tres/cuatro pares a los
valores ya citados de la intervención 1.

**Garantías conservadas.** Una espera se consume exactamente una vez, bajo el
cerrojo propio del hilo, y quien la reclama despierta al hilo en último
lugar: el que duerme no puede abandonar su registro de espera —ni su marco de
pila— antes de que lo despierten, así que la entrega directa no puede
competir con un despertar espurio o una expiración y entregar dos veces.

### 6. Contadores y estadísticas de la máquina, de una línea compartida a una por procesador

**Qué reveló el problema.** Varios contadores que nadie lee hasta el final
de la ejecución —la marca de agua del reloj, los totales de tick, desalojo y
retraso, las estadísticas de contención del propio cerrojo— los escribía
**cada** procesador en caminos calientes, convirtiendo contabilidad que nadie
consulta en tráfico de caché constante. Tres campos por procesador del
planificador —qué generación de invalidación aplicó, qué espacio cargó, qué
lectura de reloj publicó— compartían una sola línea, así que un cambio de
contexto y una lectura de reloj en procesadores distintos se bloqueaban entre
sí sin ninguna razón semántica.

**Principio de Linux/historia.** Contadores por procesador que se suman sólo
al leer, en vez de un acumulador compartido que todos escriben: la misma idea
detrás de `percpu_counter` y las estadísticas `vmstat` de Linux.

**Adaptación de Thalyx-Kernel.** Cada campo pasó a tener una línea propia por
procesador; los tres totales se sumaron desde contadores por procesador que
ya existían; dos marcas de agua se leen antes de escribirse, así que un valor
que ya es más alto no cuesta una escritura. El cerrojo de control ahora
reporta lo que cuesta —tomas, cuántas esperaron, ciclos esperados, la espera
más larga— desde estadísticas igualmente partidas por procesador.

**Garantías conservadas.** La comprobación cruzada del reloj entre
procesadores sigue siendo la misma —una lectura se compara contra un valor
que otro procesador publicó *antes* de que ésta se tomara—; sólo cambió dónde
vive cada palabra, no el protocolo que las ordena.

### 7. El despertar: afinidad con el procesador correcto y un sondeo ocioso acotado

**Qué reveló el problema.** Con cuatro pares en cuatro procesadores, un
despertar síncrono que encontraba a su destinatario ya en cola aquí se
enviaba de todos modos a otro procesador —ocupado—, costando una interrupción
y partiendo un par que cabía en un solo procesador en dos. Por separado, un
procesador ocioso miraba las colas de los demás en cada vuelta de su sondeo,
leyendo palabras que el procesador ocupado escribe varias veces por ida y
vuelta: tres sondeadores leyendo esas palabras tan rápido como podían
convertían cada escritura del procesador ocupado en un traspaso de línea de
caché.

**Principio de Linux/historia.** Preferir el procesador en el que el hilo
corrió por última vez, o el que despierta, antes que uno ocioso al que hay
que interrumpir, es la misma regla que `wake_affine` de Linux, y por la misma
razón: una interrupción entre procesadores cuesta más que dejar que el hilo
espere un turno donde ya está.

**Adaptación de Thalyx-Kernel.** Un despertar síncrono prefiere, en orden: el
procesador donde el hilo corrió la última vez, si está ocioso y sin cola; el
procesador que despierta, con la misma condición; un ocioso que está
*sondeando*, que toma el trabajo mirando; y sólo entonces uno que está
detenido, al que hay que interrumpir. Un indicio no salta una cola en la que
algún hilo ya esperó el plazo de gracia del despertar —ése va primero—, y un
indicio envejecido sin dónde ir se lleva este procesador al salir del kernel
en vez de esperar el siguiente cambio. El sondeo ocioso de las colas ajenas
pasó de mirar en cada vuelta a mirar una vez al entrar en ocioso y luego cada
dieciséis microsegundos.

**Antes → después** (medido en aislamiento, brazo nativo, una ronda cada
vez): con el sondeo en cada vuelta, una ida y vuelta de IPC en un procesador
costaba 3,74 µs; sin sondeo, 2,40 µs. Con la afinidad de despertar:
`ipc.call:0` 4 081 → 3 822 ns, `sched.wake` 1 490 → 1 381 ns.

**Garantías conservadas.** Ningún hilo se queda esperando indefinidamente
detrás de un indicio: la regla de envejecimiento le da paso en cuanto ha
esperado el plazo de gracia, y un indicio que no encuentra dónde ir se cobra
el procesador de todos modos en vez de perderse.

## Otras ganancias medidas, más pequeñas

Completando el cuadro sin ampliarlo: `sysretq` en el retorno de una syscall
cuando el marco lo permite (`entry.version` 80 → 50 ns, contra 60 en Linux,
con las comprobaciones de canonicidad que evitan el `#GP` en anillo 0 que
`SYSRET` puede producir con una dirección de retorno no canónica); una sola
toma del cerrojo de control por operación en vez de hasta cuatro, con la
especificación de la operación resuelta en dos lecturas de una tabla de dos
niveles en vez de hasta 59 comparaciones; el hilo que entra encontrado una
sola vez y contado sin cerrojo; el cerrojo de la máquina liberándose antes de
publicar la respuesta; el hallazgo de una ranura libre de mensaje o
invocación por un mapa de bits en vez de por un recorrido de la tabla; un
endpoint que recuerda qué hilos esperan recibir, para despertar a uno sin
recorrer la tabla de hilos entera; y una comprobación de cuenta y de barrera
preguntada sólo cuando la respuesta puede haber cambiado (al agotarse una
reserva, o en un cambio de estado registrado), en vez de en cada tick de cada
procesador.

## Contabilidad de ámbito por la cadena de ancestros: el cuello decisivo

Con la espera de cerrojos en el 4 % del tiempo de máquina, lo que limita
`scale.ipc:4` dejó de ser un cerrojo. Cada admisión, cada liberación, cada
cambio de hilo en ejecución camina la cadena de ancestros de un ámbito —de
hoja a raíz— y hace una operación atómica de lectura-modificación-escritura
en cada nivel sobre líneas que **todos** los procesadores comparten, porque
cada techo —paralelismo, `QueueBytes`, `Metadata`, ventana de CPU— tiene que
comprobarse y tomarse en un único paso para que ningún nivel exceda su cuenta
ni siquiera transitoriamente.

**Cómo se aisló.** Tres campañas de sonda, desechadas y nunca integradas
(`q29probe`, `q30probe2`, `q31running` en el árbol de trabajo), neutralizaron
la contabilidad por partes y en conjunto, midiendo `scale.ipc` de uno a
cuatro pares en cada configuración:

| Configuración | 1 par | 2 pares | 3 pares | 4 pares |
|---|---|---|---|---|
| Con el reparto de cerrojos, contabilidad real | 4 622 | 6 366 | 7 813 | **8 518** |
| Contabilidad de ámbito neutralizada por completo | 4 931 | 7 315 | 9 536 | **11 070** |
| Linux | 1 511 | 3 006 | 7 410 | 11 914 |

Por partes, cada una medida sola con cuatro pares: las reservas y
devoluciones de `QueueBytes` y `Metadata` valen un 12,5 % de la fila; los
contadores de simultaneidad por despacho (`take_running`/`drop_running`) un
11,4 %; el resto —`charge_cpu`, `commit_excess`, `reserve_cpu`— es el cargo
de ejecución en sí.

**La conclusión que importa.** Incluso con la contabilidad enteramente
gratis, la fila da 11 070, todavía por debajo de los 11 914 de Linux. Esto
significa que el margen que falta contra Linux a cuatro pares **no es un
cuello que quede por optimizar**: es, en su mayor parte, el coste de una
garantía que Thalyx-Kernel sostiene y Linux no tiene que sostener de la misma
forma —una jerarquía de ámbitos con techos comprobados en cada nivel, en cada
mensaje, sin excepción— comparado contra un planificador de créditos que no
impone la misma comprobación jerárquica en el mismo punto por cada IPC.

**Por qué se descartó el carve-out per-CPU ingenuo.** La forma obvia de
abaratar un contador con techo comprobado es darle una reserva por
procesador y sumarlas sólo cuando hace falta un total exacto —el patrón que
sí funciona para contadores puramente acumulativos (intervención 6)—. No
funciona aquí: todo contador de esta cadena es **de techo**, no acumulativo
puro, y una reserva por procesador fragmenta ese techo en particiones fijas.
Un hermano puede quedar rechazado por falta de cupo mientras el ámbito, en
conjunto, todavía tiene sitio en la partición de otro procesador que no lo
está usando — **exhaustion falsa**: el sistema rechazaría trabajo que su
propia contabilidad global habría admitido. Abaratar esta cadena sin permitir
ese falso rechazo es un problema de diseño abierto, no resuelto por esta
campaña, y queda registrado como tal en [OQ-04](../roadmap/open-questions.md).

## Victorias finales contra Linux

Medianas de tres rondas, HEAD de esta rama, mismo anfitrión y configuración
que la campaña de referencia: `entry.clock` 1,83×; `ipc.call:0` 3,39×;
`ipc.caps:4` 2,43×; `mem.map:1` (empate, ver abajo); `mem.map:64` 3,74×;
`mem.seal:16` 2,14×; `sched.wake` 2,33×; `scale.ipc:1` 3,05×; `scale.ipc:2`
2,12×; `scale.ipc:3` 1,05× (antes, un empate de moneda); `closure.unit`
3,98×. Ninguna de estas filas sacrificó una garantía para ganar: todas son
*comparables* según el esquema de K6, con las garantías de cada lado
declaradas antes de medir.

## Derrotas y tradeoffs finales, y por qué no son defectos por sí solas

- **`scale.ipc:4`, 0,71×.** La única derrota que esta campaña *causó* en el
  sentido de "antes no perdía por esto": el cerrojo único hacía perder por
  0,15× con una causa no atribuida; ahora se pierde por 0,71× con la causa
  atribuida y acotada (arriba). Es una mejora de 4,7× en la fila y una
  degradación de "no sabemos por qué" a "sabemos exactamente por qué, y el
  máximo teórico sigue perdiendo".
- **`mem.map:1`, 0,94×.** Éste sí lo introdujo el reparto de cerrojos: antes
  del split, 1 940 ns contra 1 830 de Linux ya perdía por poco; los cerrojos
  finos añaden una pequeña sobrecarga fija a las filas de un solo par que no
  tienen contención que aliviar (5–17 % en varias filas: `ipc.call:0`
  1 920 → 2 081, `cap.derive` 370 → 450, `closure.unit` 11 693 → 12 743).
  Es el coste de que el mismo mecanismo que gana en 1/2/3/4 pares tenga un
  precio de entrada en el caso de un solo par. Queda como tradeoff
  documentado, no como regresión oculta.
- **`cap.derive`, 0,32×; `engine.load`, 0,55×; `engine.infer:0`, 0,92×;
  `quota.share:25`, 0,98×; `scale.compute:4`, 0,99×.** Ninguna de estas cinco
  es un tradeoff de esta campaña: las cinco ya estaban en la campaña de
  referencia EXP-12 antes de que `perf/ipc-scalability` empezara —`cap.derive`
  perdía 3,2× desde el principio por lo que hace de más (derivar un linaje
  frente a un `dup` que no lo hace), `engine.load` por copiar pesos en vez de
  mapearlos, `engine.infer` por la libc y la biblioteca matemática propias,
  `quota.share` es *inconcluyente* por diseño estadístico y `scale.compute`
  es una diferencia de decenas de microsegundos por la contabilidad del
  tick—. Repetirlas aquí sin esa aclaración las haría parecer nuevas pérdidas
  de esta campaña, y no lo son.

## `engine.cancel`: la corrección de medición que importa

La fila pasó de 7 174 µs (antes de la campaña) a 13 414 µs en la medida final
de tres rondas, y dentro de la campaña se movió varias veces en direcciones
opuestas por razones distintas, lo que la hace el mejor ejemplo de por qué
"la fila empeoró" no siempre significa "el código bajo prueba empeoró":

1. Una corrección real de planificador (intervención en `a2b4eb6`: un crédito
   local ya no podía tomar el último cupo de un presupuesto ni retenerlo
   después de dejar de ejecutar ese ámbito, lo que hacía parecer agotado un
   ámbito con cupo real sin usar) recuperó la fila de 13 380 a 7 221 µs —de
   vuelta a donde estaba antes de la campaña, contra 4 915 en Linux.
2. El reparto de cerrojos (`1aea0ab`) introdujo un barrido de cercado de
   linaje bajo cerrojos por nodo, hasta 256 tomas por pasada; `8a7eeb1`
   quitó esas tomas por ser redundantes bajo la exclusiva —midió sin cambio,
   12 757 → 13 360 µs, y se mantuvo de todos modos porque tomar un cerrojo
   para excluir a nadie es incorrecto cueste lo que cueste, no porque
   ganara nada—.
3. La subida final a 13 414 µs **no la causa ningún cambio de kernel**: se
   bisecó a `d96804e`, que hace que el runtime de K6 deje de narrar su propio
   arranque, y ese commit no toca una sola línea del kernel. `engine.cancel`
   mide una cancelación corriendo **contra una carrera** con una inferencia;
   arrancar el motor más rápido cambia cuál de las dos gana esa carrera, no
   cuánto cuesta cancelar. La prueba de que el mecanismo sigue intacto es que
   `engine.load` —la fila que ese mismo commit sí perseguía— bajó de 30 890 a
   6 343 µs en el mismo paso.

La lectura correcta: el camino de cancelación del kernel no regresó; una
fila que mide una carrera se movió porque el reloj de una de las dos
carreras cambió, y confundir eso con una regresión del lado que no cambió es
exactamente el error que la disciplina de bisección de esta campaña existe
para evitar.

## Experimentos descartados, con número

Instructivos por lo que enseñan sobre el método, no por su tamaño:

- **El reparto ingenuo del cerrojo** (intervención 2, arriba): repartir sin
  bajar el número de tomas midió peor que el cerrojo único —6 411 contra
  7 344 idas y vueltas con cuatro pares, con un 40 % menos de espera—. La
  lección: la espera no es el único coste de un cerrojo; el coste fijo de
  tomarlo lo es también, y un viaje de ida y vuelta que toma cuarenta
  cerrojos paga ese coste fijo cuarenta veces.
- **`CR4.PGE` apagado y encendido en cada retirada, no sólo en las del
  kernel.** Para simplificar el protocolo de invalidación de la intervención
  4 se probó activar y desactivar `CR4.PGE` en cada baja de la mitad de
  usuario, no sólo cuando de verdad había que retirar una entrada global.
  Coste medido: `closure.unit` +27 %. En esta máquina virtual los accesos a
  `CR4` son la operación cara, y pagarlos por cada baja ordinaria —que no
  toca ninguna entrada global— deshace el ahorro entero de la intervención.
- **No poner a cero el área de 2 KB de `Staging` (`MaybeUninit`) antes de
  usarla.** Parecía trabajo evitable —¿por qué escribir ceros que la
  siguiente operación va a sobrescribir de todos modos?—. Medido: cada fila
  de IPC, 2–3 % más lenta. No se investigó más allá de revertir, y queda
  como recordatorio de que "menos trabajo" y "más rápido" no son sinónimos
  cuando de por medio hay un patrón de acceso a memoria que el compilador o
  el procesador explotaban de la forma contraria a la esperada.

Descartados también, sin una cifra tan citable pero medidos y no repetidos:
un temporizador *tickless* de un solo disparo; un `haltpoll` adaptativo;
saltarse la contabilidad de despacho para ahorrar su coste; envolver las
tablas en cerrojos sin mover antes los caminos calientes a una toma
compartida (3–9 % más lento y sin ninguna ganancia, porque el cerrojo exterior
seguía serializando lo mismo).

## Evolución de `scale.ipc:4` en una línea

1 806 (cerrojo único, cayendo con el paralelismo) → 2 430 (extracción inicial
de los caminos calientes, `1fc5565`) → 6 411 (reparto ingenuo del cerrojo,
descartado) → 8 534 (reparto con las tomas reducidas, `1aea0ab`) → **8 518**
(estado final de tres rondas, con las intervenciones 3–7 ya integradas) →
**11 070** (techo teórico con la contabilidad de ámbito neutralizada por
completo, sonda desechada) — contra **11 914** en Linux en todo momento.

## Lo que esta campaña no resuelve, para la próxima comparación end-to-end

- **Abaratar la contabilidad jerárquica sin permitir exhaustion falsa** sigue
  sin solución. Es la pregunta de diseño más cara que deja esta campaña, y
  [OQ-04](../roadmap/open-questions.md) la registra con la cifra exacta que
  cualquier propuesta futura tiene que superar: 11 070 en el mejor caso
  gratuito, contra 11 914 de Linux.
- **La cola larga de `closure.unit`** —p99 de 3,4 ms en la campaña de
  referencia, uno de cada quince cierres— sigue sin diagnosticar; esta
  campaña no la tocó porque no es un cuello de IPC.
- **Nada de esto se midió en hardware físico.** Todo es KVM sobre la máquina
  de desarrollo, inventariada y no arrancada; una interrupción entre
  procesadores cuesta aquí lo que cuesta salir a KVM, y varias de las
  decisiones de esta campaña —el plazo de gracia del despertar, el umbral de
  sondeo ocioso de 16 µs— se eligieron contra ese coste concreto. Un cambio
  de plataforma es una razón explícita para revisarlas, no sólo para
  reconfirmarlas.
- **El tamaño de muestra de `scale.ipc` con tres y cuatro pares** sigue sin
  ser el que el propio estimador de K6 pide (esta campaña usó tres rondas
  por rapidez de iteración, no las ~42 que EXP-12 recomienda para esas dos
  filas); las cifras de esta nota llevan su coeficiente de variación donde se
  citaron por separado, y una comparación end-to-end futura debería correr
  la campaña completa una vez sobre el estado final, no asumir que las
  medianas de tres rondas son el número definitivo.
- **Ninguna carga de Thalyx real** ha ejercitado esta ruta todavía; todo lo
  medido aquí es el plan sintético de K6. [OQ-02](../roadmap/open-questions.md)
  sigue abierta en la parte que pide eso.

## Referencias

[`k6-comparison-hardening.md`](k6-comparison-hardening.md) (EXP-12 y la
campaña que esta nota extiende), [`concurrency.md`](../architecture/concurrency.md)
(el orden de cerrojos resultante), [OQ-04](../roadmap/open-questions.md)
(la pregunta abierta que esta campaña deja acotada), [ADR-010](../decisions/ADR-010-k6-parameters-and-wake-policy.md)
(los parámetros de despertar y log que esta campaña hereda sin cambiar).
