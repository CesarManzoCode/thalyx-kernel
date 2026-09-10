---
id: EVD-008
kind: evidence
status: observed
---
# K5 — Port de Thalyx: qué se ejecutó y qué demuestra

Esta nota se escribe mientras K5 se ejecuta, etapa por etapa, y dice en cada momento qué está demostrado y qué no. K5 es el port: la semántica de Thalyx sobre los mecanismos de este kernel en vez de sobre los de Linux. Lo más fácil de fingir en un port es que haya habido un port, así que la disciplina de esta nota es una sola:

**Construir no es ejecutar.** Un compilador cruzado que produce una imagen no demuestra nada. Lo que demuestra algo es que el kernel construyó un dominio con esa imagen, lo activó, lo planificó, le cobró recursos y registró lo que hizo. Todas las afirmaciones de abajo se deciden desde los registros del kernel, y donde una se decide desde una nota del propio programa, el criterio lo dice en su título.

## Estado

| Etapa | Qué establece | Estado |
|---|---|---|
| `smoke` | El target nativo `x86_64-thalyx`: una imagen C real entra, se sostiene sobre la pila que el supervisor mapeó, habla con el kernel, hace coma flotante en hardware y hace crecer un heap con objetos de memoria que crea contra su propio ámbito. | **Ejecutado** |
| `surface` | Primera superficie nativa de Thalyx: versión identificada, contexto, workspace privado, cambio real, publicación condicionada y evidencia durable, sobre el estado administrado de K4. La validación es una afirmación que el propio trabajo hace sobre sí mismo, y eso es lo que la etiqueta de integración parcial significa. | **Ejecutado** |
| `work` | Programa acotado real —QuickJS de verdad— y herramienta nativa real, ejecutada en su propio dominio sobre un candidato sellado, decidiendo la publicación. | **Ejecutado** |
| `engine` | Motor CPU residente real y la toolchain que la carga de referencia necesita. | Pendiente |

## El target nativo

`x86_64-thalyx` no es un triple: es un conjunto de promesas, y está definido en un solo sitio, [`tools/build_native.py`](../../tools/build_native.py).

- ELF64 `ET_EXEC`, estático, sin intérprete, sin reubicaciones, entrada `_start`, tres segmentos `PT_LOAD` alineados a página y separados R-X / R-- / RW-.
- Base 0x400000, modelo de código pequeño, sin PIC. El cargador acotado del kernel no reubica.
- SSE y SSE2 en hardware, `-mfpmath=sse`, y **sin AVX**. Este kernel guarda y restaura el área `FXSAVE` heredada de forma ansiosa y no habilita `XSAVE`: un programa que usara registros anchos los perdería o los filtraría en un cambio de contexto. Es la única bandera cuya ausencia sería un fallo de corrección y no una decisión de rendimiento.
- Sin protector de pila, sin tablas de desenrollado, sin `.init_array`: nada por debajo desenrolla y nada ejecuta constructores.
- Una libc que es `user/native` y ninguna otra. `-nostdinc` es lo que hace eso cierto en vez de intencionado: las cabeceras del anfitrión no están en la ruta de búsqueda, así que un programa que busque una función POSIX falla al compilar aquí en vez de fallar al enlazar dentro del invitado.

La zona roja queda habilitada a propósito: la CPU cambia a la pila por procesador del TSS en cada interrupción y a la pila de `syscall` del kernel en cada entrada, así que nada escribe por debajo del puntero de pila de un programa de usuario.

## El runtime nativo

`user/native` es lo que un programa C pisa. No hay POSIX debajo y nada es una capa de compatibilidad:

- **Asignación.** `malloc` son objetos de memoria que el programa crea contra **su propio ámbito** y mapea en **su propio dominio**, ambos por capacidades que el supervisor instaló. El asignador es de etiquetas de frontera con listas por clase de tamaño y fusión en las dos direcciones. Un programa construido sin esas dos capacidades no puede crecer, que es la forma prevista de construir uno que no debe.
- **Hilos.** El kernel rechaza `DOMAIN_ADD_THREAD` sobre un dominio que ya corre, así que un programa nativo no crea hilos: su supervisor los crea antes de activarlo, todos entrando en el mismo trampolín con un índice, y el runtime los bloquea en una señal hasta que hay trabajo. Bloquear es un `SIGNAL_WAIT` real, no una espera activa. No hay segmento de almacenamiento local por hilo —poner `FS` necesita una instrucción que este kernel no habilita—, así que un hilo se identifica por su pila: las ranuras están alineadas a su propio tamaño y una máscara devuelve el índice.
- **Tiempo.** `CLOCK_QUERY` es la única fuente. No hay reloj de pared sincronizado y no se finge uno.
- **Transporte.** Mensajes acotados por `MAX_INLINE_PAYLOAD`, con el origen que el kernel estampa y capacidades prestadas como concesiones derivadas. Nada mayor viaja copiado: viaja en un objeto de memoria cuya capacidad lleva el mensaje, de modo que una transferencia grande es visible como autoridad.
- **Matemáticas.** `user/native/src/math.c` está escrito aquí y **no** redondea correctamente. Lo dice en su cabecera y se repite donde importa: el motor de inferencia pasa por `expf`, así que sus logits difieren de los de otra implementación en los últimos bits pase lo que pase —la `expf` vectorizada de llama.cpp difiere de la de glibc por la misma razón—. Por eso la evidencia del motor compara el token que elige un muestreador voraz y el margen con que ganó, y no un patrón de bits.
- **Salida formateada.** `printf` va al plano de diagnóstico, ocho bytes por nota. Es un canal de depuración, se puede fusionar, y nada durable se deriva de él.

## Etapa `smoke`: qué se ejecutó

Una quinta imagen UEFI construida desde las mismas fuentes con el mismo script y **el mismo binario de kernel** que las imágenes K1 a K4. Lo único que cambia es el paquete de arranque, que aquí lleva dos toolchains: el supervisor en Rust sobre el target del kernel y el programa C sobre el target nativo.

| Artefacto | Comando | Resultado |
|---|---|---|
| Runtime nativo | `python3 tools/build_native.py` | `build/native/libthalyx-native.a` y las imágenes de la etapa |
| Imagen | `python3 tools/build_image.py --phase k5 --stage smoke` | `build/thalyx-k5.img`, manifiesto con digest de cada artefacto y herramienta |
| Ejecución | `python3 tools/run_k5_stages.py` | `exit_status=33` (`k1.terminal status=complete`) |
| Veredicto | `python3 tools/check_k5.py` | `K5 GATE PASSED: 27 of 27 criteria met` |
| Autocomprobación | `python3 tools/check_k5.py --self-test` | `38 damages, each noticed by the criterion named` |
| Protocolos K5 | `python3 tools/check_k5_proto.py` | El Rust y el C generados coinciden con `abi/schema/k5-proto-v1.json` |

Plataforma: QEMU `q35` con acelerador `tcg`, cuatro procesadores, CPU `qemu64,+smep,+smap,+pdpe1gb,+x2apic`, 1024 MiB y OVMF.

Qué decidió cada criterio, y desde dónde:

- **El kernel construyó y ejecutó un dominio desde una imagen C.** `domain.created` nombra el objeto de imagen del que salió y el punto de entrada que la imagen declaró; `domain.activated` la puso a correr; y las notas del programa llevan el recuento de preempciones que el planificador le cobró. Seis preempciones: el programa fue interrumpido y continuó.
- **El runtime se levantó.** `.bss` a cero medido por el propio programa sobre un arreglo estático, la consulta de límites contestada, y el número de procesadores que el kernel le dijo al programa comparado contra los `smp.ap_online` que el kernel registró por su cuenta.
- **Coma flotante en hardware.** El programa ejecuta un bucle de sesenta y cuatro pasos con dependencia de acarreo, sembrado por el plan que el anfitrión construyó en la imagen, y reporta el resultado escalado. El anfitrión recalcula el mismo bucle en doble precisión y exige coincidencia: `1063483505` en los dos. Un programa que no hubiera hecho la aritmética no habría podido decir ese número, y el bucle atraviesa tres preempciones, que es lo que hace la comprobación también una comprobación del guardado y restaurado de estado FP.
- **El heap son objetos de memoria.** 192 páginas en el primer crecimiento, en tres arenas de 64; los bytes escritos a través de los mapeos nuevos se leen de vuelta y su suma sobre 256 desplazamientos muestreados es exactamente la esperada.
- **El techo lo pone el kernel.** El registro de arranque permite mucho más heap del que el ámbito puede cobrar, así que lo que detiene el crecimiento es `SCOPE_CREATE_MEMORY` rechazando: el objeto nunca se crea, el mapeo nunca ocurre y `malloc` no tiene qué devolver. La nota lleva el estado del kernel, `LIMIT_EXHAUSTED` (−9), y no un límite propio del programa. 512 páginas retenidas al llegar al techo.

Las regresiones K1, K2, K3 y K4 son cuatro criterios más de esta puerta, y las cuatro pasan sobre el mismo kernel: 13, 21, 28 y 31.

## Etapa `surface`: la semántica de Thalyx sobre el estado administrado de K4

La vertical que [la ruta](../roadmap/phases.md) pide —obtener el contexto de una versión, ejecutar cambios privados, publicar o abandonar, consultar evidencia— corre aquí sobre los mecanismos que ya existían: el servicio de estado de K4 sin cambios, sobre el driver de bloque de K4 sin cambios, sobre el mismo medio. **K5 no hace crecer un segundo mecanismo de persistencia al lado del primero**; esa era la mitad interesante del problema y evitarla habría sido evitar el port.

Lo que la etapa ejecuta, en orden, y de dónde se decide cada cosa:

1. **Versión identificada.** El trabajo consulta el servicio y recibe la generación publicada y su raíz. Cuando la generación es cero publica primero la versión semilla —el estado sobre el que después se trabaja— con una validación que el propio trabajo afirma, y la evidencia lo dice así.
2. **Contexto.** `contexto` responde qué es un nombre desde **la versión**, no desde un árbol mutable: cuántos usos tiene, en qué nombre está definido y con qué firma. La respuesta declara su propia cobertura, `textual`, y `resolved: false`. Thalyx responde esto desde un índice que construye un frontend de compilador; aquí no hay tal frontend y no se finge uno.
3. **Trabajo privado.** `FORK` sobre la versión publicada abre un workspace que nada publicado ve.
4. **Cambio real.** El trabajo sustituye la marca de dieciséis ceros del módulo por la semilla del run en hexadecimal. `sustituir` reemplaza **una** ocurrencia a propósito.
5. **Publicación condicionada.** `FREEZE` produce un candidato, y `PUBLISH` transiciona la raíz con CAS sobre la generación esperada.
6. **Evidencia.** El trabajo vuelve a leer la versión publicada por el mismo camino que usaría cualquiera y comprueba que la marca está donde debe.

Todo eso pasa por **una sola superficie de verbos** —`user/k5work/src/verbs.rs`— y por ningún otro camino. En esta etapa la conduce un guion escrito en el propio programa; en la siguiente la conduce un programa que ejecuta el runtime de lenguaje. Si fueran dos implementaciones distintas, «un programa alcanza exactamente lo que sus llamadas podrían haber alcanzado una a una» sería una esperanza.

Lo que la etapa **no** establece, y la puerta lo nombra en el título de sus criterios: nadie ha ejecutado una herramienta sobre el candidato. El registro de validación que se publica es una afirmación del trabajo sobre sí mismo, exactamente como la de K4. Una afirmación no es una comprobación.

## Etapa `work`: un programa real y una herramienta real

### El runtime de lenguaje es QuickJS de verdad

Es el mismo motor, en la misma versión, que ejecuta la revisión de Thalyx contra la que se hace este port: `thalyx-program` depende de `rquickjs 0.12`, que empaqueta quickjs-ng 0.15.1. `tools/fetch_quickjs.py` lo trae **fijado y comprobado por digest**, y `tools/build_native.py` lo compila para el target nativo con las mismas banderas que todo lo demás. Nada del lenguaje está reimplementado.

Se **trae en vez de vendorizarse**, a propósito: [OQ-14](../roadmap/open-questions.md) dice que la licencia de distribución y la política de contribuciones se fijan *antes* de incorporar código de terceros a este repositorio, y esa pregunta sigue abierta. Traerlo mantiene el código fuera del árbol y deja la construcción exactamente reproducible: una versión, un digest, comprobado antes de compilar nada.

El dominio del runtime tiene: una faceta de un endpoint, una página de configuración, la región que comparte con el trabajo que lo conduce, y su propio ámbito para crecer un heap. Eso es toda su tabla de capacidades, y es lo que hace que «el programa no puede leer un archivo» sea un hecho sobre el kernel y no una afirmación sobre el código: no hay archivo que leer ni capacidad que llegue a uno.

Dos techos, y solo uno es del motor: `JS_SetMemoryLimit` es la contabilidad del propio QuickJS y está **por debajo** de lo que el ámbito puede cobrar. El del ámbito lo impone el kernel. Un run que llega a cualquiera de los dos dice a cuál, en las unidades en que se cuenta.

Parar está impuesto dos veces. Una aserción que solo lanzara podría ser capturada por el programa que la falló; aquí **engancha**: se registra del lado del trabajo, lanza, y desde ese momento toda llamada se rechaza. Un programa no puede capturarse el paso más allá de lo que lo detuvo, porque lo que lo detiene no está en el lenguaje. Esto se observó funcionando antes de estar previsto: la primera versión del módulo tenía la marca de ceros escrita dos veces, `sustituir` reemplazó una, y el programa falló su propia aserción «la marca vieja ya no está», enganchó, y nada se publicó.

### La herramienta de validación es real y corre dentro del kernel

`user/ncheck` es un programa del target nativo lanzado en **un dominio propio, con un ámbito propio**, al que se le da exactamente una cosa: un objeto de memoria **sellado** con el candidato. Sin servicio de estado, sin motor, sin dispositivo, sin autoridad para construir nada.

Lo que hace es real y puede fallar de verdad: compila cada nombre JavaScript del candidato con el parser y el compilador de bytecode de QuickJS, y después ejecuta las aserciones del propio candidato en un contexto cuyo único enlace es `check`. Su código de salida es cero solo si todo compiló y toda comprobación se sostuvo.

El candidato se **sella antes de prestarse**: el kernel retira todo mapeo con escritura, así que lo que la herramienta lee es aquello sobre lo que trata el veredicto. La puerta comprueba el orden —el sello está registrado antes de que el dominio de la herramienta exista— y el lanzador rechaza un candidato que no esté sellado con un estado propio.

Lo que la herramienta **no** hace, y lo dice en su respuesta: no es una comprobación de tipos. `Check::Rust` de Thalyx compila un grafo de crates con `cargo`; nada en este sistema hace eso y nada aquí lo finge. La cobertura que declara es `parses_and_asserts` y `type_checked: false`.

### Lanzar es un servicio, no un poder ambiental

Un dominio de trabajo no tiene ninguna capacidad que construya un dominio. Pide. El lanzador —el supervisor— decide qué puede ser una herramienta: ámbito propio con techos propios, candidato sellado, una página donde escribir su informe, y nada más; cuando termina, su ámbito se retira y **lo que costó se lee de la contabilidad del kernel**, no de lo que la herramienta dice de sí misma. En la ejecución registrada la herramienta gastó 65 ms de CPU cobrados a su propio ámbito.

### Lo que la etapa `work` corrigió

Cinco defectos que compilar no encuentra:

1. **Un objeto que nadie podía sellar.** El candidato se creaba con derechos máximos de lectura, escritura y mapeo, sin `MEMORY_SEAL`, así que sellarlo se rechazaba. Sellar tiene que estar en los derechos máximos del objeto, no solo en la petición.
2. **Un ámbito sin autoridad para fabricar el candidato.** El trabajo recibía su propio ámbito con `INSPECT` y nada más, con el argumento de que leer un presupuesto no es gastarlo —cierto, pero ensamblar un candidato sí necesita crear un objeto—. `SCOPE_CREATE` se concede a propósito y queda acotado por el techo de páginas del propio ámbito.
3. **`JS_ParseJSON` exige terminador nulo.** Su documentación lo dice y su escáner se apoya en él. La primera versión parseaba en la región compartida, donde a continuación estaban los bytes de la respuesta anterior, y la tercera llamada del primer programa real volvió con «unexpected data at the end» y ciento cuarenta y dos bytes de JSON perfectamente bueno delante. Ahora se copia a memoria privada y se termina en nulo —que además es lo correcto por otra razón: la región la puede escribir el otro dominio mientras se lee—.
4. **`JS_Eval` exige lo mismo.** La herramienta evaluaba en el objeto sellado, donde los nombres están pegados unos a otros, y reportó tres errores de sintaxis en tres archivos perfectamente válidos.
5. **Un cuerpo de función compilado como script.** `program.js` termina en `return`, que es un error de sintaxis en el nivel superior de un script y es exactamente correcto dentro del envoltorio en que el runtime lo ejecuta. La entrada del candidato lleva ahora un bit que lo dice, y la herramienta lo compila como lo que es en vez de adivinarlo por el nombre.

Y dos que no son de este código: el supervisor de la primera etapa `surface` no drenaba el plano de control, el log se llenó a los cincuenta y seis recibos, y el kernel empezó a rechazar admisiones —que es su comportamiento diseñado, no un defecto—; y el servicio de estado de K4, con su propia lectura del medio rechazada por esa misma causa, contestó `NOT_FOUND` a la lectura de un objeto perfectamente publicado. Lo segundo **sí** era un defecto y está corregido: un almacén que no puede determinar alcanzabilidad tiene que decir eso, no la afirmación más fuerte. Ahora distingue «he mirado y esto no está» de «no he podido mirar».

## Lo que la etapa `smoke` corrigió

Dos defectos que compilar no encuentra:

1. **Una imagen sin script de enlace.** El primer supervisor K5 se construyó sin el script que separa los segmentos, y el kernel lo rechazó con `segment_unaligned` antes de ejecutar una instrucción. El rechazo es el comportamiento correcto; el defecto era del paquete.
2. **Un objeto de memoria que nadie podía mapear.** El heap creaba sus arenas con derechos máximos de lectura y escritura y sin `MEMORY_MAP`, y el kernel rechazaba el mapeo por derechos insuficientes. `MEMORY_MAP` es el derecho a hacer un mapeo, no un permiso de página, y tiene que estar en los derechos máximos del objeto además de en la petición. Antes de la corrección el programa no podía asignar un solo byte y la primera señal era `malloc` devolviendo nulo.

## Qué decide la puerta, y desde dónde

`tools/check_k5.py` decide **27** criterios por separado; cuatro son las regresiones K1–K4 sobre el mismo binario de kernel. La disciplina es la misma en todos:

- **Construir no es ejecutar.** Todo criterio sobre el lado nativo se decide desde los registros del kernel de un dominio que el kernel construyó, activó, planificó y cobró.
- **Un invitado que imprime `PASS` no demuestra nada.** Donde un criterio lee una nota del propio programa, lo dice en su título, y el valor que lee es un número que el programa solo pudo producir haciendo el trabajo.

El criterio decisivo no lo narra el invitado en absoluto: **el medio lleva una versión publicada cuyo módulo lleva la semilla de este run**, decodificada aquí por el módulo que genera el esquema de K4. El anfitrión eligió la semilla y la escribió en la imagen; un invitado que no hubiera hecho el trabajo no habría podido poner esos bytes ahí. Dos más se apoyan en la misma clase de evidencia: la herramienta leyó **exactamente** los bytes que el medio dice que la versión publicada enlaza (2913 de 2913), y el registro de validación durable nombra la identidad de herramienta que realmente corrió.

`--self-test` daña la evidencia de **38** formas distintas —notas cambiadas y quitadas, sucesos del kernel quitados y añadidos, bytes del medio reescritos— y un criterio nombrado tiene que notar cada una.

## Lo que estas etapas **no** demuestran

- **No hay motor de inferencia todavía.** `thalyx.model` existe en la superficie y responde `no_engine` en estos roles, que es un hecho sobre su tabla de capacidades. La etapa `engine` es la que tiene que cambiarlo.
- **No hay comprobación de tipos.** La herramienta compila y ejecuta aserciones; `Check::Rust` de Thalyx compila un grafo de crates y nada aquí lo hace.
- **No hay `cargo`, ni compilador de Rust, dentro del kernel.** La toolchain que produce estas imágenes es del anfitrión y el manifiesto lo dice; lo que se ejecuta nativamente es la imagen.
- **Un solo trabajo.** Todavía no hay dos trabajos rivales, ni cancelación durante un servicio residente, ni un corte en publicación dentro de esta vertical.
- Sigue sin haber aislamiento de DMA, hardware físico ni durabilidad frente a corte de energía: los límites de K3 y K4 se heredan enteros.

## Digests de la ejecución registrada

```text
thalyx-k5.img         0403e1197fc3f8cb555376e723f7a3326a6c04b40dfb8f368aba773225ac0a26
kernel.elf            8597eb87f8dfafad9c0112ee1479d60d398ca2a859ebc882da45166abb44b8b7
nsmoke.elf            ef6f46d5fd1c01f0832a455deb7cd859a218c34d67ce19c64e29eccad2874dd4
libthalyx-native.a    e0621cc37ead417d50e90f6f4b387f1da429d501b0286adc07422767ad91edfe
```
