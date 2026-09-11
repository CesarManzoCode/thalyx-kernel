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
| `engine` | Motor CPU residente real —llama.cpp en la etiqueta que Thalyx fija, con el `serve_one` de su propio motor— y la toolchain que esa carga necesita: C++ alojado, hilos, TLS, desenrollado, archivos administrados. Residencia y petición cobradas a ámbitos distintos por el kernel. | **Ejecutado** |

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
- **Hilos.** El kernel rechaza `DOMAIN_ADD_THREAD` sobre un dominio que ya corre, así que un programa nativo no crea hilos: su supervisor los crea antes de activarlo, todos entrando en el mismo trampolín con un índice, y el runtime los bloquea en una señal hasta que hay trabajo. Bloquear es un `SIGNAL_WAIT` real, no una espera activa. Un hilo se identifica por su pila —las ranuras están alineadas a su propio tamaño y una máscara devuelve el índice— y, desde la etapa `engine`, tiene además su propio puntero de hilo: `THREAD_POINTER_SET` es la sexta entrada del kernel, añadida porque la biblioteca estándar de C++ preconstruida guarda su estado de excepciones y su guarda de pila detrás de `FS`, y un hilo sin base `FS` no puede ejecutarla. El kernel lo escribe en cada despacho y nunca direcciona memoria a través de él.
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

## Etapa `engine`: el motor CPU residente real

### Qué motor, y de dónde sale

La carga de referencia es la que ejecuta la revisión de Thalyx contra la que se hace este port, y ninguna otra: **llama.cpp en la etiqueta `b10665`**, que es la que `dev/build-engine.sh` de Thalyx fija, y **`engine/thalyx-engine.cpp`** de esa misma revisión, cuyo `serve_one` es el que `user/nengine/engine.cpp` porta —las mismas llamadas a `common` en el mismo orden, con los mismos parámetros: memoria del contexto limpiada y muestreador construido por petición, temperatura cero, prompt decodificado por lotes, parada en fin de generación o en el presupuesto—. El modelo es el que escribe `dev/tiny-model.py` de Thalyx con el `gguf-py` de llama.cpp, de modo que la única opinión sobre GGUF en el circuito es la de llama.cpp.

Las tres cosas se **traen fijadas y comprobadas por digest**, no vendorizadas, por la razón de QuickJS: [OQ-14](../roadmap/open-questions.md) sigue abierta. `tools/fetch_engine.py` comprueba el archivo de llama.cpp dos veces —el digest del archivo y el digest del árbol de los 403 archivos que la construcción usa, porque GitHub regenera archivos de etiqueta y ha cambiado su compresión sin cambiar un byte de dentro— y el que decide es el del árbol. El único cambio del port a llama.cpp es un parche de dos sitios en `common/common.cpp`, donde dos funciones nombran cada sistema operativo que conocen y se niegan a compilar en cualquier otro; ninguna de las dos está en el camino del motor, y el manifiesto registra el digest de lo que reemplazan y de lo que ponen. El modelo tiene su digest fijado en `tools/build_reference.py`; un `numpy` distinto que produjera otros bytes detiene la construcción en vez de cambiar en silencio aquello sobre lo que se comparan los dos motores.

### La toolchain: C++ alojado sobre una libc que es de aquí

Un programa C de las etapas anteriores se compilaba *freestanding*. Un motor de inferencia real es C++ y arrastra la biblioteca estándar de C++, y esa biblioteca no es algo que convenga reescribir. La decisión está en [`tools/build_native.py`](../../tools/build_native.py): los programas C++ y las fuentes C de las bibliotecas que enlazan se compilan **alojados** contra las cabeceras de la `libstdc++` del GCC del anfitrión, y se enlazan contra sus archivos preconstruidos `libstdc++.a`, `libgcc_eh.a` y `libgcc.a`, registrados por ruta y digest en el manifiesto. Esos archivos se construyeron para glibc, así que referencian la interfaz de glibc; `user/native` implementa la parte que alcanzan, **y el enlace es lo que demuestra la clausura**: cada referencia resuelve o la imagen no se produce. `closure_report` lo comprueba sobre la imagen enlazada y no sobre las banderas: sin intérprete, sin sección dinámica, sin reubicaciones, sin símbolos fuertes sin resolver, y sin una sola instrucción VEX, EVEX, SSSE3 ni SSE4 en el desensamblado —porque el kernel no guarda estado `XSAVE` y el modelo `qemu64` de la evidencia no tiene esas extensiones—. Una imagen que rompa cualquiera de esas promesas se rechaza en el anfitrión.

Lo que el runtime nativo creció para sostener esa clausura, cada cosa una respuesta del kernel o una negativa explícita:

- **Almacenamiento local por hilo**, con la disposición de la ABI TLS de ELF x86-64 para un ejecutable estático (variante II): el puntero de hilo apunta a un bloque de control, el segmento TLS de la imagen está justo debajo, y la guarda de pila está en `FS:0x28`, que es lo que compara cada función de la biblioteca preconstruida que fue compilada con protector. Que esa disposición sea la que el enlazador supuso no se da por hecho: cada hilo lee una variable local con valor inicial conocido y su propia guarda de vuelta a través de `FS` antes de ejecutar nada más, y anota el resultado.
- **Constructores** antes de `main` y manejadores en `exit`; `_dl_find_object` para que el desenrollador de GCC encuentre las tablas de excepciones de la única imagen que hay; `errno` por hilo; un sitio donde reportar una pila aplastada.
- **`pthread`** sobre hilos construidos: `pthread_create` entrega su trabajo a un hilo que el supervisor construyó antes de activar, y responde `EAGAIN` cuando no queda ninguno en vez de devolver un hilo que nunca correrá. Mutexes y condiciones bloquean en un `SIGNAL_WAIT` real, un bit por hilo; los tipos tienen los tamaños de glibc x86-64 porque la biblioteca preconstruida los guarda dentro de los suyos. `syscall` responde exactamente un número: las operaciones de futex que esa biblioteca emite directamente cuando dos hilos compiten por inicializar el mismo objeto estático.
- **Archivos**, donde los únicos archivos son regiones que un supervisor mapeó de solo lectura: el objeto sellado en `TH_BULK_VADDR` es `/bulk` y no hay otro nombre. No hay `mmap`: un programa que habría mapeado un archivo lo lee, y lo lee a su propio heap, cobrado a su propio ámbito. Escribir se rechaza con `EROFS` y un nombre no vinculado con `ENOENT`.
- `sysconf` lee los procesadores que el kernel levantó y el tamaño de página; dormir es una espera real; `getpid` es el identificador del dominio. Señales, shell, entorno y entropía **no existen** y pedirlos se rechaza; `arc4random`, que no tiene forma de decir que falló, detiene el programa con una nota antes que devolver algo predecible que parezca correcto.
- Locale, `wchar`, `iconv`, `scanf`, formato de tiempo y `dirent`: lo mínimo que la biblioteca preconstruida referencia, y en cada uno la respuesta es la de un sistema con una sola locale, sin zona horaria y sin directorios.

Las cabeceras C generadas desde los dos esquemas ganaron lo que hace falta para que un programa C++ las lea: `_Static_assert` y `_Alignof` escritos como C++ los escribe, y un campo cuyo nombre es palabra reservada en C++ —`FaultReport.class`— con otra grafía y las mismas aserciones de desplazamiento en los dos lenguajes.

### Lo que el kernel añadió

- **`THREAD_POINTER_SET`**, sexta entrada de la interfaz (V0 pasa a 0.3). Es estado de registro del propio hilo, como el puntero de pila: no nombra ningún objeto, no necesita capacidad, y lo que el kernel le debe es que siga siendo del hilo. Se guarda en el hilo y se escribe en `IA32_FS_BASE` en **cada** despacho, incondicionalmente, así que un hilo nunca corre con el puntero de otro; nada en anillo 0 direcciona memoria por `FS`. Se rechaza con `INVALID_ADDRESS` fuera del rango de usuario, antes de escribir nada.
- Los techos de una imagen suben para el motor: 16 MiB de archivo y 4096 páginas mapeadas, y `MAX_MEMORY_PAGES_PER_OBJECT` pasa de 512 a 4096 en el esquema. El lector de ELF no cambia en clase —cada desplazamiento sigue comprobado contra la longitud— solo en tamaño.
- Un estado de hilo nuevo, `Held`: reservado a un dominio que todavía se construye, con pila de kernel y contexto inicial, no planificable hasta la activación. Existe por un defecto que se cuenta abajo.
- Una terminación por autoridad **alcanza al instante** a un hilo del dominio que esté corriendo en otro procesador, y un hilo así **se va en su siguiente entrada al kernel** en vez de fallar sobre los mapeos que la terminación retiró. También por un defecto que se cuenta abajo.

### Qué corre, y quién paga

El supervisor construye el motor como construye un servicio: un dominio en un ámbito propio —`engine`, 16384 páginas, tres de paralelismo, 2 ms de reserva de cierre—, con los pesos como **un objeto sellado mapeado de solo lectura** en él y en ningún otro sitio, un endpoint para recibir, una señal para decir que está listo, y nada más: sin servicio de estado, sin dispositivo, sin lanzador, sin log. El supervisor comprueba que el objeto está sellado y que la longitud que el plan declara cae en su última página antes de construir nada, y espera a la señal de listo con plazo; un motor que muere antes de levantarla se reporta con su código de salida.

El motor carga los pesos **una vez** —`llama_model_load_from_file` sobre `/bulk`, con `use_mmap` en su valor por defecto y llama.cpp descubriendo por su cuenta que esta plataforma no mapea archivos— y después sirve. Una petición es una invocación en su endpoint: quién pregunta está en la cabecera que el kernel escribió. El prompt viaja en un buffer que el trabajo **presta** con la invocación, y el motor lo alcanza por esa capacidad y por nada más; reporta un digest de los bytes exactos que leyó, y el trabajo compara con lo que prestó. Antes de calcular, el hilo del motor **se vincula a la invocación** (`INVOCATION_BIND_WORKER`), y desde ahí el kernel cobra la inferencia al ámbito del trabajo que preguntó; los pesos y todo lo cargado una vez siguen cobrados al ámbito del motor. Entre token y token el motor pregunta al kernel si el ámbito de quien preguntó sigue abierto (`INVOCATION_QUERY`); si no, para, responde `CANCELLED` y se queda residente para el siguiente.

Lo que llama.cpp y ggml imprimen no va a ningún sitio —el plano de diagnóstico son ocho bytes por nota— salvo los errores, y el número de líneas descartadas se anota. En la ejecución registrada, 228.

Dos hilos: el que sirve y uno más, porque `common` de llama.cpp arranca un hilo de log la primera vez que algo escribe por él, y un programa que no pudiera arrancarlo fallaría ahí. Un solo hilo de cómputo: cada instrucción de una inferencia corre en el hilo vinculado a la invocación y se cobra a quien preguntó; un segundo hilo de ggml sería un hilo que nadie vinculó.

### Qué decidió la puerta, y desde dónde

Siete criterios nuevos, sobre la ejecución `engine` (semilla `0x5EED0004`):

- **El runtime de C++ se levantó, y lo dice el kernel primero.** `thread.pointer` registra el puntero que cada hilo del motor pidió: dos hilos, dos bases distintas. Las notas del programa dicen que el TLS de cada uno se sostuvo —40 bytes, que es lo que la imagen enlazada declara en su `PT_TLS`— y que corrieron 13 constructores; ninguna nota de guarda aplastada, de fortificación ni de hilo rechazado.
- **Un motor, una carga, dos respuestas, y los pesos son los que el anfitrión fijó.** Un dominio `nengine` creado y ninguno terminado; una nota de carga (222 ms); `served` 1 y 2 vistos por el motor y por el trabajo; 470304 bytes de `/bulk` con FNV-1a `0x5b8939aa99a499ca`, que el anfitrión recalcula sobre su copia del modelo —cuyo SHA-256 el manifiesto de la imagen y `tools/build_reference.py` fijan igual— y exige idéntico; 462592 bytes de pesos residentes según `llama_model_size`.
- **Quién paga lo decide el kernel.** Dos `sched.bound` del hilo del motor, cada uno con `effective_scope` y `origin_scope` iguales al ámbito `work` (34) y `account=origin_budget`, y el ámbito del motor es otro (95). Las invocaciones que el kernel vinculó (318, 323) son las que el motor anotó. Después de la ejecución: el ámbito del motor cobrado 140 ms y 8020 páginas; el del trabajo 176 ms. Residencia y petición son **dos números del kernel sobre dos ámbitos**.
- **El motor leyó exactamente lo que el anfitrión eligió.** Los digests de los dos prompts según el motor, según el trabajo que los prestó y según el anfitrión coinciden.
- **La versión publicada lleva las completaciones que el motor de Thalyx da en Linux.** El decisivo, y el invitado no lo narra: el medio lleva `model.json` en la versión que el trabajo publicó, con los bytes de cada completación en hexadecimal; `tools/run_reference.py` arranca `thalyx-engine.cpp` **sin cambios** en el anfitrión, sobre el mismo modelo, con el mismo contexto (512), un hilo y muestreo voraz, y le pregunta lo mismo. Iguales byte a byte en los dos prompts: `6f0be993c822a0ed98762f7f` y `6f0be99caf568900547fafb7`. Además, el primer token que eligió el muestreador es el argmax de los logits crudos medido antes de muestrear (114 y 114), con márgenes de 199 y 144 ppm —una decisión y no un empate, que es la única forma en que dos aritméticas que difieren en los últimos bits se pueden comparar—, y los digests de tokens que el registro publica son los que el motor anotó.
- **Confinamiento.** La tabla del motor: su ámbito, su dominio, un endpoint, tres señales; el objeto del modelo mapeado con derechos `0x100` y `writable_maps=0`.
- **La vertical entera con el motor dentro.** Generaciones 1 y 2 publicadas, herramienta con salida 0, evidencia releída con la marca, programa terminado por `return`, y `model.json` entre los nombres de la versión.

`--self-test` crece a **52** daños; los catorce nuevos reescriben notas del motor, quitan y añaden sucesos del kernel (`thread.pointer`, `sched.bound`, `domain.terminated`), cambian la vinculación al ámbito del motor, reescriben las respuestas de referencia y los bytes del medio.

### La comparación con Linux es ejecución en el anfitrión, y se dice

`tools/build_reference.py` y `tools/run_reference.py` producen la otra mitad de la comparación: el motor de Thalyx, sin cambios, construido contra el mismo árbol de llama.cpp con la configuración que `dev/build-engine.sh` pide, sobre la máquina de desarrollo, bajo Linux. **Nada de eso es evidencia nativa**, y cada archivo que producen lo lleva escrito en un campo `note` que la puerta exige encontrar. Lo que aportan es la respuesta esperada; lo que demuestra que un motor corrió nativamente son los registros del kernel de un dominio que construyó, activó, planificó, cobró y vinculó.

### Lo que la etapa `engine` corrigió

Seis defectos que compilar no encuentra, y dos son del kernel:

1. **Un hilo reservado que parecía libre.** El motor es el primer dominio nativo construido con un segundo hilo. `DOMAIN_ADD_THREAD` buscaba una ranura libre en la tabla de hilos, y el hilo inicial de un dominio en construcción estaba en estado `Empty`: la búsqueda lo reutilizó, sobrescribió su contexto, y `_start` no corrió nunca. El estado `Held` existe para que una ranura reservada no parezca libre.
2. **Ningún hilo tenía base `FS`.** La biblioteca de C++ preconstruida falla en su primer `mov %fs:0x28`. No es lento ni parcial: no corre. `THREAD_POINTER_SET` es la respuesta, con la garantía de que el kernel la escribe en cada despacho.
3. **Un techo de ámbito escrito para lo que se usa y no para lo que se pide.** En Linux una asignación que nadie toca es espacio de direcciones; aquí cada página del heap es un objeto de memoria cobrado. El primer techo del ámbito del motor, dieciséis megabytes, terminó en `GGML_ASSERT(ctx->mem_buffer != NULL)`. El techo cubre ahora lo que llama.cpp reserva con el lote de 512 que Thalyx configura, y lo que la residencia cuesta de verdad se lee del ámbito después.
4. **Una imagen de cinco megabytes contra un techo de cuatro.** El cargador acotado rechazaba el motor antes de ejecutar una instrucción, que es su comportamiento correcto; el techo era del kernel y subió junto con el del esquema.
5. **Un runtime parado mientras liberaba su heap.** El lanzador terminaba por autoridad al runtime de lenguaje en cuanto el trabajo lo pedía, y el runtime, en otro procesador, seguía liberando memoria; la terminación retiró sus mapeos, el hilo falló sobre la arena retirada, y la ejecución contó **un fallo de usuario contra un dominio ya terminado**. Dos correcciones, y la del kernel es la que cuenta: una terminación envía ahora una interrupción a cada procesador que esté ejecutando un hilo del dominio, y toda vuelta a anillo 3 —interrupción, trampa o `syscall`— comprueba si el hilo fue parado y, si lo fue, lo abandona con un registro propio, `thread.stopped_late`, en vez de devolverle el control; un fallo que llegue antes que la interrupción se clasifica igual y no se cuenta. El lanzador, además, da al runtime un plazo acotado para irse por su cuenta, porque parar a un programa en medio de liberar memoria es una forma de parar y no la única.
6. **Cabeceras C que un compilador de C++ no aceptaba.** `_Static_assert`, `_Alignof` y un campo llamado `class`. Los generadores lo escriben ahora en las dos grafías con las mismas aserciones.

## Lo que la etapa `smoke` corrigió

Dos defectos que compilar no encuentra:

1. **Una imagen sin script de enlace.** El primer supervisor K5 se construyó sin el script que separa los segmentos, y el kernel lo rechazó con `segment_unaligned` antes de ejecutar una instrucción. El rechazo es el comportamiento correcto; el defecto era del paquete.
2. **Un objeto de memoria que nadie podía mapear.** El heap creaba sus arenas con derechos máximos de lectura y escritura y sin `MEMORY_MAP`, y el kernel rechazaba el mapeo por derechos insuficientes. `MEMORY_MAP` es el derecho a hacer un mapeo, no un permiso de página, y tiene que estar en los derechos máximos del objeto además de en la petición. Antes de la corrección el programa no podía asignar un solo byte y la primera señal era `malloc` devolviendo nulo.

## Qué decide la puerta, y desde dónde

`tools/check_k5.py` decide **34** criterios por separado; cuatro son las regresiones K1–K4 sobre el mismo binario de kernel. La disciplina es la misma en todos:

- **Construir no es ejecutar.** Todo criterio sobre el lado nativo se decide desde los registros del kernel de un dominio que el kernel construyó, activó, planificó y cobró.
- **Un invitado que imprime `PASS` no demuestra nada.** Donde un criterio lee una nota del propio programa, lo dice en su título, y el valor que lee es un número que el programa solo pudo producir haciendo el trabajo.

El criterio decisivo no lo narra el invitado en absoluto: **el medio lleva una versión publicada cuyo módulo lleva la semilla de este run**, decodificada aquí por el módulo que genera el esquema de K4. El anfitrión eligió la semilla y la escribió en la imagen; un invitado que no hubiera hecho el trabajo no habría podido poner esos bytes ahí. Dos más se apoyan en la misma clase de evidencia: la herramienta leyó **exactamente** los bytes que el medio dice que la versión publicada enlaza (2913 de 2913), y el registro de validación durable nombra la identidad de herramienta que realmente corrió.

`--self-test` daña la evidencia de **52** formas distintas —notas cambiadas y quitadas, sucesos del kernel quitados y añadidos, bytes del medio reescritos, respuestas de referencia reescritas— y un criterio nombrado tiene que notar cada una.

## Lo que estas etapas **no** demuestran

- **El motor es real y pequeño.** llama.cpp entero corre nativamente, pero el modelo es el de `dev/tiny-model.py`: 470 KB, lo que Thalyx usa para ejercitar su propio motor. Que las completaciones coincidan byte a byte con Linux es una afirmación sobre el motor y la aritmética; no dice nada sobre rendimiento, y una inferencia de doce tokens tarda entre 54 y 114 ms bajo TCG.
- **Un solo hilo de cómputo.** Es una decisión de contabilidad —cada instrucción de la inferencia corre en el hilo vinculado a quien pregunta— y no una medida de lo que un motor con varios hilos costaría.
- **La biblioteca estándar de C++ es la del anfitrión.** Se enlaza preconstruida, registrada por digest, y lo que se demuestra es que la clausura de lo que referencia está en `user/native`; no que esa biblioteca haya sido auditada.
- **No hay comprobación de tipos.** La herramienta compila y ejecuta aserciones; `Check::Rust` de Thalyx compila un grafo de crates y nada aquí lo hace.
- **No hay `cargo`, ni compilador de Rust, dentro del kernel.** La toolchain que produce estas imágenes es del anfitrión y el manifiesto lo dice; lo que se ejecuta nativamente es la imagen.
- **Un solo trabajo.** Todavía no hay dos trabajos rivales, ni cancelación durante un servicio residente, ni un corte en publicación dentro de esta vertical: la comprobación de cancelación existe en el motor y no se ha ejercido.
- Sigue sin haber aislamiento de DMA, hardware físico ni durabilidad frente a corte de energía: los límites de K3 y K4 se heredan enteros.

## Digests de la ejecución registrada

```text
thalyx-k5.img (engine) 57b3d3a198cf03f5188812ffc7a042cec415d35c5074a77bef8d9974f693a6f5
kernel.elf             12f2482231093b706800f439b8530b9fb322c01b5d156d523441c75374fd3b3b
nengine.elf            2b3d4221452cc79ac7d8008394b7f0dd21530f3addb6096a525b04401bee8d3e
nhacer.elf             be0e36dd69a98a579dd308ceaa43f37ae420d0a8899d3278931c8ceeea62fb5c
ncheck.elf             b0e0fcc69e7c85e8b4341db09763092adcfd39a11671c4f42763a5059a4736ad
libthalyx-native.a     e92f4a98822e1ae88e7bb31ceb6ac4507d01c704a1dacc542c5528906e89dc38
tiny.gguf              1b726566329f3aaca2f6d89ee0ec4efc31986936628ca6d68591077c857d3267
llama.cpp b10665 tree  4e623890a44101bdc55674b90ab9eea9fce7ef5952ad2f904f58d32106e4f93c
```

El binario de referencia `thalyx-engine` y el modelo se construyen en el anfitrión con `python3 tools/build_reference.py`; su `reference.json` registra compilador, banderas y paquetes. Es tooling del anfitrión y no evidencia.
