---
id: EVD-005
kind: evidence
status: observed
---
# K2 — Objetos, autoridad y trabajo: qué se ejecutó y qué demuestra

Esta nota registra la primera ejecución del sistema de capacidades. K1 mostró que el kernel puede contener a un programa; esto muestra qué puede hacer un programa que no tiene permisos ambientales, y qué le pasa a la autoridad que reparte cuando alguien la cierra debajo de él.

Como en K1, describe lo observado y no lo que la arquitectura promete. Aquí la tentación es distinta y mayor: los mecanismos son numerosos, compilan todos, y una ejecución que termina sin quejarse invita a dar por bueno el conjunto. El kernel cuenta cuántas de las 51 operaciones asignadas alcanzó el despacho y **nombra las que no**: **51 de 51**, con cero registros `k2.operation_untouched`. La puerta lo exige como criterio propio, así que no puede volver a bajar en silencio.

Lo que ese número no dice es igual de importante: cada operación está ejercida **por un camino**, el que la vertical y sus controles recorren. Cobertura de interfaz no es cobertura de caminos, y la diferencia es la mayor limitación de esta evidencia.

## Qué se ejecutó

Una segunda imagen UEFI construida solo desde fuentes con el mismo script y el mismo kernel que la imagen K1. Lo único que cambia es el paquete de arranque, y el paquete es lo que elige la ruta: un módulo declarado `SUPERVISOR` lleva al kernel por el camino K2, su ausencia mantiene los dominios de K1. Que el binario del kernel sea el mismo es lo que permite decir «K1 sigue pasando» como afirmación sobre un kernel y no sobre dos.

| Artefacto | Comando | Resultado |
|---|---|---|
| Imagen | `python3 tools/build_image.py --phase k2` | `build/thalyx-k2.img`, manifiesto con digest de cada artefacto y herramienta |
| Ejecución | `python3 tools/run_k2.py` | `exit_status=33` (`k1.terminal status=complete`), 483 registros, ~1.7 s, `user_faults=0` |
| Veredicto | `python3 tools/check_k2.py` | `K2 GATE PASSED: 21 of 21 criteria met` |
| Autocomprobación | `python3 tools/check_k2.py --self-test` | `28 of 28 damaged runs were caught` |
| Cobertura | `k2.coverage` en el propio registro | `operations_assigned=51 operations_reached=51`, ningún `k2.operation_untouched` |
| Controles negativos | notas de los tres programas | 41 rechazos esperados en 13 estados distintos, ninguno inesperado y ninguno ausente |

Toolchain fijada: `rustc 1.98.1`, targets `x86_64-unknown-uefi` y `x86_64-unknown-none`, QEMU 11.1.1 con OVMF, perfil de CPU `qemu64,+smep,+smap,+pdpe1gb`, un solo núcleo, 512 MiB. El runner de K2 reutiliza el del K1 en lugar de repetir esos parámetros: si las dos fases corrieran en máquinas distintas, la comparación entre ellas mediría también el entorno.

La imagen K2 se reconstruyó byte a byte desde un árbol limpio: `cargo clean` seguido de la misma construcción produce los cuatro artefactos con los mismos digests. La reproducibilidad la aportan las mismas cuatro medidas que [K1](k1-protected-boot.md) tuvo que descubrir, y la comprobación se hizo forzando la reconstrucción, que es la única forma en que significa algo.

Los contadores temporales de una ejecución —ticks, número de preempciones, vueltas de espera— **no** son reproducibles: dependen del ritmo de emulación TCG. Ningún criterio de la puerta es una cifra exacta de esas.

Digests de la ejecución registrada:

```text
image   0268d76446a41c2acf297f7a261b7c741408a0586d655deaae9121582eb64385
kernel  c145a2dd514ae4c272790611f691ef4627b2759fbd91855cd9eec6a7d3bb0b49
loader  669c530b353b8aba9dfa58147167c6879765b3cbaeb109204c41764a1f7bea83
package 563e9b7c35064dbafaab0887371f54921221cd43dc5ec58bfd24bda57261ff83
```

## Qué observó la ejecución

### Creación sin privilegio ambiental

El kernel construyó un ámbito raíz, un log de control y **un** dominio. Ese dominio recibió cinco capacidades explícitas —él mismo, el ámbito del que colgarán sus hijos, el log, y una por imagen sellada— y ninguna otra. No hay llamada que abra un recurso por nombre: el supervisor localizó las imágenes preguntando a cada objeto sellado su etiqueta, no asumiendo un número de ranura.

Todo lo demás lo creó el supervisor a través de la interfaz: dos ámbitos hijos, dos endpoints, una señal, un objeto de memoria y dos dominios. El cliente quedó con dos capacidades —una faceta del endpoint y un buffer propio— y ninguna sobre ámbitos, dominios o el log. Ninguno de los dos hijos pudo activarse antes de tener canal de fallos.

### Autoridad que se estrecha y no se amplía

Tres derivaciones, ninguna más ancha que su padre. El cliente estrechó una capacidad de solo lectura sobre su propio buffer y se la entregó al servidor con la llamada. El servidor leyó por ella, no pudo escribir por ella y no pudo derivar de ella una versión con escritura. El supervisor tampoco pudo instalar en el cliente más derechos de los que su propia concesión llevaba. Dos intentos de ampliar, dos rechazos.

Una capacidad copiada llevó los mismos derechos y un handle distinto. Al cerrarla y volver a copiar, la ranura liberada volvió bajo un handle diferente, y el antiguo fue rechazado como handle inválido antes y después de que la ranura se reutilizara. Una capacidad derivada con un plazo ya vencido fue rechazada como expirada.

### Origen que el kernel afirma

Dos llamantes declararon un origen falso en el cuerpo de su petición. El kernel lo sustituyó por el real en ambos casos y ninguno de los valores declarados llegó a un recibo. El servidor leyó, en el encabezado que el kernel construyó, el mismo dominio de origen que el kernel registró al admitir el mensaje.

### Barrera, drenaje y retirada como tres cosas distintas

Con el servidor reteniendo trabajo, el supervisor cerró el ámbito del cliente. El registro de la barrera cuenta tres obligaciones pendientes: un hilo, una invocación y un efecto. Ese es el punto: una barrera que dejara los contadores a cero no distinguiría cerrar de drenar.

La retirada se pidió sobre el ámbito abierto y se rechazó como conflicto de estado. Se pidió de nuevo justo después de la barrera y 27 veces más durante el drenaje, y cada rechazo vino acompañado del informe que lo justifica: el kernel escribe ese informe y decide bajo un mismo cerrojo, de modo que la respuesta y su explicación no pueden discrepar sobre un perímetro que cambió en medio. Solo se concedió desde el estado quiescente.

### Alcance de la barrera y supervivencia de la obligación

Después de la barrera, la capacidad que el cliente había **delegado a otro dominio** dejó de funcionar: el servidor, que no está en el perímetro cerrado y seguía teniendo el handle, fue rechazado con ámbito cerrado. El propio cliente fue rechazado al intentar una nueva llamada.

La obligación, en cambio, sobrevivió. El efecto se admitió antes de la barrera reservando capacidad de cierre en el ámbito del **servidor** y no en el del cliente que estaba siendo cerrado, y se resolvió después de ella. El servidor observó la invocación como `origin_fenced` —no solo como origen muerto— porque el cliente cancelado sigue existiendo hasta que decide parar. La llamada del cliente volvió `CANCELLED`.

### Trabajo atribuible y recuperación

El servidor se vinculó al ticket antes de anunciar el efecto: adoptó el ámbito efectivo que el kernel había estampado en la invocación, de modo que el tiempo lo pagó quien pidió el trabajo y no se duplicó presupuesto. Después de la barrera se volvió a vincular, y esa segunda vinculación es de recuperación: la cuenta pasó al ámbito **del servicio**, contra la reserva de cierre que su propio efecto había apartado por adelantado. Los registros nombran las dos cuentas y son distintas.

### Reloj y expiración

V0 toma un plazo monotónico en seis operaciones. Esta ejecución añadió la entrada que faltaba para poder expresarlo: una lectura del reloj, sin handle, sin descriptor y sin autoridad, porque el paso del tiempo no es autoridad y negarse a exponerlo mientras se aceptan plazos no los hace seguros sino inservibles.

El supervisor leyó el reloj, armó un timer contra esa lectura, esperó a que levantara sus bits en una señal, comprobó que el reloj había avanzado y que el timer constaba disparado y desarmado. Armar con plazo cero se rechazó: cero es como esta interfaz escribe «sin plazo».

### Petición y respuesta ordinarias

Antes de nada de lo anterior, el cliente hace una petición corriente y el servidor la contesta. Todo lo demás en esta ejecución trata de lo que se rechaza o se cierra, y una vertical que nunca mostrara la interfaz funcionando sería un sitio raro desde el que sacar conclusiones. Responder resuelve la invocación: contestar dos veces se rechaza como ya resuelta, no sobrescribe un resultado sobre el que quien llamó pudo ya haber actuado.

### Publicación conservadora

El supervisor llenó un objeto de memoria, copió sus bytes a un segundo objeto dentro del kernel, mapeó el segundo con escritura en el dominio del cliente, y lo selló. El sello retiró el mapeo escribible antes de poder prometer nada: un escritor retirado, una página desmapeada, cero mapeos restantes. Después volvió a mapearlo de solo lectura y el cliente leyó sus bytes a través del mapeo, con una carga ordinaria.

Dos mapeos se rechazaron. Uno pedía escritura y ejecución a la vez, con el techo del objeto permitiendo ambas por separado, de modo que quien lo rechazó fue la regla W^X y no el techo —un control que falla por el motivo equivocado no comprueba nada—. El otro pedía escritura sobre el objeto ya sellado.

Ese es el camino conservador que el contrato de memoria pide: copiar, retirar al escritor, publicar el sello. No se entrega una página mutable llamándola inmutable.

### Presupuesto agregado y deuda

El planificador retuvo hilos cuyo ámbito había gastado su ventana; el registro de cada preempción dice si fue por eso. Veintiocho ventanas cerraron por encima del presupuesto y cada una dejó constancia del exceso que arrastraba a la siguiente: una deuda que nadie puede leer no es un límite, es un número. El supervisor comparó además su propio ámbito con uno de sus hijos y comprobó que lo que gasta el hijo también cuenta contra el padre; un presupuesto que solo acotara la hoja se evadiría creando hijos.

### Contabilidad

La retirada liberó lo que el perímetro patrocinaba y no retuvo nada: cero páginas y cero metadatos cargados a un ámbito retirado. Los tres dominios terminaron sin marcos a su nombre. El kernel acabó con 24 marcos propios y 129 254 libres.

### Límites y cierre disponible

Seis rechazos por límite agotado y uno por cola llena, cada uno después de admitir algo: el servidor llenó su tabla de capacidades hasta que el kernel se negó a agrandarla, y el supervisor llenó la cola del endpoint de supervisión hasta que la admisión se negó, con las celdas reservadas para los canales de fallo de sus hijos todavía libres.

Con el log de control saturado de recibos ordinarios y perdiendo, la barrera escribió igualmente su recibo, desde la reserva que el tráfico ordinario no puede ocupar. En ese mismo estado, cuatro admisiones cubiertas fueron rechazadas en lugar de ocurrir sin registro: el perfil auditado paga su cobertura por adelantado.

### Peticiones malformadas

Once peticiones estructuralmente inválidas, todas por el mismo camino que acepta las válidas: longitud declarada distinta de la del registro, encabezado nombrando otra operación, palabra reservada sucia, versión mayor futura, puntero de descriptor inalcanzable, bit de flag no asignado, código de operación no asignado, cuenta de capacidades excesiva, payload excesivo, operación de capacidad indefinida, y el mismo handle movido dos veces. Cuatro razones de rechazo distintas. Las últimas cuatro son transferencias: la capacidad que nombraban seguía en poder del emisor después, de modo que ninguna tuvo efecto parcial.

### Recibos leídos por capacidad

El contrato de observabilidad afirma dos cosas de este plano y hasta esta ampliación ninguna estaba comprobada desde fuera del kernel: que el log es alcanzable por capacidad, y que lo que un recibo dice sobre quién actuó lo decide el kernel y no el llamante. La ejecución escribía recibos y nadie los leía nunca.

El supervisor recorre el anillo entero, un lote acotado a la vez —cuatro recibos, que es lo que la interfaz fija—, reconociendo lo leído para que el más antiguo avance: reconocer es lo único que libera celdas. Lee 58 recibos, comprueba que las secuencias crecen estrictamente y que todos llevan el mismo esquema, y reconoce los 58; después el log informa `used=0` con su secuencia más antigua igual a la siguiente.

Entre ellos busca uno concreto: el que el propio guion escribió al empezar declarando un origen falso. Lo encuentra con el origen real estampado encima, y ningún recibo del log lleva el valor declarado en su campo de origen. El kernel ya registraba que había ignorado la declaración; esto es la mitad independiente de esa afirmación, leída por un programa.

Reconocer una secuencia ya liberada no suelta nada y no es un error: un lector que muriera entre leer y reconocer volverá a pedirlo, y responder a eso con un rechazo haría peligroso el reintento. Y leer y reconocer son derechos distintos: un handle estrechado a `LOG_READ` lee y no consigue reconocer, que es la separación que el contrato describe dejando de ser una afirmación sobre un bit que nadie había sostenido por separado.

### Barrera sobre una concesión, no sobre un ámbito

La vertical cierra un ámbito entero, que es el instrumento para un inquilino que debe irse. Retirar una sola delegación mientras el servicio sigue corriendo es otro instrumento, y el contrato los mantiene separados.

El supervisor deriva dos generaciones por debajo de su faceta y cerca la de en medio. La barrera marca dos nodos: el cercado y el derivado bajo él. Después, `CAP_INSPECT` informa de linaje cercado para los dos y vivo para la faceta de la que salieron, y derivar desde el nieto se rechaza con `SCOPE_CLOSED` mientras derivar desde la faceta sigue funcionando. El nieto conserva `DERIVE`, de modo que lo único que queda para rechazarlo es la barrera: un control que fallara por falta de derecho no comprobaría nada. El ámbito que patrocina las tres concesiones permanece abierto de principio a fin.

`CAP_DRAIN_STATUS` sobre el linaje cercado informa `FENCED` y nada pendiente, que es la respuesta correcta —la llamada que esa faceta llevó se resolvió mucho antes— y describe la concesión y no el ámbito, que habría contestado `OPEN`.

### Un dominio que se construye y se termina sin activarse nunca

Tres operaciones hablan de la construcción y del final de un dominio y no de su trabajo. Ejercerlas sobre el servidor o el cliente sería detener lo que está bajo prueba, así que el supervisor construye un cuarto dominio desde la imagen del cliente, en su propio ámbito, y no lo activa nunca.

`DOMAIN_QUERY` lo encuentra en construcción, con un hilo y sin fallos. Un punto de entrada que el dominio no tiene mapeado se rechaza con `INVALID_ARGUMENT` —el supervisor tiene `DOMAIN_BUILD` sobre él, así que lo que se observa es la dirección comprobándose y no la autoridad faltando—. Con una página propia mapeada, `DOMAIN_ADD_THREAD` crea un segundo hilo y la consulta lo confirma. `DOMAIN_TERMINATE` lo detiene; terminarlo otra vez es un conflicto de estado y no un segundo final, y añadirle un hilo después también: un dominio detenido ya no está en construcción, y el rechazo lo dice en lugar de culpar al punto de entrada.

`SIGNAL_QUERY` se ejerce donde corresponde, a ambos lados de la espera del timer: antes, el bit no está levantado, de modo que la espera que sigue no está satisfecha de antemano; después, la secuencia ha avanzado y el bit que la espera observó ya no está, porque esperar consume.

### Techos que bajan después

Un techo solo es un techo si puede bajarse más tarde; un supervisor que solo pudiera fijar límites al crear tendría que adivinar por adelantado todo lo que un hijo llegará a necesitar y concederlo entero. El ámbito `srv` se crea con 128 páginas y se estrecha a 96 antes de que se le cargue nada, de modo que el servidor se construye dentro del techo que el supervisor decidió y no dentro del que pidió primero.

Bajarlo está acotado en los dos sentidos y por dos reglas distintas que es fácil confundir. Hacia arriba, nada puede exceder lo que el padre tiene: pedir `u64::MAX / 2` páginas se rechaza con `LIMIT_EXHAUSTED`. Hacia abajo, nada puede caer por debajo de lo que el subárbol ya tiene cargado: pedir una página para el ámbito que sostiene toda la ejecución se rechaza con `STATE_CONFLICT`, porque un cargo contabilizado no puede convertirse en una deuda que nadie aceptó. El segundo control se le hace al padre y no al hijo a propósito: el hijo todavía no tiene nada cargado, y preguntarle si cero está por debajo de un límite lo aprobaría cualquier implementación.

## Cómo se decide la puerta

Los programas informan de lo que observaron y no informan de ninguna sorpresa. Eso es el relato del acusado. Sirve —solo el llamante sabe qué estado devolvió una llamada que el kernel rechazó— pero no decide nada por sí solo: un programa que nunca intentó una operación y otro cuyo intento se permitió indebidamente pueden terminar los dos sin quejarse.

Por eso cada criterio se decide desde los registros del propio kernel allí donde el kernel emite uno, y se evalúa por separado arrastrando las líneas que lo decidieron. Son **21**. De ellos, once leen además alguna nota de los programas, y cuatro —reloj, cierre disponible, límites y presupuesto— no pueden decidirse sin ella, porque lo que afirman es lo que el llamante observó al recibir una respuesta. Solo uno, el de controles negativos, lo dice en su título; esa convención está aplicada a medias y queda anotada abajo como lo que es.

El criterio de cobertura es de los que se deciden enteramente desde el kernel: lee el recuento de `k2.coverage` **y** la lista de `k2.operation_untouched`, y los compara entre sí. El kernel deriva los dos del mismo mapa de bits, así que un recuento que afirme el total mientras la lista todavía nombra algo es un kernel que cuenta mal, y fiarse de uno solo no lo notaría.

El comprobador se validó con **28** controles negativos que dañan la ejecución de una sola forma cada uno y nombran el criterio que debe notarlo. Los 28 lo hacen fallar. Las mutaciones son por patrón y no por literal: una que escribiera el daño con las cifras de una ejecución concreta dejaría de dañar algo en cuanto esas cifras cambiaran, y entonces informaría de éxito sobre una comprobación que ya no hace. Eso pasó dos veces durante K2 y es la razón de la regla.

Tres de las 28 son del criterio de cobertura: sin registro de cobertura, un recuento por debajo de lo que la interfaz asigna, y —la que importa— un recuento coherente consigo mismo mientras la lista de operaciones sin tocar lo contradice. Un comprobador que no falla nunca no es evidencia.

La regresión de K1 es un criterio de esta puerta. K2 creció dentro del kernel que arranca K1, y una puerta K2 que pasara mientras el arranque protegido se hubiera roto en silencio estaría midiendo otra cosa.

## Qué queda demostrado y qué no

| Experimento | Alcance cubierto por esta ejecución | Lo que falta |
|---|---|---|
| [EXP-01](../validation/experiments.md) | Completado en su parte K2: primer ring 3 del supervisor tras el manifiesto, dominios adversarios ya cubiertos en [K1](k1-protected-boot.md). | Nada de K2; el resto pertenece a fases posteriores. |
| [EXP-02](../validation/experiments.md) | Derivar, mover, copiar, expirar, reciclar ranuras y rechazar operaciones bajo un ámbito cerrado, en alcance UP. | Reinicio con handles persistidos, y el reciclado bajo presión concurrente, que es K3. |
| [EXP-03](../validation/experiments.md) | Cierre concurrente con un RPC en vuelo: barrera separada de drenaje, contadores no falsamente cero, resultado posterior admitido solo desde quiescente. | Servidor muerto durante la retención, y el resultado posterior con estado durable, que es K4. |
| [EXP-04](../validation/experiments.md) | Conservación de cargos en la retirada, presupuesto agregado que alcanza a los ancestros, deuda arrastrada y observada entre ventanas, reserva de cierre retenida y gastada en la cuenta del servidor. | CPU en SMP y mantenimiento, que es K3. |
| [EXP-06](../validation/experiments.md) | IPC malformado sin efecto parcial, agotamiento de tabla de capacidades, de cola y de log, con cierre disponible. | Agotamiento bajo concurrencia y con varios servidores, que es K3. |

Los invariantes que esta ejecución toca —autoridad no amplificada, origen no falsificable, admisión indivisible, barrera antes que drenaje, obligación que sobrevive al cierre, cargos conservados— quedan **observados en una vertical**, no probados en general. Las 51 operaciones están ejercidas; cada una lo está por un camino, y los mecanismos tienen más caminos que los que una ejecución recorre. Que la cobertura de interfaz sea completa hace más visible esa distinción, no menos: es exactamente lo que queda por ampliar.

Esta ejecución **no** demuestra: SMP, drivers propios, DMA, estado durable, Thalyx sobre este kernel, hardware físico ni rendimiento. Nada de eso está implementado. Sigue siendo una sola ejecución, en un emulador, con un núcleo.

El plano de diagnóstico de K1 sigue presente y sigue sin ser el plano de recibos. K2 añade el log de control, que sí reserva, sí es alcanzable por capacidad y sí rechaza operaciones que no puede registrar; las notas `user.note` que los programas emiten no son eso y la puerta las trata como lo que son. Las dos entradas de andamiaje de K1 permanecen, marcadas, sin tocar ningún objeto, para que la regresión siga ejecutándose.

El primer supervisor no tiene supervisor. Su fallo termina la ejecución, y el registro lo dice.

### Limitaciones de esta evidencia, explícitas

- **Un camino por operación.** Las 51 están ejercidas; ninguna lo está por más de un camino. La cobertura de interfaz es completa y la de caminos no, y esta ejecución no mide la segunda.
- **Cobertura contada en el despacho.** El kernel marca una operación como alcanzada al resolver su especificación, antes de comprobar derechos. Una operación que solo se hubiera intentado y rechazado contaría. Aquí no ocurre —las nueve que faltaban se ejercen con un camino que tiene éxito, y los rechazos son controles añadidos sobre él— pero el contador no es lo que garantiza eso; lo garantizan las comprobaciones que los programas hacen sobre el resultado.
- **El hilo del dominio de repuesto nunca corre.** `DOMAIN_ADD_THREAD` se ejerce completo —reserva, pila de kernel, tabla de hilos, ranura de paralelismo— sobre un dominio que se termina antes de activarse. Que un hilo añadido así *ejecute* correctamente no se observa aquí.
- **El linaje cercado no retenía nada.** `CAP_DRAIN_STATUS` informa `FENCED` con todos los contadores a cero, que es la respuesta correcta para ese estado. No se ha observado ese informe con trabajo realmente pendiente bajo la concesión cercada.
- **La convención de títulos de la puerta está a medias.** Once criterios leen notas de los programas y solo uno lo dice en su título. Los veredictos no son menos ciertos por ello, pero el lector no puede separarlos por el título como el propio comprobador dice que debería poder.
- **18 advertencias de clippy preexistentes** siguen en el árbol, todas de estilo y ninguna introducida por este trabajo: están en `arch/x86_64`, `diag.rs`, `mm/`, `scope.rs` e `ipcops.rs`. Las dos últimas son archivos K2, de modo que la afirmación anterior de «ninguna advertencia de clippy en ningún archivo K2» era falsa y queda corregida aquí.

## Qué corrigió ejecutar los mecanismos

Compilar las 51 operaciones no encontró ninguno de estos trece defectos; ejecutarlos los encontró todos. Los diez primeros aparecieron al construir la vertical; los tres últimos, al ampliarla hasta ejercer la interfaz entera.

1. El creador de un objeto de memoria recibía solo el techo de derechos del objeto, sin los derechos comunes, de modo que no podía inspeccionar, estrechar ni entregar lo que acababa de crear.
2. Un dominio no podía reclamarse receptor de un endpoint propio, así que ningún supervisor podía tener canal de fallos y ningún dominio gestionado podía activarse.
3. La barrera no alcanzaba a las concesiones patrocinadas por el ámbito cerrado: la autoridad que un dominio cercado ya había delegado seguía funcionando.
4. Cercar un ámbito con un hilo vivo lo dejaba permanentemente inejecutable mientras el planificador seguía creyendo posible el progreso, y la ejecución no terminaba.
5. El informe de drenaje contaba hilos despachados en lugar de hilos vivos, y declaraba quiescencia con un dominio del ámbito todavía en pie.
6. La retirada marcaba el ámbito y no liberaba nada, reportando como retenido lo que nadie había intentado liberar.
7. El despacho descartaba la respuesta de toda operación que rechazara, de modo que el informe de `SCOPE_RETIRE` —los contadores que `DRAIN_INCOMPLETE` manda mirar— nunca llegaba a quien tenía que mirarlos.
8. El límite de paralelismo de un ámbito estaba declarado, se reportaba en su informe y no se comprobaba nunca: los hilos propios de un dominio no tomaban ranura y ninguna toma consultaba el límite antes de ocuparla.
9. Una vinculación de recuperación cargaba el ámbito del cliente cerrado en lugar del ámbito del servicio. El contrato dice lo contrario y la ejecución mostró por qué importa: el ámbito cerrado puede tener reserva de cierre cero, y entonces el hilo que debía terminar la obligación quedaba permanentemente inejecutable. La reserva se comprobaba además contra lo gastado y no contra lo gastado más lo que otros efectos siguen reteniendo, de modo que era una reserva por adelantado solo de nombre.
10. La interfaz tomaba plazos monotónicos absolutos en seis operaciones y no ofrecía forma de leer el reloj, así que ninguno de esos plazos podía expresarse.

11. Las tres operaciones que hablan de un linaje —leer lo que permite, leer lo que un linaje cercado todavía retiene, y soltar el handle del llamante sobre él— pasaban por la misma puerta de resolución que todo lo demás, y esa puerta rechaza un linaje cercado o vencido: exactamente el estado del que cada una existe para informar o limpiar. Esto se midió en lugar de argumentarse. Con la puerta en su sitio, `CAP_DRAIN_STATUS` **no se alcanza nunca**: la ejecución termina en 50 de 51 con la operación todavía nombrada como no tocada, porque el `CAP_FENCE` del que informa es lo que impide llamarla. `CAP_INSPECT` sobre una concesión cercada se rechaza con `SCOPE_CLOSED`, así que los dos estados que su campo `lineage_state` existe para nombrar eran inalcanzables y las constantes del kernel para ellos estaban muertas. Y `CAP_CLOSE` sobre un linaje vencido se rechaza con `EXPIRED` —visible en el registro de la ejecución anterior, donde el cliente descarta ese resultado—, dejando un handle que su poseedor no puede liberar. La resolución se partió en dos para que el recorrido de vigencia sea lo único que esas tres se salten: tipo de objeto, derechos y objeto vivo siguen siendo obligatorios, y ninguna de las tres puede actuar sobre el objeto.
12. `DOMAIN_ADD_THREAD` colapsaba todo error de construcción en `LIMIT_EXHAUSTED`, de modo que «este dominio se ha detenido» volvía como «se agotó un límite»; y consultaba el espacio de direcciones antes que el estado, lo que informa de un punto de entrada no mapeado para un dominio cuya respuesta real es que ya no tiene espacio de direcciones. Un rechazo que llega con el motivo equivocado es peor que ninguno, porque invita a arreglar lo que no está roto.
13. `CapInfo.lineage_state` cruzaba la frontera como un entero cuyos valores solo nombraba el kernel, en constantes privadas de un módulo. El esquema los documentaba en prosa —«1 vivo, 2 cercado, 3 vencido»— y ningún programa podía referirse a ellos. Nadie lo había notado porque el campo era inobservable por el defecto anterior. `CapLineage` entra en el esquema y los dos lados usan los mismos nombres generados.

Los tres primeros son fallos de autoridad, los cinco siguientes de contabilidad, liveness e interfaz, y los cinco últimos agujeros en la propia interfaz: un plazo que no se podía expresar, tres operaciones que su propia puerta de resolución hacía inalcanzables, un rechazo que mentía sobre su motivo y un campo sin nombres. Ninguno era visible sin ejecutar.

La misma revisión convirtió en comprobación varios datos que el kernel registraba y nadie leía: la generación con la que un registro de mapeo nombra su objeto y su dominio, la coincidencia entre el objeto que nombra una entrada de capacidad y el que autoriza su concesión, y el endpoint que una invocación registró al ser admitida. Cada uno era estado redundante en un diseño basado en índices de tabla; ahora cada uno es un invariante que se verifica.

## Cómo repetirlo

```sh
python3 tools/build_image.py --phase k2   # imagen K2 + manifiesto de digests
python3 tools/run_k2.py                   # arranque en QEMU + captura serie
python3 tools/check_k2.py                 # veredicto por criterio
python3 tools/check_k2.py --self-test     # el comprobador contra ejecuciones dañadas
```

La regresión de K1 es parte de la puerta y se produce con las herramientas de K1:

```sh
python3 tools/build_image.py              # imagen K1, mismo kernel
python3 tools/run_k1.py
python3 tools/check_k1.py --json build/k1-gate.json
```

Si QEMU, OVMF o mtools no están instalados en el sistema, `THALYX_TOOL_PREFIX` apunta a un árbol que los contenga; `python3 tools/toolchain.py` informa de qué binario resolvió cada uno.
