---
id: EVD-005
kind: evidence
status: observed
---
# K2 — Objetos, autoridad y trabajo: qué se ejecutó y qué demuestra

Esta nota registra la primera ejecución del sistema de capacidades. K1 mostró que el kernel puede contener a un programa; esto muestra qué puede hacer un programa que no tiene permisos ambientales, y qué le pasa a la autoridad que reparte cuando alguien la cierra debajo de él.

Como en K1, describe lo observado y no lo que la arquitectura promete. Aquí la tentación es distinta y mayor: los mecanismos son numerosos, compilan todos, y una ejecución que termina sin quejarse invita a dar por bueno el conjunto. La ejecución cubre una vertical y sus controles; no cubre los 60 manejadores del esquema.

## Qué se ejecutó

Una segunda imagen UEFI construida solo desde fuentes con el mismo script y el mismo kernel que la imagen K1. Lo único que cambia es el paquete de arranque, y el paquete es lo que elige la ruta: un módulo declarado `SUPERVISOR` lleva al kernel por el camino K2, su ausencia mantiene los dominios de K1. Que el binario del kernel sea el mismo es lo que permite decir «K1 sigue pasando» como afirmación sobre un kernel y no sobre dos.

| Artefacto | Comando | Resultado |
|---|---|---|
| Imagen | `python3 tools/build_image.py --phase k2` | `build/thalyx-k2.img`, manifiesto con digest de cada artefacto y herramienta |
| Ejecución | `python3 tools/run_k2.py` | `exit_status=33` (`k1.terminal status=complete`), 409 registros, ~1.6 s |
| Veredicto | `python3 tools/check_k2.py` | `K2 GATE PASSED: 19 of 19 criteria met` |
| Autocomprobación | `python3 tools/check_k2.py --self-test` | `22 of 22 damaged runs were caught` |

Toolchain fijada: `rustc 1.98.1`, targets `x86_64-unknown-uefi` y `x86_64-unknown-none`, QEMU 11.1.1 con OVMF, perfil de CPU `qemu64,+smep,+smap,+pdpe1gb`, un solo núcleo, 512 MiB. El runner de K2 reutiliza el del K1 en lugar de repetir esos parámetros: si las dos fases corrieran en máquinas distintas, la comparación entre ellas mediría también el entorno.

La imagen K2 se reconstruyó byte a byte desde un árbol limpio: `cargo clean` seguido de la misma construcción produce los cuatro artefactos con los mismos digests. La reproducibilidad la aportan las mismas cuatro medidas que [K1](k1-protected-boot.md) tuvo que descubrir, y la comprobación se hizo forzando la reconstrucción, que es la única forma en que significa algo.

Los contadores temporales de una ejecución —ticks, número de preempciones, vueltas de espera— **no** son reproducibles: dependen del ritmo de emulación TCG. Ningún criterio de la puerta es una cifra exacta de esas.

Digests de la ejecución registrada:

```text
image   f147d2988051e3a1d49029b75076c1c88577956c0b8540103a0c05b60c1881af
kernel  884bbed3105319561b4d666643476add1299c3ff8bcab3acc3a1e21c35d97a75
loader  669c530b353b8aba9dfa58147167c6879765b3cbaeb109204c41764a1f7bea83
package 54f4236b98dac057bccf9f89e672f1d305559289001c57ca4d784ade701c8a9a
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

### Presupuesto agregado y deuda

El planificador retuvo hilos cuyo ámbito había gastado su ventana; el registro de cada preempción dice si fue por eso. Veintiocho ventanas cerraron por encima del presupuesto y cada una dejó constancia del exceso que arrastraba a la siguiente: una deuda que nadie puede leer no es un límite, es un número. El supervisor comparó además su propio ámbito con uno de sus hijos y comprobó que lo que gasta el hijo también cuenta contra el padre; un presupuesto que solo acotara la hoja se evadiría creando hijos.

### Contabilidad

La retirada liberó lo que el perímetro patrocinaba y no retuvo nada: cero páginas y cero metadatos cargados a un ámbito retirado. Los tres dominios terminaron sin marcos a su nombre. El kernel acabó con 24 marcos propios y 129 254 libres.

### Límites y cierre disponible

Seis rechazos por límite agotado y uno por cola llena, cada uno después de admitir algo: el servidor llenó su tabla de capacidades hasta que el kernel se negó a agrandarla, y el supervisor llenó la cola del endpoint de supervisión hasta que la admisión se negó, con las celdas reservadas para los canales de fallo de sus hijos todavía libres.

Con el log de control saturado de recibos ordinarios y perdiendo, la barrera escribió igualmente su recibo, desde la reserva que el tráfico ordinario no puede ocupar. En ese mismo estado, cuatro admisiones cubiertas fueron rechazadas en lugar de ocurrir sin registro: el perfil auditado paga su cobertura por adelantado.

### Peticiones malformadas

Once peticiones estructuralmente inválidas, todas por el mismo camino que acepta las válidas: longitud declarada distinta de la del registro, encabezado nombrando otra operación, palabra reservada sucia, versión mayor futura, puntero de descriptor inalcanzable, bit de flag no asignado, código de operación no asignado, cuenta de capacidades excesiva, payload excesivo, operación de capacidad indefinida, y el mismo handle movido dos veces. Cuatro razones de rechazo distintas. Las últimas cuatro son transferencias: la capacidad que nombraban seguía en poder del emisor después, de modo que ninguna tuvo efecto parcial.

## Cómo se decide la puerta

Los programas informan de lo que observaron y no informan de ninguna sorpresa. Eso es el relato del acusado. Sirve —solo el llamante sabe qué estado devolvió una llamada que el kernel rechazó— pero no decide nada por sí solo: un programa que nunca intentó una operación y otro cuyo intento se permitió indebidamente pueden terminar los dos sin quejarse.

Por eso catorce de los dieciséis criterios se deciden desde los registros del propio kernel, y los dos que necesitan las notas de los programas lo dicen en su título. Cada criterio se evalúa por separado y arrastra las líneas que lo decidieron.

El comprobador se validó con dieciséis controles negativos que dañan la ejecución de una sola forma cada uno: sin registro de barrera, barrera sin nada pendiente, una derivación que amplía derechos, sin retirada, retirada que no libera nada, retirada que deja un cargo, sin efecto admitido, sin invocación resuelta, un origen declarado llegando a un recibo, nada rechazado, sin memoria liberada, una operación rechazada reportada como exitosa, un programa que no termina su guion, un segundo dominio construido por el kernel, sin manifiesto de arranque, y un hueco en la secuencia de registros. Los dieciséis hacen fallar al criterio que les corresponde. Un comprobador que no falla nunca no es evidencia.

La regresión de K1 es un criterio de esta puerta. K2 creció dentro del kernel que arranca K1, y una puerta K2 que pasara mientras el arranque protegido se hubiera roto en silencio estaría midiendo otra cosa.

## Qué queda demostrado y qué no

| Experimento | Alcance cubierto por esta ejecución | Lo que falta |
|---|---|---|
| [EXP-01](../validation/experiments.md) | Completado en su parte K2: primer ring 3 del supervisor tras el manifiesto, dominios adversarios ya cubiertos en [K1](k1-protected-boot.md). | Nada de K2; el resto pertenece a fases posteriores. |
| [EXP-02](../validation/experiments.md) | Derivar, mover, copiar, expirar, reciclar ranuras y rechazar operaciones bajo un ámbito cerrado, en alcance UP. | Reinicio con handles persistidos, y el reciclado bajo presión concurrente, que es K3. |
| [EXP-03](../validation/experiments.md) | Cierre concurrente con un RPC en vuelo: barrera separada de drenaje, contadores no falsamente cero, resultado posterior admitido solo desde quiescente. | Servidor muerto durante la retención, y el resultado posterior con estado durable, que es K4. |
| [EXP-04](../validation/experiments.md) | Conservación de cargos en la retirada, presupuesto agregado que alcanza a los ancestros, deuda arrastrada y observada entre ventanas, reserva de cierre retenida y gastada en la cuenta del servidor. | CPU en SMP y mantenimiento, que es K3. |
| [EXP-06](../validation/experiments.md) | IPC malformado sin efecto parcial, agotamiento de tabla de capacidades, de cola y de log, con cierre disponible. | Agotamiento bajo concurrencia y con varios servidores, que es K3. |

Los invariantes que esta ejecución toca —autoridad no amplificada, origen no falsificable, admisión indivisible, barrera antes que drenaje, obligación que sobrevive al cierre, cargos conservados— quedan **observados en una vertical**, no probados en general. Una vertical ejerce un camino por mecanismo; los mecanismos tienen más caminos.

Esta ejecución **no** demuestra: SMP, drivers propios, DMA, estado durable, Thalyx sobre este kernel, hardware físico ni rendimiento. Nada de eso está implementado. Sigue siendo una sola ejecución, en un emulador, con un núcleo.

El plano de diagnóstico de K1 sigue presente y sigue sin ser el plano de recibos. K2 añade el log de control, que sí reserva, sí es alcanzable por capacidad y sí rechaza operaciones que no puede registrar; las notas `user.note` que los programas emiten no son eso y la puerta las trata como lo que son. Las dos entradas de andamiaje de K1 permanecen, marcadas, sin tocar ningún objeto, para que la regresión siga ejecutándose.

El primer supervisor no tiene supervisor. Su fallo termina la ejecución, y el registro lo dice.

## Qué corrigió ejecutar los mecanismos

Compilar los 60 manejadores no encontró ninguno de estos siete defectos; ejecutarlos los encontró todos:

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

Los tres primeros son fallos de autoridad, los cinco siguientes de contabilidad, liveness e interfaz, y el último un agujero en la propia interfaz. Ninguno era visible sin ejecutar.

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
