---
id: STATE-001
kind: evidence
status: observed
---
# Estado actual

**2026-09-11 · Fundación 0.1.0 · K0, K1, K2, K3, K4, K5 y K6 completos en su alcance. Primera referencia: este árbol, sobre KVM.**

> **Punto de reanudación.** Este documento es el checkpoint. La sección [Reanudar aquí](#reanudar-aquí) dice exactamente dónde empieza el trabajo siguiente; no hace falta reauditar K0, K1, K2, K3, K4, K5 ni K6.

## Qué existe

Un vault de 47 notas con constitución, 13 contratos de arquitectura, glosario, reconstrucción de Thalyx, 32 fuentes primarias anotadas, diez decisiones, 23 invariantes, alternativas, integración Linux/nativa, experimentos y ruta de implementación. Se incluyen herramientas documentales y dos modelos finitos de investigación con controles negativos y resultado versionado.

Existe además un kernel que arranca, un sistema de capacidades que se ejecuta sobre él, una ejecución en cuatro procesadores con un dispositivo de bloque real conducido desde usuario, un almacén de versiones que sobrevive a la ejecución que lo escribió, y la semántica de Thalyx corriendo sobre todo eso: un target nativo propio, una libc escrita aquí, QuickJS real, una herramienta real y llama.cpp entero como motor residente, con el kernel cobrando cada inferencia a quien la pidió. El workspace tiene loader UEFI, protocolo de arranque, kernel con `arch/x86_64`, interfaz V0 generada desde un esquema, dieciséis programas de usuario en Rust y cinco en C y C++ sobre un target nativo propio, tres bibliotecas que comparten —el runtime, el formato durable y el paquete K5— y una libc escrita aquí, con toolchain fijada y construcción reproducible desde un solo script para las seis fases. Y existe la primera medida: diecisiete protocolos emparejados corridos sobre este kernel y sobre Linux en la misma máquina virtual, con sus resultados, sus límites y su puerta. [Qué se midió exactamente](../evidence/k6-comparison-hardening.md).

La imagen K1 arranca en QEMU, ejecuta dominios en ring 3, los preempta con timer, contiene sus fallos ilegales y sobrevive. [Qué se ejecutó exactamente](../evidence/k1-protected-boot.md). La imagen K2, con el mismo kernel y otro paquete, construye un supervisor con un manifiesto explícito de capacidades y deja que ese supervisor cree todo lo demás por la interfaz. [Qué se ejecutó exactamente](../evidence/k2-objects-authority-work.md). La imagen K3, otra vez con el mismo kernel, arranca cuatro procesadores, reparte trabajo entre ellos con presupuesto agregado, retira traducciones con acuse de todos, y le entrega a un dominio de usuario una función virtio-blk moderna con la que lee, escribe y hace flush. [Qué se ejecutó exactamente](../evidence/k3-smp-devices.md). La imagen K4, con el mismo kernel una cuarta vez, publica versiones inmutables sobre un medio de bloques que persiste entre arranques, y la matriz corta la ejecución en cada punto donde una publicación puede cortarse para arrancar de nuevo sobre lo que el corte dejó. [Qué se ejecutó exactamente](../evidence/k4-durable-state.md). La imagen K5, con el mismo kernel una quinta vez, ejecuta el port: un programa en QuickJS real que sale de la versión publicada, una herramienta nativa real que decide la publicación, un motor de inferencia real que da byte a byte lo que el motor de Thalyx da en Linux, y todo ello bajo rivales, cierre y cortes. [Qué se ejecutó exactamente](../evidence/k5-thalyx-port.md).

La base de evidencia de Thalyx está fijada al commit `0492f72e487e2463b0d7b938365a8b3383364cb9`: inventario de 407 commits alcanzables, 96 archivos del vault, 38 rutas de código/configuración de evidencia y 15 revisiones históricas seleccionadas. Inventariar no significa haber ejecutado ni auditado exhaustivamente cada archivo.

El repositorio Thalyx se mantuvo sin modificaciones durante este trabajo.

## Qué está decidido

Microkernel de capacidades con ámbitos de trabajo; dominios de memoria como frontera adversaria; autoridad por grants/facetas; IPC con origen y cargo; barrera/drenaje/retirada separados; memoria sellada; CPU con cuotas agregadas y recuperación reservada; estado versionado y publicación durable en usuario; x86_64/UEFI y Rust; ABI propio; port de fuente y Linux permanente.

Las decisiones son contratos para implementar, no resultados del sistema. [Registro de decisiones](../decisions/README.md).

## K2

### Interfaz V0

`abi/schema/v0.json` es la única fuente de la interfaz. `tools/gen_abi.py` deriva de ella los bindings Rust y C y las fixtures; `tools/check_abi.py` comprueba que lo generado coincide byte a byte con el esquema, que ningún número significa dos cosas, que las 325 aserciones de disposición en C se cumplen y que 15 fixtures decodifican en 669 desplazamientos. Cuatro de cuatro comprobaciones pasan.

La tabla asigna cinco entradas de kernel —consulta de versión, invocación, consulta de límites, salida y lectura del reloj monotónico— y 51 operaciones repartidas en nueve tipos de objeto. La quinta entrada la añadió K2 al ejecutar: seis operaciones toman un plazo monotónico absoluto y no había forma de leer el reloj para expresarlo. K5 añade la sexta, `THREAD_POINTER_SET`, y se cuenta en su sección.

### Sustrato del kernel

El kernel implementa los mecanismos K2 del lado del núcleo: tabla generacional de objetos y capacidades (`obj.rs`), ámbitos con límites, ventana de CPU, barrera, drenaje y retirada (`scope.rs`), objetos de memoria con sellado y copia (`memobj.rs`), IPC copiado con invocaciones, origen y cargo (`ipc.rs`), señales y timers (`events.rs`), log de control con celdas reservadas (`ctrl.rs`), copia de usuario acotada (`ucopy.rs`) y las capacidades de arranque del primer supervisor (`k2boot.rs`).

`kernel/src/api/` es el único punto de admisión: estructura, autoridad y efecto se comprueban en ese orden, y los pasos de autoridad y efecto ocurren bajo una sola toma del cerrojo de la máquina, que la barrera también toma. Las 51 operaciones del esquema tienen manejador y se despachan desde ahí.

`kernel/src/syscall.rs` conecta las cinco entradas asignadas con ese punto de admisión. Las dos entradas de andamiaje de K1 siguen presentes, marcadas como tales y sin tocar ningún objeto, para que la regresión de K1 se siga ejecutando contra el kernel en el que K2 creció.

El arranque elige la ruta por el paquete: si un módulo declara `SUPERVISOR`, el kernel construye la raíz, el log de control y ese único dominio con su manifiesto explícito de capacidades; si no, cae en los dominios de K1. `tools/build_image.py --phase k2` produce ese paquete; el kernel es el mismo binario en ambas fases, de modo que «K1 sigue pasando» es una afirmación sobre un kernel y no sobre dos.

### Primera vertical

Tres programas de usuario ejecutan la vertical descrita en la ruta. `k2super` recibe el manifiesto de arranque y construye todo lo demás por la interfaz: dos ámbitos hijos, un endpoint de trabajo, uno de supervisión, una señal, un objeto de memoria, y los dominios `k2server` y `k2client` con sus capacidades y su canal de fallos. No usa ninguna llamada que abra un recurso por nombre; localiza las imágenes preguntando a cada objeto sellado su etiqueta.

El supervisor publica además una página inmutable por el camino que el contrato de memoria pide: llena un objeto, copia sus bytes a otro dentro del kernel, lo mapea con escritura, y lo sella —lo que obliga al sello a retirar ese mapeo antes de prometer nada—; después lo mapea de solo lectura en el dominio que va a leerlo.

`k2client` arranca con tres capacidades y nada más: una faceta del endpoint, un buffer propio y la página sellada. Estrecha una capacidad de solo lectura sobre ese buffer y se la entrega al servidor con la llamada. `k2server` la lee, no consigue escribir por ella, no consigue ampliarla, admite un efecto sobre la invocación y retiene la obligación.

Con el servidor reteniendo trabajo, el supervisor cierra el ámbito del cliente. El informe de drenaje después de la barrera sigue contando el hilo, la invocación y el efecto pendientes; la retirada se rechaza mientras eso siga siendo cierto. El servidor observa `ORIGIN_FENCED`, comprueba que la capacidad derivada por el cliente ya no funciona, y resuelve la obligación —que sí sobrevive a la barrera, porque su grant lo patrocina el ámbito del servidor—. La llamada del cliente vuelve `CANCELLED` y su segunda llamada se rechaza con `SCOPE_CLOSED`. Solo entonces el ámbito queda quiescente y la retirada libera lo que patrocinaba.

Cuarenta y un controles negativos se ejecutan dentro de esa vertical y los cuarenta y uno son rechazados con el código esperado, en trece estados distintos. Ninguna operación que debía ser rechazada tuvo éxito, y ninguna devolvió un estado que el programa no esperara.

La vertical cubre además lo que EXP-02 y EXP-06 piden en alcance K2: copiar una capacidad, cerrarla, ver la ranura liberada volver bajo otro handle que el antiguo no nombra, sostener una capacidad vencida, once peticiones malformadas rechazadas sin efecto parcial, y los límites de tabla, cola y log alcanzados con el cierre todavía disponible.

### Lo que cierra la interfaz

Cuatro pasos más la llevan de 42 a 51 operaciones ejercidas, y cada uno comprueba algo que los contratos afirmaban y nada había mirado:

`receipts` lee el log de control por capacidad. Recorre el anillo entero un lote acotado a la vez, reconociendo lo leído para que el más antiguo avance, y comprueba que las secuencias crecen estrictamente. Entre los 58 recibos busca el que el guion escribió al empezar declarando un origen falso, y lo encuentra con el origen real estampado encima. Leer y reconocer son derechos distintos: un handle estrechado a `LOG_READ` lee y no consigue reconocer.

`grant_barrier` cerca **una concesión** y no un ámbito. Dos generaciones bajo la faceta del supervisor, cercando la de en medio: la barrera marca dos nodos, alcanza la derivación de abajo, se detiene en la faceta de arriba, y el ámbito que las patrocina sigue abierto. `CAP_DRAIN_STATUS` informa del linaje cercado, que es lo que esa operación existe para hacer.

`spare_domain` construye un cuarto dominio, lo consulta, le añade un hilo y lo termina sin activarlo nunca. Detener el servidor o el cliente sería detener lo que está bajo prueba.

`ceilings` estrecha el ámbito `srv` de 128 a 96 páginas antes de que se le cargue nada, y comprueba los dos rechazos que lo acotan: por encima de lo que el padre tiene, `LIMIT_EXHAUSTED`; por debajo de lo que el subárbol ya tiene cargado, `STATE_CONFLICT`.

`SIGNAL_QUERY` se ejerce a ambos lados de la espera del timer: antes el bit no está levantado, después la secuencia avanzó y el bit ya no está, porque esperar consume.

### Puerta K2

`tools/check_k2.py` decide **21** criterios por separado, desde los registros del kernel allí donde el kernel emite uno. La regresión de K1 es uno de ellos, porque una puerta K2 que pasara con el arranque protegido roto mediría otra cosa.

El criterio 21 es la cobertura, y exige las 51: lee el recuento de `k2.coverage` **y** la lista de `k2.operation_untouched` y los compara entre sí, porque el kernel deriva los dos del mismo mapa de bits y un recuento que afirme el total mientras la lista todavía nombra algo es un kernel que cuenta mal. Sin ese criterio la puerta podía seguir verde mientras la mayor parte de la interfaz quedaba sin ejercer, y el veredicto no lo habría dicho.

`--self-test` daña la ejecución de **28** formas distintas y comprueba que cada daño hace fallar al criterio que le corresponde. Tres son de cobertura: sin registro, un recuento por debajo de lo asignado, y un recuento coherente consigo mismo que la lista contradice.

### Lo que la vertical corrigió del sustrato

Ejecutar los mecanismos encontró trece defectos que compilar no encuentra, y los trece están corregidos: el creador de un objeto de memoria no recibía derechos comunes sobre lo que acababa de crear; un dominio no podía reclamarse receptor de un endpoint propio, de modo que ningún supervisor podía tener canal de fallos; la barrera no alcanzaba a los grants patrocinados por el ámbito cerrado, así que la autoridad delegada seguía funcionando después del cierre; cercar un ámbito con un hilo vivo lo dejaba permanentemente inejecutable y la ejecución no terminaba; el informe de drenaje contaba hilos despachados en lugar de hilos vivos, declarando quiescencia con un dominio todavía en pie; la retirada marcaba el ámbito y no liberaba nada; el despacho descartaba la respuesta de toda operación que rechazara, de modo que el informe de `SCOPE_RETIRE` nunca llegaba a quien `DRAIN_INCOMPLETE` manda mirarlo; el límite de paralelismo de un ámbito nunca se comprobaba; una vinculación de recuperación cargaba el ámbito del cliente cerrado en vez del del servicio, dejando inejecutable al hilo que debía terminar la obligación; y la interfaz tomaba plazos monotónicos sin ofrecer forma de leer el reloj.

Cerrar la cobertura encontró tres más, y los tres están corregidos. Las tres operaciones que hablan de un linaje —`CAP_INSPECT`, `CAP_CLOSE` y `CAP_DRAIN_STATUS`— pasaban por la misma puerta de resolución que todo lo demás, y esa puerta rechaza un linaje cercado o vencido: exactamente el estado del que existen para informar o limpiar. Se midió desactivando la corrección: `CAP_DRAIN_STATUS` no se alcanza nunca —50 de 51, todavía nombrada como no tocada—, `CAP_INSPECT` sobre una concesión cercada se rechaza con `SCOPE_CLOSED`, y `CAP_CLOSE` sobre un linaje vencido se rechaza con `EXPIRED`, dejando un handle que su poseedor no puede soltar. `DOMAIN_ADD_THREAD` colapsaba todo error en `LIMIT_EXHAUSTED` y consultaba el espacio de direcciones antes que el estado, de modo que «este dominio se ha detenido» volvía como «se agotó un límite» o como «ese punto de entrada no está mapeado». Y `CapInfo.lineage_state` cruzaba la frontera como un entero cuyos valores solo nombraba el kernel en constantes privadas; `CapLineage` entra en el esquema. [Detalle](../evidence/k2-objects-authority-work.md).

## K3

### Interfaz V0, ampliada a 59 operaciones

`abi/schema/v0.json` sigue siendo la única fuente. K3 le añade un noveno tipo de objeto —`DEVICE`—, cuatro derechos (`DEVICE_MAP`, `DEVICE_IRQ`, `DEVICE_DMA`, `DEVICE_CONTROL`), el estado `UNSUPPORTED_PROFILE` (−21), las estructuras del camino de dispositivo y ocho operaciones nuevas. `Limits` gana `cpus_online`, de modo que un programa puede saber en cuántos procesadores puede ser planificado; `DomainInfo` gana `entry_point`, que una autoridad necesita para añadir un hilo a un dominio que construyó.

### Arranque de procesadores

`acpi.rs` valida RSDP/XSDT/MADT/MCFG/DMAR por firma, longitud y checksum, y construye un mapa explícito `cpu_index → APIC ID` con el procesador de arranque identificado por su identidad real. `arch/x86_64/ap.rs` es un trampolín 16→32→64 bits en una página bajo 1 MiB; `smp.rs` lo instala, hace INIT/SIPI/SIPI con los tiempos del SDM y espera un handshake explícito. `percpu.rs` da a cada procesador su bloque por `GS`, su GDT/TSS/IST y su ranura de pila de `syscall`. `lapic.rs` tiene backends xAPIC y x2APIC, con la valla explícita antes del `WRMSR` del ICR.

Los recursos de un procesador que no contesta **no se reutilizan**: la ranura se retira y la pila se conserva, porque un procesador que contestara tarde correría sobre memoria de otro. La ejecución prueba ese camino con un APIC id que no existe.

### Planificación multinúcleo

La selección de un hilo, la reserva de saldo y de simultaneidad en todos sus ancestros, y la reclamación del hilo ocurren bajo **una sola** toma del cerrojo. El cuanto sale de lo que la reserva concedió, no de una constante. Un hilo que otro procesador todavía nombre como actual o como saliente no es candidato en ningún otro. Cada ámbito registra lo máximo que llegó a tener comprometido, lo máximo que llegó a tener corriendo, las reservas concedidas y rechazadas, y cuánto se pasó una ventana de lo que podía admitir; el planificador registra el intervalo más largo que cubrió un solo cobro, que es lo que hace atribuible ese exceso en lugar de excusable.

### Invalidación entre núcleos y reclamación diferida

`tlb.rs` publica una generación monótona de invalidación, cada procesador acusa la que ha retirado, y un iniciador espera a que todos hayan pasado por la suya. El refresco antes de despachar usuario cierra la carrera del procesador que entra después. `MEMORY_SEAL`, `DOMAIN_UNMAP`, `DEVICE_UNMAP_REGION` y `DEVICE_RESET` salen del despacho bajo cerrojo y **no responden** hasta que todos acusan. Los marcos de un dominio reclamado pasan por una cuarentena sellada con la generación vigente y salen de ella cuando todos han invalidado.

Un espacio de direcciones registra además la máscara de todos los procesadores en los que ha llegado a ejecutarse, y una retirada la publica al lado del recuento instantáneo: el instante casi nunca es el interesante, y la máscara es el conjunto al que la invalidación tiene que llegar.

### Camino de dispositivo

`pci.rs` direcciona por ECAM, mide los BARs con la decodificación de memoria apagada y recorre capacidades con límite y detección de ciclos. `device.rs` valida las estructuras virtio contra su BAR y contra la tabla MSI-X —una estructura que comparta página con la tabla se rechaza en el descubrimiento—, mantiene la sesión del dispositivo y pone en cuarentena las concesiones. `api/devops.rs` implementa las ocho operaciones. La entrada de la tabla de interrupciones la escribe el kernel; maestro de bus y reset se quedan con quien asigna y no con quien conduce. [ADR-009](../decisions/ADR-009-device-path-and-dma-profiles.md) registra por qué el camino es ese y no otro.

El perfil de DMA es `WEAK_TRUSTED_DRIVER` y el kernel lo declara con las palabras que le corresponden: `enforced_by=nothing_driver_is_trusted`. `STRONG_IOMMU` se rechaza con `UNSUPPORTED_PROFILE`, y sigue rechazándose cuando hay una unidad DMAR **descrita** pero no programada, que es el perfil de control que la puerta también ejecuta.

### Paquete K3

`k3super` construye cuatro ámbitos y siete dominios y después hace, desde otro procesador, lo que solo significa algo cuando hay más de uno: quitarle un mapeo a un dominio de dos hilos, sellar un objeto mientras un escritor lo intenta, levantar una barrera mientras se admiten llamadas, y parar un dispositivo bajo su driver. `k3worker` sirve cuatro papeles desde una sola imagen: el papel se lo escribe el supervisor en una página que le mapea de solo lectura, de modo que qué es un dominio no es elección del programa. `k3driver` es un driver virtio-blk moderno completo, con validación de cada entrada del anillo de usados y un autotest de ese validador contra entradas que fabrica en la única página del anillo que el supervisor **no** concedió al dispositivo.

### Puerta K3

`tools/check_k3.py` decide **28** criterios por separado. Las regresiones de K1 y K2 son dos de ellos, y la plataforma es otro: el número de procesadores del `run.json` tiene que coincidir con los que arrancaron, porque una puerta multiprocesador leída de una ejecución uniprocesador es el error que conviene hacer imposible. Solo tres criterios leen notas de un programa y los tres lo dicen en su título; la convención que en K2 quedó a medias aquí está entera.

`--self-test` daña la ejecución de **57** formas distintas, una cada vez, y un criterio nombrado tiene que notar cada una.

### Lo que la ejecución corrigió del sustrato

Seis defectos que compilar no encuentra, más dos de la evidencia y dos del propio paquete. Los tres primeros son fatales y comparten familia —suponer que el estado de un hilo basta para decir de quién es, que en un procesador es cierto—: dos procesadores tomando el mismo hilo `Ready` y ejecutándolo sobre una pila de kernel; la reclamación liberando la pila o el espacio de direcciones que otro procesador todavía usaba; y un hilo que se bloquea, es despertado por un receptor en otro procesador y despachado por un tercero antes de que el procesador en el que sigue ejecutando haya guardado dónde continuar. Los otros tres son de contabilidad: una reserva que acotaba la admisión y no la ejecución; un dominio de dos hilos que solo devolvía uno a su ámbito, dejándolo permanentemente no quiescente; y un registro de mapeo que nombraba por índice una concesión sobre la que no retenía referencia. [Detalle](../evidence/k3-smp-devices.md).

## K4

Completo dentro de su alcance, que es un medio de bloques real conducido por el driver del invitado y **no** un corte de energía. Lo que sigue describe lo construido y ejecutado; lo que K4 no demuestra está en [qué no existe](#qué-no-existe-todavía) y, con detalle, en [la evidencia](../evidence/k4-durable-state.md).

### Formato del almacén, fijado antes de escribir nada

`abi/schema/k4-store-v1.json` es la única fuente del formato durable y del protocolo del servicio: geometría del medio, prefijos de dominio de digest, magias, nueve enumeraciones, 20 estructuras y 22 vectores dorados. `tools/gen_k4_format.py` deriva de él el módulo Rust que el invitado codifica (`user/k4fmt/src/generated.rs`), el módulo Python con el que la puerta decodifica un medio real (`tools/k4_format.py`) y los bytes dorados en `abi/fixtures/k4-store-v1/`. Ninguno de los tres se escribe a mano.

Generar los dos lados no los hace estar de acuerdo por argumento: comparten desplazamientos, así que por supuesto coinciden en eso. Lo que compra el arreglo es que ni el servicio ni la puerta definen el formato. Los digests los calcula un SHA-256 escrito en este repositorio dentro del invitado y `hashlib` en el anfitrión, y los vectores dorados son a lo que se somete a los dos.

El contrato pedía fijar el formato **antes** de escribir datos que se pretendan conservar. Eso es lo que este paso hace, y por eso es el primero.

### Qué decide el formato

Un registro ocupa bloques enteros y su digest cubre la cabecera hasta el propio campo de digest y después la carga, de modo que una escritura rota daña su propio registro y nunca uno ya durable. Cada registro enlaza con el anterior por digest y su secuencia crece exactamente en uno dentro de una arena. Dos superblocks alternos con generación y digest seleccionan la arena; el válido de generación más alta gana. El digest de contenido lleva prefijo de dominio, tipo y longitud dentro de la preimagen, así que un objeto de cero bytes y un árbol vacío no comparten identidad. La identidad de una petición cubre todos los inputs que deciden su significado, que es de donde sale `CONFLICT`.

La directiva de fallos es andamiaje y está marcada como tal: vive en un bloque **fuera** del almacén, lleva su propia magia, y ninguna estructura del formato la nombra.

### Comprobador del formato

`tools/check_k4_format.py` decide **12** criterios por separado. Cuatro son controles negativos ejecutados, no afirmaciones: seis daños distintos a un registro —un byte de carga, la secuencia, la magia, una longitud mayor que sus bloques, un recuento de bloques cero, y una cola rota tras el primer sector— y tres a un superblock, con la exigencia añadida de que los originales intactos se sigan aceptando.

Ese último control encontró algo que conviene decir en lugar de fingir que se probó: todos los registros pequeños caben en un sector, y un desgarro de un registro así lo deja entero o ausente, lo cual es una propiedad de la geometría y no del checksum. El vector dorado que el control usa es por eso un registro de objeto de 1024 bytes, que cruza el sector; el desgarro se aplica donde significa algo.

### Contrato del paquete y driver de bloque

`user/k4fmt/src/pkg.rs` fija lo que los programas del paquete K4 acuerdan y ningún programa decide por su cuenta: qué ranura de capacidad guarda qué, dónde aterriza cada mapeo, qué significa cada bit de señal, y la forma de los dos protocolos —el de bloques y el del estado—. No es autoridad: una ranura no nombra nada hasta que el supervisor instala una capacidad en ella.

`user/k4disk` es el transporte virtio-blk de K3 con dos cosas añadidas —una interfaz de servicio, para que el servicio de estado llegue al medio por capacidad y no por un dispositivo propio, y un motor de fallos— y ninguna quitada. El motor es andamiaje y está marcado como tal: puede dejar una escritura sin emitir, emitir solo su primer sector, negarla como fallo del medio, retenerla y emitirla después de la siguiente, o cerrar el paso a toda escritura posterior. Cada una se cuenta, y esos recuentos son lo que la puerta contrasta contra el medio. Cada una se ejecutó al menos una vez en la matriz de casos.

El esquema creció dos modos de fallo, `IO_ERROR` y `REORDER`, y el campo reservado de la directiva pasó a ser `stop_at_next_flush`, que hace terminar la ejecución donde habría flush para que un modo que solo importa antes de un flush pueda observarse después de uno.

### Servicio, cliente y supervisor

`user/k4store` es el motor del almacén con un bucle de protocolo delante: catorce operaciones, el principal tomado de la faceta que el kernel autenticó y nunca de un campo de la petición, y la capacidad prestada tomada del mensaje y no de un número. `user/k4client` es una imagen y cuatro papeles —publicador, rival, lector y broker— escritos en una página que el supervisor mapea de solo lectura. `user/k4super` construye la ejecución: driver, servicio y clientes, cada uno con lo que le corresponde y nada más.

### La imagen K4, la matriz de casos y la puerta

`tools/build_image.py` tiene fase `k4`. `tools/run_k4.py` ejecuta un **caso**: un escenario, una directiva de fallos y un número de tramos sobre un medio creado una vez y arrastrado de un tramo al siguiente. Un tramo se corta —el que el caso nombre— y los demás son la recuperación, con una directiva que no nombra ningún punto. La recuperación nunca consulta la directiva. El anfitrión solo escribe la directiva, en un bloque fuera del almacén: lo que un medio contiene es lo que el invitado puso ahí.

`tools/run_k4_cases.py` ejecuta **quince** casos: un baseline, un corte en cada punto donde una publicación puede cortarse, los cuatro modos de escritura que no ocurre o que ocurre mal, un servicio reemplazado dentro de una ejecución, dos publicadores compitiendo, un broker que no puede decir, un tramo de recuperación cortado para poder leer el abort que escribió, y uno donde el plano de control se pierde.

`tools/check_k4.py` decide **31** criterios por separado, desde dos fuentes que el invitado no narra: los registros del kernel y los bytes del medio, decodificados por el módulo que genera el esquema. `--self-test` daña la evidencia de 48 formas —reescrituras del registro de un tramo, ediciones de bytes de un medio, y daños aplicados a todos los tramos de la matriz para los criterios cuya afirmación es sobre ella entera— y un criterio nombrado tiene que notar cada una.

### Lo que la ejecución corrigió

Nueve defectos que compilar los cinco programas no encontró, descritos uno a uno en [la evidencia](../evidence/k4-durable-state.md). Tres son de la misma familia —dar por hecho que una cosa que se guarda no cuesta nada— y agotaban la tabla de invocaciones, la tabla global de concesiones y el área de preparación. Tres son sobre decir la verdad en la respuesta: una publicación con éxito que no contestaba nada, una escritura que no se pudo pedir informada como almacén dañado, y dos controles negativos que no llegaban a lo que apuntaban. Tres solo aparecen bajo un corte: un servicio cortado que no dejaba terminar la máquina, uno que se comía su propia petición de ser reemplazado, y una cadena de descriptores que decía que una transferencia había ocurrido sin mover un byte.

## Evidencia ejecutada aquí

| Comprobación | Resultado y alcance |
|---|---|
| Reconstrucción de Thalyx | Lectura estática de código, vault, historial y pruebas existentes. Sin build ni ejecución de Thalyx. |
| MODEL-01 | 294 estados / 615 transiciones, con contraejemplos de las afirmaciones/variantes incorrectas. |
| MODEL-02 | 23 casos de caída del protocolo correcto; variantes incorrectas detectadas. |
| Caso ABA | Confirma la necesidad de generación para rechazar expectativas antiguas sobre contenido repetido. |
| Revisión arquitectónica | Hallazgos y correcciones registrados en [la auditoría](../validation/audit.md). |
| Integridad documental | PASS: 43 IDs y enlaces locales. [Comprobador](../../tools/check_vault.py). Además, 36 destinos de código/historia de Thalyx resueltos contra Git. |
| Esquema ABI | PASS en 4 comprobaciones. [Comprobador](../../tools/check_abi.py). |
| Puerta K1 | PASS en 13 criterios decididos por separado desde los registros del kernel, con controles negativos. [Detalle y límites](../evidence/k1-protected-boot.md). |
| Regresión K1 sobre el sustrato K2 | PASS en los mismos 13 criterios con los mecanismos K2 compilados dentro del kernel. |
| Puerta K2 | PASS en 21 criterios decididos por separado, la mayoría desde los registros del kernel. [Detalle y límites](../evidence/k2-objects-authority-work.md). |
| Cobertura de la interfaz en K2 | 51 de 51 operaciones alcanzadas por el despacho, ninguna nombrada como no tocada. Es criterio de la puerta K2. |
| Autocomprobación de la puerta K2 | 28 ejecuciones dañadas de una forma cada una, las 28 detectadas por el criterio que les corresponde. |
| Regresiones K1 y K2 sobre el sustrato K3 | PASS en 13 y en 21 criterios con el SMP y el camino de dispositivo compilados dentro del mismo kernel. |
| Puerta K3 | PASS en 28 criterios decididos por separado, todos menos tres desde los registros del kernel. [Detalle y límites](../evidence/k3-smp-devices.md). |
| Puerta K3, perfil de control | PASS en los mismos 28 con `-device intel-iommu`: unidad descrita, `remapping_programmed=0`, perfil fuerte igualmente rechazado. |
| Cobertura de la interfaz en K3 | 37 de 59 alcanzadas, las 26 que las afirmaciones K3 necesitan entre ellas y las 22 restantes nombradas una a una. Es criterio de la puerta K3. |
| Autocomprobación de la puerta K3 | 57 ejecuciones dañadas de una forma cada una, las 57 detectadas por el criterio que les corresponde. |
| Formato del almacén K4 | PASS en 12 criterios, nueve de ellos daños aplicados y rechazados. [Comprobador](../../tools/check_k4_format.py). |
| Puerta K4 | PASS en 31 criterios decididos por separado, desde los registros del kernel y desde los bytes del medio. [Detalle y límites](../evidence/k4-durable-state.md). |
| Autocomprobación de la puerta K4 | 48 ejecuciones dañadas de una forma cada una —registro, medio y matriz entera—, las 48 detectadas por el criterio que les corresponde. |
| Regresiones K1, K2 y K3 sobre el sustrato K4 | PASS en 13, 21 y 28 criterios con el estado durable compilado dentro del mismo kernel. |
| Cobertura de la interfaz en K4 | 33 de 59 alcanzadas, las 23 que las afirmaciones K4 necesitan entre ellas y las 26 restantes nombradas una a una. Es criterio de la puerta K4. |
| Puerta K5 | PASS en 47 criterios decididos por separado sobre cuatro ejecuciones —`smoke`, `surface`, `work`, `engine`— y ocho tramos de cinco casos, desde los registros del kernel y los bytes del medio; la comparación con Linux es ejecución del anfitrión etiquetada como tal. [Detalle y límites](../evidence/k5-thalyx-port.md). |
| Autocomprobación de la puerta K5 | 87 ejecuciones dañadas de una forma cada una, las 87 detectadas por el criterio que les corresponde. |
| Protocolos K5 | PASS: el Rust y el C generados coinciden con `abi/schema/k5-proto-v1.json`. [Comprobador](../../tools/check_k5_proto.py). |
| Regresiones K1–K4 sobre el sustrato K5 | PASS en 13, 21, 28 y 31 criterios con la sexta entrada, el estado `Held` y la terminación que alcanza compilados dentro del mismo kernel. |
| Puertas K1–K5 en dos plataformas | PASS en 13, 21, 28, 31 y 47 criterios, con autocomprobaciones 28, 57, 48 y 88, en TCG y en KVM, sobre el kernel K6. `tools/run_gates.py` las corre en serie en cada plataforma a un árbol propio. La puerta K5 sobre KVM pasó en cuatro de cinco ejecuciones consecutivas: el escenario de cancelación depende de dónde cae una barrera dentro de una inferencia y los registros de traza mueven ese punto en milisegundos. |
| Campaña K6 de referencia | Seis rondas, tres brazos, dieciocho arranques completos, 516 entradas sin muestra perdida ni operación rechazada, calibraciones a 0,03 %. [Detalle y límites](../evidence/k6-comparison-hardening.md). |
| Puerta K6 | PASS en 19 criterios decididos por separado desde los artefactos de la campaña —registros del kernel, notas, digests, inventario— más las cinco regresiones. [Comprobador](../../tools/check_k6.py). |
| Autocomprobación de la puerta K6 | 27 campañas dañadas de una forma cada una, las 27 detectadas por el criterio que les corresponde. |
| Esquema de benchmarks K6 | PASS: el C y el Rust generados coinciden con `abi/schema/k6-bench-v1.json` y cada benchmark declara su equivalencia. [Comprobador](../../tools/check_k6_bench.py). |

Todas ejecutadas en el estado actual del árbol, con **el mismo binario de kernel** en las seis fases: `fmt` limpio, ninguna advertencia de compilación, ABI 4/4, vault 43 notas PASS, modelos PASS, puerta K1 13/13, puerta K2 21/21 con autocomprobación 28/28, puerta K3 28/28 con autocomprobación 57/57 y en los dos perfiles de plataforma, y cero resultados inesperados en las ejecuciones K2 y K3 —ni una nota de resultado no esperado, ni una operación que debiera haber sido rechazada y no lo fuera—. Los dos fallos de usuario de K3 son los dos que la ejecución provoca a propósito. Las imágenes y el kernel se reconstruyen byte a byte desde un árbol limpio.

Clippy deja **27 advertencias de estilo**, todas de tres familias —implementar a mano `is_multiple_of`, `Range::contains` y la división comprobada, e indexar un array con la variable de un bucle—. Diecisiete venían de antes de K3; las diez restantes están en archivos que K3 escribió o reescribió (`tlb.rs`, `acpi.rs`, `api/devops.rs`, `arch/x86_64/lapic.rs`, `pci.rs`, `device.rs`). Ninguna está arreglada, y ninguna fase ha afirmado que sus archivos estén limpios de clippy.

## Cobertura real de la interfaz

El kernel cuenta cuáles de las **59** operaciones asignadas alcanza el despacho y emite `k2.coverage` con el total, más un `k2.operation_untouched` por cada una que no se tocó. Cada puerta exige lo que su paquete debe recorrer, comparando el recuento contra la lista, de modo que la cobertura no puede bajar en silencio.

El paquete K2 alcanza **51 de 51** de las operaciones que existían cuando se escribió, y la puerta K2 lo exige nombrando explícitamente las ocho de dispositivo como las únicas que puede no tocar: cualquier otra sin tocar la hace fallar.

El paquete K3 alcanza **37 de 59**, y la puerta K3 exige las **26** sobre las que descansan sus afirmaciones —las ocho de dispositivo enteras, más mapear, desmapear, añadir hilo, activar, sellar, leer, escribir, cercar, drenar, retirar, consultar, llamar, recibir, responder, esperar y levantar señales, armar timer y cerrar capacidad—. Las 22 restantes pertenecen a caminos que este paquete no recorre y están nombradas una a una en el propio registro. K3 no es K2 con más procesadores y no pretende volver a recorrer la interfaz entera.

El paquete K4 alcanza **33 de 59**, y la puerta K4 exige las **23** sobre las que descansan sus afirmaciones: llamar, recibir, responder, admitir un efecto, leer, acusar, añadir y consultar el log de control, cinco de dispositivo, consultar memoria, mapear, instalar capacidad, activar dominio, levantar y esperar señales, derivar, cerrar e inspeccionar capacidades, y enlazar una faceta. `INVOCATION_RESOLVE` no está entre ellas y sí se exige: solo ocurre cuando una publicación **no** se compromete, así que la puerta la busca en los tramos cortados y no en el baseline, donde exigirla obligaría a fallar una publicación a propósito.

Tres cosas que estos números **no** dicen:

- Cada operación está ejercida por **un** camino. Cobertura de interfaz no es cobertura de caminos, y la segunda sigue abierta.
- El contador marca una operación al resolver su especificación, **antes** de comprobar derechos, así que una operación solo intentada y rechazada contaría. Lo que garantiza que no ocurre son las comprobaciones que los programas hacen sobre el resultado, no el contador.
- Que dos paquetes cubran juntos las 59 no es lo mismo que una ejecución que las cubra todas. Ninguna lo hace.

## Qué no existe todavía

Lo que K3 demuestra está acotado por la plataforma en la que se demuestra. EXP-02, EXP-03, EXP-04 y EXP-06 quedan ejecutados **en su alcance K3** —multiprocesador, con un dispositivo, sin estado durable—; EXP-05 queda ejecutado **salvo el DMA tardío**, que es justamente la parte que necesita hardware que esta plataforma no tiene.

Lo que no existe, en orden de cuánto se parece a existir:

- **Aislamiento de DMA.** No hay unidad de remapeo programada. El perfil débil es lo único que esta plataforma sostiene, el kernel lo dice en cada registro que lo menciona, y el perfil fuerte se rechaza con un estado propio en lugar de aproximarse. Un driver no confiable **no** está contenido aquí, y ninguna nota puede decir lo contrario. [ADR-009](../decisions/ADR-009-device-path-and-dma-profiles.md), [OQ-05](open-questions.md).
- **Hardware físico.** Todo es QEMU: TCG para las puertas canónicas, KVM para las mismas puertas y para K6. El inventario de CPU, firmware, dispositivos y grupos de aislamiento de la máquina de desarrollo está hecho; arrancar sobre ella, no.
- **Más de un dispositivo, y rutas de interrupción legadas.** Una función virtio-blk moderna con MSI-X. No hay IOAPIC, INTx, hotplug, NUMA, suspensión, virtio-net ni GPU, y ninguno está a medias.
- **Durabilidad frente a un corte de energía.** K4 demuestra que el almacén sobrevive a que **el escritor** desaparezca en cualquier punto: la supresión la hace el driver del invitado y QEMU ve solo las escrituras emitidas. Qué habría sobrevivido a que se fuera la luz, y qué hace el flush de una controladora real con su caché de escritura, no se ha medido y por tanto no se declara.
- **Un almacén grande, o de trabajo.** Dos arenas de 256 bloques, 40 entradas de objeto, 24 ranuras de preparación, cinco workspaces. Los límites se ejercen y se rechazan con estados propios; nada de esto se parece a un almacén real en tamaño.
- **Escalabilidad del IPC más allá de cuatro pares, y su límite exacto.** K6 midió el cerrojo único como el límite: con cuatro pares, 1 806 idas y vueltas por corte de 10 ms contra 11 912 en Linux, sin mover con el número de pares. `perf/ipc-scalability` lo partió —la máquina se toma compartida en el viaje de ida y vuelta de IPC, con un cerrojo por registro— y `scale.ipc:4` subió a 8 518, con la espera de cerrojos caída al 4 % del tiempo de máquina. Lo que queda, medido y acotado, es el coste de la contabilidad de ámbito por la cadena de ancestros: neutralizarla por completo (sonda desechada) sitúa el techo teórico en 11 070, todavía bajo los 11 912 de Linux. Abaratarla sin permitir que una reserva por procesador rechace a un hermano con cupo real es el problema abierto. [La campaña](../evidence/k6-performance-campaign.md), [OQ-04](open-questions.md).
- **Rendimiento sobre hardware físico.** Todo K6 es KVM sobre la máquina de desarrollo: las instrucciones corren en el procesador físico, pero los dispositivos, el firmware, las interrupciones y cada salida del invitado son del emulador y del anfitrión. La máquina está inventariada; el kernel no ha arrancado sobre ella.
- **Thalyx entero sobre este kernel.** K5 ejecuta la semántica de Thalyx —versión, contexto, programa, herramienta, motor, publicación, evidencia— con sus mecanismos: QuickJS real, llama.cpp real, el servicio de estado de K4. No ejecuta el binario de Thalyx: no hay `cargo` ni compilador de Rust dentro del kernel, no hay comprobación de tipos, el modelo es el que Thalyx usa para ejercitar su propio motor y no uno de trabajo, hay un hilo de cómputo, y el perfil de Linux es una lectura y no una ejecución. La prueba formal general sigue sin existir; el rendimiento está medido en K6 sobre KVM y sobre nada más.

El plano de diagnóstico de K1 sigue presente, sigue sin ser el plano de recibos, y sus dos entradas de andamiaje permanecen para que la regresión de K1 se siga ejecutando. El primer supervisor no tiene supervisor: su fallo termina la ejecución. No se ha retirado ni reemplazado Linux.

## K5

Completo dentro de su alcance, que es el port de Thalyx sobre este kernel con un modelo pequeño, un hilo de cómputo y sin compilador dentro; lo que K5 no demuestra está en [qué no existe](#qué-no-existe-todavía) y, con detalle, en [la evidencia](../evidence/k5-thalyx-port.md).

### El target nativo y su runtime

Hay un segundo target de usuario, `x86_64-thalyx`, definido en [`tools/build_native.py`](../../tools/build_native.py) y no como un triple: ELF64 estático a 0x400000 con tres segmentos separados R-X / R-- / RW-, SSE y SSE2 en hardware con `-mfpmath=sse` y **sin AVX** —el kernel guarda el área `FXSAVE` heredada de forma ansiosa y no habilita `XSAVE`—, sin protector de pila, sin tablas de desenrollado y con `-nostdinc`, de modo que la única libc en la ruta de búsqueda es `user/native`.

`user/native` es lo que un programa C pisa: asignación sobre objetos de memoria que el programa crea contra su propio ámbito y mapea en su propio dominio, hilos que el supervisor construye antes de activar y que bloquean en una señal real, tiempo desde el único reloj que hay, transporte acotado con el origen que estampa el kernel, tiempo civil en UTC porque no hay zona horaria que fingir, y una biblioteca matemática escrita aquí que **no** redondea correctamente y lo dice donde importa.

### Cuatro etapas ejecutadas

**`smoke`** establece el target: el kernel construye un dominio desde una imagen C, la activa y la planifica; el programa mide `.bss` a cero, lee límites y reloj, ejecuta un bucle de coma flotante sembrado por el plan que el anfitrión puso en la imagen y reporta un resultado que el anfitrión recalcula y exige idéntico, hace crecer un heap de 192 páginas en tres arenas leyendo de vuelta lo que escribió por los mapeos nuevos, y se detiene contra el techo de páginas de su ámbito con `LIMIT_EXHAUSTED` del kernel.

**`surface`** ejecuta la semántica de Thalyx sobre el estado administrado de K4 —el mismo servicio, el mismo driver, el mismo medio, sin cambios—: versión identificada, `contexto` respondido desde esa versión y declarando su propia cobertura, workspace privado por `FORK`, cambio real, publicación condicionada con CAS y evidencia releída por el mismo camino que usaría cualquiera. Todo pasa por **una sola superficie de verbos**. La validación de esta etapa es una afirmación que el trabajo hace sobre sí mismo, y eso es lo que su etiqueta de integración parcial significa.

**`work`** sustituye las dos cosas que faltaban. El programa lo ejecuta **QuickJS de verdad** —quickjs-ng 0.15.1, el que empaqueta `rquickjs 0.12`, que es el que ejecuta la revisión de Thalyx de referencia—, traído fijado y comprobado por digest en vez de vendorizado, porque [OQ-14](open-questions.md) sigue abierta. Y la validación la decide una **herramienta nativa real**: `user/ncheck`, lanzada en un dominio propio con un ámbito propio, a la que se le da exactamente un objeto de memoria **sellado** con el candidato, que compila cada nombre JavaScript con el parser de QuickJS y ejecuta las aserciones del propio candidato. Su código de salida decide la publicación. Lanzar es un servicio que el trabajo pide, no un poder que tenga: un dominio de trabajo no posee ninguna capacidad que construya un dominio.

**`engine`** pone el motor CPU residente real: **llama.cpp en `b10665`**, la etiqueta que Thalyx fija, con el `serve_one` de `engine/thalyx-engine.cpp` de la revisión de referencia portado sobre el transporte de este kernel, y el modelo que `dev/tiny-model.py` de Thalyx escribe; los tres traídos fijados y comprobados por digest —el de llama.cpp por el árbol de los 403 archivos que se compilan, no por el archivo—. La toolchain que eso necesita es la mitad del trabajo: C++ **alojado** contra la `libstdc++` preconstruida del GCC del anfitrión, con `user/native` implementando la clausura de lo que esa biblioteca referencia —TLS con la disposición de la ABI ELF, `pthread` sobre hilos construidos, futex, constructores, desenrollado, archivos administrados donde `/bulk` es el único nombre— y el enlace demostrando la clausura sobre la imagen. El kernel gana su sexta entrada, `THREAD_POINTER_SET`, porque la biblioteca de C++ guarda su estado detrás de `FS` y un hilo sin base `FS` no la ejecuta. El motor es un dominio en un ámbito propio con los pesos sellados mapeados de solo lectura; carga una vez, sirve dos peticiones del trabajo, se **vincula** a cada invocación para que el kernel cobre la inferencia al ámbito de quien preguntó, y la residencia queda cobrada al suyo: dos ámbitos, dos números del kernel. La versión publicada lleva las completaciones, y son byte a byte las que el motor de Thalyx, sin cambios, da en Linux sobre el mismo modelo y los mismos prompts —ejecución en el anfitrión, etiquetada como tal, usada solo como la otra mitad de la comparación—.

### La matriz EXP-10

`tools/run_k5_cases.py` ejecuta cinco casos sobre la misma imagen de la etapa `engine`, con el escenario y el corte escritos por el anfitrión en la directiva del medio como en K4. **`rivals`**: dos trabajos con dos principales, dos ámbitos y dos runtimes sobre la misma versión y el mismo motor, ejecutando el mismo programa sobre intenciones distintas; el motor sirve a los dos intercalados y cobra cada inferencia al ámbito de quien preguntó, sin un digest en común; uno publica y el otro es rechazado con `GENERATION_STALE`, vuelve a empezar sobre la versión que ganó, encuentra el marcador ya reemplazado y abandona con honestidad —el módulo publicado lleva exactamente una marca de las dos que el anfitrión recalcula—; el rival pide la operación reservada al publicador y el servicio lo rechaza con `FORBIDDEN`, y las tablas de capacidades que el kernel escribió tienen objetos distintos en cada ranura privada. **`cancel`**: un trabajo pide cuatrocientos tokens y su ámbito es cercado un cuarto de segundo después; la barrera encuentra la obligación del motor sobre él, el motor lo nota entre tokens, resuelve la invocación como abortada, y solo entonces el ámbito se retira quiescente; el motor no termina y un trabajo sin relación le pregunta después y publica. **`cut-after-prepare`, `cut-after-commit`, `io-error-commit`**: los cortes de K4 dentro de esta vertical, con un segundo tramo en el que el trabajo encuentra sus identidades gastadas —abortada, comprometida, abortada— y hace la vertical entera otra vez, o encuentra su cambio publicado y no publica nada, o publica lo que el medio rechazó escribir el día anterior.

### La parte K5 de EXP-11

Dos perfiles en una fixture del esquema de protocolos, en el vocabulario de la frontera con Thalyx. El nativo lo devuelve el verbo `perfil` tal cual y el programa lo publica con la versión; la puerta exige que sea byte a byte el de la fixture y contrasta cada rasgo con un registro que no escribió el declarante —el `enforced_by` del kernel para DMA, un solo hilo en los `sched.bound` del motor, lo que llama.cpp contesta sobre `mmap`, las identidades de herramienta construidas y rechazadas, las regiones de dispositivo en un solo dominio, los recibos drenados, los tipos de objeto de la interfaz—. El programa pide la comprobación que el perfil dice no tener —`Check::Rust`, una toolchain real— y el lanzador la rechaza con `NO_SUCH_TOOL`: `not_proven` con razón, exigido por el propio programa, nunca el parser en lugar del compilador. El perfil de Linux sale de la reconstrucción estática y la puerta registra las diferencias rasgo a rasgo; coinciden en uno, el motor residente, que es el que la comparación de completaciones ejercita. La pregunta al motor —prompts, contexto, hilos— sale de la misma fixture para el programa nativo, la referencia y la puerta.

### Puerta K5

`tools/check_k5.py` decide **47** criterios por separado, cuatro de ellos las regresiones K1–K4, diez la matriz EXP-10 y tres la parte K5 de EXP-11. El decisivo no lo narra el invitado: el medio lleva una versión publicada cuyo módulo lleva la semilla que el anfitrión eligió y escribió en la imagen, decodificada por el módulo que genera el esquema de K4. Dos más se apoyan en lo mismo: la herramienta leyó exactamente los bytes que el medio dice que la versión enlaza, y el registro de validación durable nombra la identidad de herramienta que realmente corrió. `--self-test` daña la evidencia de **87** formas y un criterio nombrado nota cada una.

Los protocolos entre los dos lenguajes salen de `abi/schema/k5-proto-v1.json`: `tools/gen_k5_proto.py` deriva el módulo Rust y la cabecera C con aserciones estáticas sobre cada desplazamiento, y `tools/check_k5_proto.py` regenera y compara.

### Lo que la ejecución corrigió

Diecinueve defectos que compilar no encuentra, descritos uno a uno en [la evidencia](../evidence/k5-thalyx-port.md). Cuatro los encontró la matriz, porque tenía dos trabajos donde la etapa tenía uno: un supervisor que se quedaba sin tabla de handles con dos runtimes y una herramienta, un endpoint de supervisión cuya cola de ocho celdas no admitía el octavo dominio con canal de fallos, un lanzador que solo sabía servir a un trabajo —ahora sirve por la faceta que el kernel autenticó— y un motor que contestaba con `INVOCATION_REPLY` a un llamante cancelado en vez de resolver la obligación, dejando su ámbito sin retirar para siempre. Seis son de la etapa `engine`, y dos de esos son del kernel: `DOMAIN_ADD_THREAD` reutilizaba el hilo inicial de un dominio en construcción porque su ranura estaba en estado `Empty` —hay un estado `Held` para eso—, y una terminación por autoridad no alcanzaba a un hilo corriendo en otro procesador hasta que fallaba sobre los mapeos retirados, contando un fallo de usuario contra un dominio ya terminado —ahora la terminación interrumpe a ese procesador y toda vuelta a anillo 3 abandona un hilo parado con un registro propio—. Los otros cuatro: ningún hilo tenía base `FS`, un techo de ámbito escrito para lo que se usa y no para lo que llama.cpp pide, una imagen de cinco megabytes contra un techo de cuatro, y cabeceras generadas que un compilador de C++ no aceptaba.

Los nueve anteriores: dos de la etapa `smoke` —una imagen sin script de enlace y un objeto de memoria sin `MEMORY_MAP` en sus derechos máximos—. Cinco de la etapa `work`: un candidato sin `MEMORY_SEAL` en sus derechos máximos, un ámbito sin `SCOPE_CREATE` para fabricarlo, y tres de la misma familia —`JS_ParseJSON` y `JS_Eval` exigen terminador nulo y la región compartida no lo tiene, y un cuerpo de función compilado como script—. Y dos que no son de este código: un supervisor que no drenaba el plano de control hasta llenarlo, y el servicio de estado de K4 contestando `NOT_FOUND` cuando lo que le pasaba era que no podía leer el medio. Lo segundo era un defecto de K4 y está corregido.

## K6

Completo dentro de su alcance, que es KVM sobre una máquina física inventariada y no arrancada; lo que K6 no demuestra está en [qué no existe](#qué-no-existe-todavía) y en [la nota de evidencia](../evidence/k6-comparison-hardening.md).

### Los protocolos emparejados

`abi/schema/k6-bench-v1.json` nombra diecisiete benchmarks y dice de cada uno qué hace cada lado, qué garantías tiene cada primitiva y qué puede concluir la comparación: *equivalente*, *comparable*, *distinto* o *solo nativo*. `tools/gen_k6_bench.py` deriva de él la cabecera C que compilan los dos lados y el módulo Rust del supervisor, y `tools/check_k6_bench.py` regenera y compara. `tests/k6/bench.c` es el bucle, el contador y el informe, una sola fuente; `plat_native.c` y `plat_linux.c` son los dos backends y no saben nada del otro. En nativo, `user/k6super` construye cliente, servidor de cuatro endpoints y el motor de K5 con el constructor de K5 —en modo silencioso y con un procesador de presupuesto—, audita el log mientras el plan corre y ejecuta las entradas que el cliente no tiene autoridad para correr. En Linux, `tools/build_k6_linux.py` construye un invitado que es solo Linux: el kernel del anfitrión copiado y digerido, `bench.c` estático como init, el motor de Thalyx sin cambios enlazado desde los objetos de la referencia K5, el modelo fijado.

### Las medidas

`tools/run_k6.py` corre campañas de rondas —un arranque de cada brazo por ronda, en orden sorteado, con el mismo plan— sobre una máquina QEMU fija en KVM, confinada a un hilo por núcleo físico, y conserva plan, digests, línea de órdenes, registro crudo, inventario del anfitrión y revisión. `tools/k6_analysis.py` decide con bootstrap sobre rondas y dice solo lo que la equivalencia permite; `tools/inventory_host.py` registra la máquina física. La campaña de referencia y sus resultados están en [la nota](../evidence/k6-comparison-hardening.md) y sus artefactos en [`evidence/k6/baseline/`](../evidence/k6/baseline/).

### Lo que medir corrigió del sustrato

Ocho cuellos, cada uno medido antes y después, en [ADR-010](../decisions/ADR-010-k6-parameters-and-wake-policy.md) y en la nota: el plano de diagnóstico, que escribía por operación y bajo KVM costaba más que la operación —ahora dos clases de registro y una bandera de paquete que retiene la traza y la declara—; las ranuras de dominio y ámbito, que nunca se reciclaban —ahora bajo demanda, conservando su contabilidad hasta entonces—; el presupuesto de raíz, que era una ventana en cuatro procesadores; el despertar, que esperaba un tick —ahora un plazo de gracia de 10 µs con su trade medido—; el log de control de 64 celdas con lotes de 4 y un auditor que dormía o sondeaba —ahora 256, 16 y una lectura que espera, ABI 0.4—; el cerrojo sin turno que dejaba sin procesador a la reclamación; la reserva de tablas por mapeo que nunca volvía; y las notas y el presupuesto del motor K5, que hacían de una inferencia de 1,8 ms una de 40. Tres esperas por tiempo de los paquetes K3, K4 y K5 pasaron a ser esperas por condición, porque el planificador dejó de esperar un tick por despertar y esos tiempos dejaron de valer.

### Puerta K6

`tools/check_k6.py` decide 19 criterios desde los artefactos de la campaña y las cinco regresiones desde los veredictos canónicos, con autocomprobación de 27 daños. PASS sobre la campaña de referencia.

## K7

K7 optimiza lo que se puede verificar con una medida antes/después, contra el baseline de dos réplicas del árbol `1eacb11`. Las cifras de K7 las mide su cierre; esta sección dice qué se cambió y qué busca mover, no cuánto movió.

### El puerto serie y el arranque (k7-01)

Observado en el baseline y en [EXP-13](../evidence/exp13-k1-final.md): el kernel escribe cada registro por COM1 de forma síncrona y por sondeo, unos 15,6 µs por byte y 2,1 ms por registro bajo KVM; casi todo el arranque hasta `LINK_READY` es escritura de registros, y una transacción con publicación deja cinco registros en su ventana en la imagen de medida.

Implementado, [ADR-011](../decisions/ADR-011-serial-bursts-and-measurement-notes.md):

- **Ráfagas de FIFO.** `Uart::init` lee `IIR` y confirma la FIFO del 16550; `Uart::write_burst` lee el estado de línea una vez por cada dieciséis bytes en lugar de una por byte, o una por byte si la FIFO no se confirmó. `diag` reúne cada registro en un búfer de 256 bytes antes de enviarlo, para que los fragmentos de `write!` no empiecen cada uno con su sondeo. Mismos bytes en el cable, mismo formato `THLX1`, el registro sigue en el cable antes de que su escritor vuelva, la ruta de pánico sigue byte a byte. `diag.summary` añade al final `burst=<n>`, que es la autocomprobación del mecanismo: dice si el arranque escribió por ráfagas. Busca bajar `k7.serial_ns_per_byte` y, con él, `k7.boot_guest_ms` y `k7.boot_host_s`.
- **Notas por transacción solo en la imagen de medida.** La feature `quiet-io` de `k4store` calla también `EFFECT_ADMITTED`, `PUBLISHED` y `ROOT_HEAD` por publicación, y una feature `quiet-io` nueva de `k5super` calla `AUDIT_EFFECT` por recibo; `tools/build_image.py --k4-io-notes off` activa las tres. Las imágenes de las puertas K4 y K5 no la llevan y conservan esas notas; `WORK_OPENED` (`0x510a`) y la nota de control (`0x510e`) no se tocan, y el total `AUDIT_EFFECTS` sigue en el log. Busca bajar `k7.tx_serial_records`.

Diseñado, no medido: la escritura en QEMU sigue costando una llamada al sistema por byte en el anfitrión, así que la reducción por byte esperada es menor que la mitad. Las puertas K1–K6 y las pruebas de EXP-13 no se corrieron dentro de este mini sprint; las verifica el cierre de K7.

### La fase `open` y las llamadas a K4 (k7-02)

Observado en el baseline y en [EXP-13](../evidence/exp13-k1-final.md): de los +27,7 ms que separan K1 de L1 en cada transacción, +23,0 ms están en `open`. Cada petición de Thalyx llega a `k5link`, que atendía cada `get` de un objeto durable volviéndolo a leer de K4. Leer un objeto de más de 3 072 bytes cuesta una llamada IPC por su árbol y otra por cada trozo. Cada publicación volvía a preparar y a ligar la raíz entera. El baseline cuenta 992 llamadas a K4 por ronda de los 16 casos.

Implementado en `user/k5link/src/managed.rs` ([ADR-012](../decisions/ADR-012-link-object-copies-and-root-reuse.md)), sin cambiar el protocolo de K4 ni lo que se escribe en el medio:

- **Copias en la arena.** La arena del link ya no se vacía al publicar. Lo que tiene de objetos durables queda como copia, y un objeto leído de K4 se copia en ella. Un `get` o la comprobación del árbol de un candidato sirven esa copia sin llamar a K4. Cuando un `put` necesita sitio, `make_room` descarta las copias y conserva lo no publicado, así que el límite de lo no publicado es el mismo. Busca bajar `k7.tx_open`, y con él `k7.tx_e2e` y `k7.tx_store_calls`.
- **No repetir lo que la raíz publicada tiene.** Al construir una raíz, el link calcula el digest de cada árbol y cada trozo. Si la raíz recién publicada ya alcanza el objeto, no lo prepara otra vez. Si ya liga un nombre al mismo objeto, no lo vuelve a ligar. Cualquier desenlace de `PUBLISH` que no sea `OK` olvida lo sabido. `FORK`, `FREEZE` y `PUBLISH` siguen pasando por el grant del trabajo, así que la barrera de un trabajo sigue llegando como `SCOPE_CLOSED` del kernel. Busca bajar `k7.tx_store_calls`.
- **Autocomprobación.** El `stats` del plano de control añade `reads_cached`, `stages_reused` y `binds_reused` después de los campos que ya tenía.

Diseñado, no medido: no se tocó el costo de cada llamada (consola virtio, despertar, IPC del kernel), solo cuántas se hacen. Un `get` servido desde la copia ya no observa el medio, y ninguna puerta ni prueba de EXP-13 lee ese caso. Las puertas K1–K6 y las pruebas de EXP-13 no se corrieron dentro de este mini sprint; las verifica el cierre de K7.

### Las demás fases de la transacción (k7-03)

Observado en el baseline y en [EXP-13](../evidence/exp13-k1-final.md): después de `open`, la única fase con una diferencia clara es `settle` (+0,19 ms). `end`, `request`, `validate` y `observe` quedan entre +0,003 y +0,022 ms, con intervalos que no tocan el cero. Observado en el código de Thalyx (`3ac575f`, sin cambios): en el brazo K1, `end` es un solo `published` que el link sirve de memoria, y `settle` son tres idas y vueltas más la publicación en K4. `request`, `validate` y `observe` corren en el anfitrión sin llamar al link. Observado en este árbol: toda interrupción de dispositivo llega al procesador 0. El despertar que provoca iba al procesador donde el hilo corrió por última vez, y si ese procesador estaba parado había que interrumpirlo. Además, el link esperaba la interrupción de transmisión completada de cada respuesta, que QEMU ya había entregado al socket.

Implementado ([ADR-013](../decisions/ADR-013-idle-takes-interrupt-wakes-and-quiet-tx.md)):

- **El procesador ocioso que recibe la interrupción se queda con el hilo.** En `select_cpu` (`kernel/src/sched.rs`), un despertar cuyo despertador es el hilo ocioso del procesador encola el hilo ahí si ese procesador no está retenido, su cola está vacía y el procesador donde el hilo corrió por última vez no está sondeando. Así no hay interrupción entre procesadores ni hay que despertar un segundo procesador virtual. `sched.cpu_summary` añade `idle_takes` al final de cada fila. Busca bajar `k7.tx_end` y `k7.tx_settle`, y el bloque de K4 se beneficia igual en la publicación.
- **El link no espera a que el dispositivo tome la respuesta.** Las colas de transmisión de los puertos llevan `VIRTQ_AVAIL_F_NO_INTERRUPT`. `send` publica y vuelve, y `drain_tx` recoge la completación anterior antes de escribir otra vez el búfer. Solo si la encuentra en vuelo activa la interrupción mientras espera. `Console::flush` vacía las colas antes de que el link salga. El `stats` del plano de control añade `tx_waits` al final. Busca bajar `k7.tx_end` y `k7.tx_settle`, con una interrupción y un despertar del link menos por respuesta.

Diseñado, no medido. Que esto mueva `k7.tx_request`, `k7.tx_validate` y `k7.tx_observe` es una hipótesis: esas fases no tocan la máquina virtual, y el cambio solo les quitaría actividad del invitado que compite por el anfitrión. El tick periódico de cada procesador no se tocó. No se cambió Thalyx. Las puertas K1–K6 y las pruebas de EXP-13 no se corrieron dentro de este mini sprint; las verifica el cierre de K7.

### Entradas al kernel, IPC y capacidades (k7-04)

Observado en el baseline: la puerta K6 da 23/24 en las dos réplicas. Solo cae `no_loss`, por `LIMIT_EXHAUSTED` en el brazo nativo en `scale.ipc` con 2 y 4 pares y en `ipc.caps` con 1 capacidad, una vez por boot afectado, en rondas distintas. `ipc.lineage` crece de 2 065 ns con 0 derivaciones a 2 330 ns con 28. Observado en el código (`b6b8017`): la suma acotada de `scope.rs` sumaba y luego deshacía, así que una reserva que iba a fallar dejaba su contador por encima del techo mientras se deshacía, y un hermano que lo leyera en ese momento se rechazaba. La recorrida de un linaje recorría la cadena de ámbitos hasta la raíz dos veces por nodo. La resolución tomaba dos veces el cerrojo del nodo hoja. El estado de cada ámbito compartía línea de caché con sus contadores de despacho.

Implementado ([ADR-014](../decisions/ADR-014-exact-scope-reservations-and-lineage-walks.md)), sin cambiar ningún techo, estado, interfaz ni orden de comprobación:

- **Suma acotada exacta.** `bounded_add` mueve el contador con `compare_exchange` solo a un valor que no pasa el techo, así que un rechazo se decide contra lo que el contador tiene de verdad. Sirve a `reserve` y a `reserve_cpu`. Busca que `no_loss` vuelva a pasar y la puerta K6 a 24/24 (`k7.behavior_gates`). Que los −9 del baseline vengan de aquí es una hipótesis, porque la imagen de medida no escribe qué recurso rechazó.
- **Recorridas sin arreglo y una memoria por linaje.** `is_open`, `reserve` y `release` siguen los enlaces al padre sin construir un `Chain`. `scope::OpenMemo` hace que `walk_lineage` pregunte una sola vez por cada ámbito en una recorrida. `resolve_entry` le pasa a la recorrida la copia del nodo hoja que ya leyó. Busca bajar `k7.k6_ipc_lineage_*` (0, 8, 16 y 28), `k7.k6_ipc_call_*`, `k7.k6_ipc_caps_*` y `k7.k6_cap_derive_0`.
- **Disposición de `Scope` por línea de caché.** `repr(C, align(64))`: estructura, identidad y estadísticas, contadores de reserva y contadores de despacho van cada uno en su línea. Una aserción en tiempo de compilación exige que la estructura quepa en la primera. Busca bajar las mismas métricas de IPC, porque cada reserva y cada comprobación de linaje leen la estructura de cada ancestro.
- **Salida de la entrada.** `on_kernel_exit` lee una vez el índice del procesador, y `syscall` comprueba la parada con el hilo que encontró al entrar. Busca bajar `k7.k6_entry_version_0` y `k7.k6_entry_clock_0`, en poco: quita lecturas, no cambia la forma de la entrada.

Diseñado, no medido. Queda una ventana residual: si un ancestro rechaza, los niveles inferiores quedan cargados unos nanosegundos mientras se deshacen. La lectura previa al intercambio puede costar más bajo contención que el `fetch_add`, y su balance con la separación de líneas lo mide el cierre. `k7.k6_audit_drain_0` no tiene un mecanismo dirigido en este cambio. Las puertas K1–K6 y las pruebas de EXP-13 no se corrieron dentro de este mini sprint; las verifica el cierre de K7.

### Memoria (k7-05)

Observado en el baseline: las razones nativo / `linux-nomitig` de K6 son 1,05 y 1,08 para mapear 1 página, 0,257 y 0,264 para mapear 64, y 0,476 y 0,490 para sellar 16. Observado en el código (`2ce6cda`): el sello esperaba la invalidación interrumpiendo a todos los procesadores en línea, aunque estuvieran parados y nunca hubieran cargado el espacio del escritor. `DOMAIN_UNMAP` ya esperaba solo a los que tenían el espacio cargado. `DOMAIN_MAP` y la retirada recorrían la jerarquía de tablas una vez por página. La retirada leía `CR3` en cada llamada. Cada objeto creado recorría las 1024 ranuras de la cuarentena aunque estuviera vacía.

Implementado ([ADR-015](../decisions/ADR-015-targeted-seal-wait-and-table-walks.md)), sin cambiar ninguna interfaz, estado ni orden de comprobación:

- **Espera dirigida del sello.** `tlb::shootdown_withdrawn` espera a los procesadores que las retiradas del objeto registraron (`unmapped_cpus`), y solo a los que no pasaron todavía la generación de la última retirada (`unmapped_at`). Interrumpe solo a esos. Es el mismo argumento que ya usaba `DOMAIN_UNMAP`. `mem.sealed` sigue contando el acuse sobre todos los procesadores en línea. Busca bajar `k7.k6_mem_seal_16`.
- **Una recorrida por tabla.** `AddressSpace::map_run` y `unmap_run` escriben y borran las mismas entradas que el bucle por página, recorriendo PML4, PDPT y PD una vez por tabla. Las usan `DOMAIN_MAP` y `withdraw_map_deferring`, que sigue invalidando con `invlpg` cada página presente del espacio cargado. Busca bajar `k7.k6_mem_map_64` y, en parte, `k7.k6_mem_seal_16`.
- **Raíz cargada y cuarentena vacía.** La retirada compara con la raíz que `switch_space` registró (`tlb::loaded_root`) en lugar de leer `CR3`, y `drain_quarantine` vuelve en cuanto la cuarentena está vacía. Busca bajar `k7.k6_mem_map_1`, que necesita salir de la paridad.

Diseñado, no medido. No se sabe cuánto pesa cada parte. Si el costo de mapear una página lo dominan la creación del objeto o la entrada al kernel, este cambio puede no alcanzar su umbral. El registro del sello nombra ahora la generación de la última retirada del objeto, no una propia. Las puertas K1–K6 y las pruebas de EXP-13 no se corrieron dentro de este mini sprint; las verifica el cierre de K7.

### Planificador, escala y cierre (k7-06)

Observado en el baseline: las razones nativo / Linux de K6 son 0,452 y 0,447 para `sched.wake`; para `scale.ipc` con 1 a 4 pares, de 2,93 a 0,61; para `scale.compute`, entre 0,994 y 0,997; y para `closure.unit`, 0,278 y 0,290. La primera muestra de `closure.unit` de cada ronda del baseline A se reparte en unos 3 µs de valla, 5 de terminación y entre 10 y 14 de retirada, con un intento. Observado en el código (`bc2ba73`): un procesador ocioso al que un tick despertaba sin traerle nada volvía a sondear 200 µs, dos quintos de su tiempo con un tick cada 500 µs. Mientras sondea, lee cada 16 µs las líneas que un procesador ocupado escribe en cada ida y vuelta. El turno ocioso que encontraba un hilo en su cola hacía antes la recogida de dominios muertos y la contabilidad del tick. `SIGNAL_WAIT`, despertado, tomaba dos veces el cerrojo de control. La retirada recorría las 512 ranuras de capacidad de la máquina una vez por cada objeto que liberaba.

Implementado ([ADR-016](../decisions/ADR-016-quiet-idle-and-one-sweep-retirement.md)), sin cambiar ninguna interfaz, estado, parámetro ni orden de comprobación:

- **Sondeo solo después de haber corrido algo.** `halt_counting` (`kernel/src/sched.rs`) dice si el `hlt` terminó sin que se despachara un hilo de usuario. Si fue así, el siguiente `poll_before_halt` mira una vez y vuelve a parar. Un procesador que acaba de dejar de correr un hilo sondea los 200 µs de siempre. Busca subir `k7.k6_scale_ipc_1` a `_3` y `k7.k6_scale_compute_1` a `_3`, que tienen procesadores ociosos.
- **El hilo despertado antes que la contabilidad.** `idle_turn` despacha primero si su cola no está vacía. `reap_dead` y `tick_bookkeeping` quedan para el turno que no encuentra nada y para los ticks. Busca bajar `k7.k6_sched_wake_0`.
- **Una toma del cerrojo al despertar.** `evtops::wait` baja la cuenta de esperantes de la señal en la misma toma que vuelve a mirar los bits. Busca bajar `k7.k6_sched_wake_0`.
- **Un barrido por retirada.** `scope::subtree` calcula el perímetro una vez. `release_sponsored` decide qué objetos se liberan, retira todas sus capacidades en un solo barrido y los libera en el mismo orden de antes. Busca bajar `k7.k6_closure_unit_0`.

Diseñado, no medido. Que las lecturas de los procesadores ociosos pesen en `scale` es una hipótesis. `scale.compute` con cuatro hilos y `scale.ipc` con cuatro pares no tienen procesadores ociosos, y no hay en este cambio un mecanismo dirigido a ellos. La distancia de `scale.compute` con Linux probablemente viene del tick de 2 kHz, que no se tocó. Un despertar que llega a un procesador ocioso después de un tick sin trabajo lo encuentra ahora parado y lo interrumpe; `kicks` en `sched.cpu_summary` dice cuánto. Las puertas K1–K6 y las pruebas de EXP-13 no se corrieron dentro de este mini sprint; las verifica el cierre de K7.

### El motor residente (k7-07)

Observado en el baseline: las razones nativo / `linux-nomitig` de K6 son 2,40 y 2,66 para `engine.load`, entre 1,097 y 1,105 para `engine.infer` con los prompts 0, 1 y 4, y 2,47 y 2,73 para `engine.cancel`. Observado en el código (`023028d`): el modelo de la fixture tiene dos capas y 64 de ancho, así que lo que rodea al cálculo pesa más que el cálculo. `memcpy` y `memset` de la libc nativa avanzaban ocho bytes por paso a cualquier tamaño, y `memmove` hacia atrás y `memcmp` byte a byte. Cada petición vacía unos 256 KiB de contexto con `memset`. `free` coalescía cada bloque para que `malloc` lo volviera a partir. `calloc` volvía a poner a cero memoria que el kernel acababa de entregar a cero. El motor calculaba el FNV del modelo entero aunque estuviera callado y no escribiera la nota. `domain::build` ponía a cero cada página de la imagen antes de copiarle encima los bytes del archivo.

Implementado ([ADR-017](../decisions/ADR-017-resident-engine-libc-and-image-pages.md)), sin cambiar lo que el motor calcula, ninguna interfaz ni el contrato de memoria:

- **Copias y rellenos.** `memcpy` y `memset` (`user/native/src/string.c`) usan `rep movsb` y `rep stosb` desde 256 bytes y dieciséis bytes por paso por debajo. `memmove` con el destino dentro del origen copia desde el final de ocho en ocho, y `memcmp` compara ocho bytes por paso mientras son iguales. Busca bajar `k7.k6_engine_infer_0`, `_1` y `_4`, `k7.k6_engine_cancel_0` y `k7.k6_engine_load_0`.
- **Bloques pequeños conservados y `calloc` sobre memoria nueva.** `free` (`user/native/src/heap.c`) guarda hasta 64 bloques por tamaño, hasta 544 bytes, y `malloc` los devuelve enteros. Antes de crecer o de rechazar, los conservados vuelven a los bins. `bin_of` usa `clz`. `calloc` solo pone a cero los enlaces del bloque libre cuando el bloque sale de la parte todavía no repartida de la arena más nueva. Busca bajar `k7.k6_engine_infer_*` y `k7.k6_engine_cancel_0`, por el muestreo con gramática, y `k7.k6_engine_load_0`.
- **El digest solo cuando se escribe.** `th_main` (`user/nengine/engine.cpp`) calcula el FNV del modelo solo si el motor no está callado. K5 recibe la misma nota y K6 no la recibía. Busca bajar `k7.k6_engine_load_0`.
- **Páginas de imagen sin cero previo.** `FrameAllocator::alloc_filled` (`kernel/src/mm/frame.rs`) copia los bytes dados en el marco y pone a cero el resto. `domain::build` la usa para cada página de un segmento. Cada byte del marco se escribe antes de que sea alcanzable. Busca bajar `k7.k6_engine_load_0`, porque la imagen del motor mapea unas 1 200 páginas.

Diseñado, no medido. No se sabe cuánto pesa cada parte. Este cambio no toca tres cosas:

- Cada lectura del reloj sigue siendo una entrada al kernel.
- La memoria del motor se pone a cero al crearla y no al tocarla.
- El hilo del motor ligado a la petición cancelada sigue sujeto al presupuesto del ámbito que pregunta.

Es una hipótesis que esas tres cosas sean lo que quede de `engine.load` y `engine.cancel`. La puerta K6 compara la respuesta del motor byte a byte en cada muestra, y es la que vería un cambio de resultado. Las puertas K1–K6 y las pruebas de EXP-13 no se corrieron dentro de este mini sprint; las verifica el cierre de K7.

### Tamaño de la imagen (k7-08)

Observado en el baseline y en los artefactos (`759bb62`): la carga del kernel es de 573 440 bytes, el paquete de la imagen de medida de 5 951 488 y el loader de 64 000. Los cuatro programas Rust del paquete pesan entre 1,0 y 1,8 MB cada uno, y la mayor parte son secciones de depuración que nada lee desde el paquete. A los tres segmentos del kernel les sobran 3 217, 2 472 y 3 815 bytes antes de su última página. El kernel se enlaza sin plegado de código idéntico. El loader se compila con `opt-level = 3`.

Implementado ([ADR-018](../decisions/ADR-018-image-size-stripped-modules-folded-kernel-small-loader.md)), sin cambiar ningún segmento cargable de los programas, ninguna interfaz ni ningún parámetro:

- **Programas sin depuración en el paquete.** `tools/build_image.py` aplica `strip --strip-debug` a la copia de cada programa Rust que entra al paquete. La copia completa queda en `build/<imagen>/<programa>.debug.elf`. Busca bajar `k7.size_package`.
- **Plegado en el kernel.** `kernel/build.rs` enlaza con `--icf=all`. Busca bajar `k7.size_kernel_load`.
- **Loader para tamaño.** `Cargo.toml` compila `thalyx-boot-uefi` con `opt-level = "s"`. Busca bajar `k7.size_loader`.

Diseñado, no medido. Que el plegado quite una página del kernel es una hipótesis: al texto le faltan 880 bytes para perder una, y no se sabe cuánto se pliega. Una dirección del kernel puede nombrar ahora a varias funciones plegadas, y un loader compilado para tamaño puede tardar algo más en `k7.boot_guest_ms`. Las puertas K1–K6 y las pruebas de EXP-13 no se corrieron dentro de este mini sprint; las verifica el cierre de K7.

### Tiempo de build y de las puertas (k7-09)

Observado en el baseline y en la última ejecución de las puertas (`ab742e6`): de los unos 280 s de `tools/run_gates.py --platform tcg`, `k4-run` se lleva entre 173 y 189. Un tramo de K4 sin corte tarda unos 6 s de reloj, y en el de `baseline` el trabajo termina antes de 0,7·10⁹ ciclos del invitado mientras el tramo sigue hasta 4,5·10⁹. Lo que falta es el broker, que solo se detenía tras cuatro esperas de 1 s sin intención. El servicio llama al broker solo dentro de una publicación que un cliente espera. `release` generaba depuración completa en todo el espacio de trabajo, aunque el loader la tira al enlazar y los programas entran sin ella al paquete.

Implementado ([ADR-019](../decisions/ADR-019-broker-told-to-stop-and-lighter-release-debug.md)), sin tocar `tools/check_k*.py`, los runners ni lo que contesta el broker:

- **El broker se detiene cuando se lo dicen.** El supervisor de K4 levanta `bit::BROKER_STOP` en la señal `DONE` cuando todos los demás clientes terminaron, o antes de salir cuando el servicio pide el corte. El broker espera en ventanas de 100 ms y, cuando una vence con el endpoint vacío y el bit está, sale de forma voluntaria como antes. Sin el bit, sale tras 4 s sin intención, el mismo total de siempre. Busca bajar `k7.gates_seconds`, en unos 3 a 4 s de reloj por cada tramo de K4 que terminaba esperando al broker.
- **Depuración donde se lee.** `release` usa `debug = "line-tables-only"`, el kernel conserva `debug = true`, y el loader y sus dependencias se compilan sin depuración. Busca bajar `k7.build_seconds`.

Diseñado, no medido. No se sabe cuánto del build es depuración: el kernel la conserva y puede ser el camino crítico. Los tramos donde un cliente queda bloqueado en una llamada al servicio detenido terminan cuando vence su llamada de 8 s, y siguen igual. `k5-cases`, `k5-stages` y las autocomprobaciones no tienen un mecanismo en este cambio. El broker deja ahora un `k2.refused` por cada ventana de 100 ms que vence. Las puertas K1–K6 y las pruebas de EXP-13 no se corrieron dentro de este mini sprint; las verifica el cierre de K7.

### El primer cierre de K7

Observado en `build/k7/check-20261028T122146` (kernel `d6ac7f1`, Thalyx `3ac575f`). Las puertas K1–K5 pasan en las tres ejecuciones, y `ci.py` solo cayó por `cargo fmt --check`. La puerta K6 da 23/24, igual que el baseline: `LIMIT_EXHAUSTED` en `ipc.caps` con 1 capacidad, ronda 3. Ninguna métrica de transacción se midió, porque `/mnt/exp13` dejó de estar montado al reiniciar el anfitrión y la campaña de EXP-13 no pudo crear su arena btrfs. Contra el baseline, varias métricas empeoraron: `k7.size_kernel_load` (618 496 bytes contra 573 440), `k7.k6_closure_unit_0` (0,384 contra 0,278 y 0,290) y `k7.k6_entry_version_0` (0,970 contra 0,859). Otras mejoraron sin llegar al umbral, como las de IPC y `scale.compute`.

Corregido ([ADR-020](../decisions/ADR-020-auditor-settles-after-handoff-and-scope-walks-out-of-line.md)):

- **El auditor de K6 se repone antes de devolver una entrada relevada.** El −9 cae justo después de `closure.unit`: el cliente hizo exactamente las 248 admisiones que caben en las celdas ordinarias del log sin que el auditor drenara ninguna. Las notas del supervisor muestran que su ámbito agotaba el medio ventana en cada ventana y arrastraba el exceso. `entries::settle` duerme, drenando, hasta que empiezan tres ventanas nuevas antes de levantar `HANDOFF_DONE`. Refuta la hipótesis de ADR-014 sobre el origen del −9.
- **Las recorridas de ámbito, fuera de línea, salvo en el camino de IPC.** `scope::is_open`, `reserve` y `release` con `#[inline(never)]`. En `2ce6cda` quedaron copiadas, desenrolladas, en sus 67 llamadores, y eso sumaba unos 40 KB de texto. El texto vuelve de `0x50000` a `0x46000`: 577 536 bytes de carga, todavía una página por encima del baseline. Una medida con todo fuera de línea (`build/k7/check-20261028T130936`) dio la puerta K6 24/24, pero `ipc.lineage` subió unos 35 ns a toda profundidad. Por eso la admisión, la liberación de una invocación, la instalación y la liberación de una capacidad, `OpenMemo` y `begin_effect` usan las formas `_inline`. El rechazo de una reserva quedó en una función fría, y el texto no creció de página.
- **Formato.** `cargo fmt` sobre tres archivos que `bc2ba73` y `d6ac7f1` dejaron sin formatear.
- **Una ranura de pila del kernel reutilizada arrancaba un hilo desde un marco en cero** ([ADR-021](../decisions/ADR-021-kernel-stack-slot-reuse-invalidates-before-writing.md)). En `build/k7/check-20261028T142453`, la ronda 1 nativa terminó en `kernel.panic` con `rip=0` y `rsp` en tope − 0xb0 de la ranura 14: el primer cambio de contexto de un hilo nuevo leyó cero donde estaba su `resume`. La puerta K6 dio 18/24. `allocate_kernel_stack` invalida ahora cada página que mapea antes de escribir por ella, y `release_kernel_stack` sella el retiro después de quitar las entradas. El defecto es anterior a K7, y el pánico es el único en los 219 arranques nativos guardados.

Medido con todo lo anterior en `build/k7/check-20261028T144837`, con el kernel en `d6ac7f1` más estos cambios y Thalyx en `3ac575f`. Las puertas K1–K5 pasan en las tres ejecuciones, la puerta K6 da 24/24 sin pánico, las 7 pruebas de EXP-13 sobre la imagen de medida pasan y `ci.py` pasa. `k7.behavior_gates` y `k7.behavior_tests` (419 contra 392) pasan sobre esa medida, pero `check_k7.py` registra además un `FAIL` porque `measure_k7.py` sale con 1 cuando la campaña de EXP-13 no encuentra su arena. De las métricas, pasan 17 de 48. No pasan las nueve `k7.tx_*`, que no se midieron, ni `k7.size_kernel_load` (577 536 bytes contra menos de 573 440). Tampoco pasan 21 de las de K6: `entry.version`, `entry.clock`, `ipc.call` con 0, 64 y 256 bytes, `ipc.caps` con 1 y 4, `closure.unit`, `audit.drain`, tres de `ipc.lineage`, `scale.compute` con 1 a 4 procesadores, `scale.ipc` con 1 y 3 pares, `sched.wake`, `engine.load` y `engine.infer` con 4: o no mejoran lo suficiente, o su intervalo es más ancho que la mejora. Esos criterios de optimización siguen abiertos. No los cierra ninguna corrección de esta sesión.

Observado sin corregir: `/mnt/exp13` no está montado desde que se reinició el anfitrión. La imagen sigue en `~/exp13.img` (btrfs), pero montarla en esa ruta requiere root, y ninguna sesión de kiln lo tiene: `sudo mount -o loop ~/exp13.img /mnt/exp13`. Sin ese montaje, `measure_k7.py` no mide ninguna métrica `k7.tx_*`.

Observado sin corregir: la retirada de `closure.unit` necesita más intentos. En la primera muestra de cada ronda, el cierre necesitó dos intentos en cuatro de seis rondas y tardó entre 4 y 25 µs. En el baseline A bastó uno en cinco de seis, con entre 10 y 14 µs. Es compatible con lo que [ADR-016](../decisions/ADR-016-quiet-idle-and-one-sweep-retirement.md) anticipó: el hilo despertado por la valla encuentra un procesador parado que hay que interrumpir. Revertir esa política movería `scale.ipc`, que con ella mejoró. La regresión de `entry.version` no tiene diagnóstico: el camino de `syscall` solo cambió en la lectura del hilo a la salida, y su intervalo, [0,913, 1,000], es ancho.

### El segundo cierre de K7

Observado en `build/k7/check-20261028T155106` (kernel `2f7e14f`, Thalyx `3ac575f`, `/mnt/exp13` montado). Las puertas K1–K5 pasan en las tres ejecuciones. La puerta K6 da 24/24 y las 7 pruebas de EXP-13 sobre la imagen de medida pasan, así que `k7.behavior_gates` y `k7.behavior_tests` (419 contra 392) pasan. De las 48 métricas pasan 21:

- Las nueve `k7.tx_*` se midieron por primera vez. Pasan `tx_e2e`, `tx_open`, `tx_settle`, `tx_store_calls` y `tx_serial_records`. No pasan `tx_end`, `tx_request`, `tx_validate` ni `tx_observe`.
- No pasan 22 de las de K6.
- No pasa `k7.size_kernel_load`, con 577 536 bytes.

Corregido ([ADR-022](../decisions/ADR-022-short-entries-quick-exit-and-kernel-image-pages.md)):

- **Las consultas de versión y de reloj, por un camino propio.** `syscall::handle` las atiende en línea (`quick`), y el despachador general queda fuera de línea. La salida de la consulta de versión no lee el contador. La de reloj compara la reserva con la lectura que responde, tomada al entrar. La salida sin trabajo son cuatro lecturas en línea. La entrada de `syscall` no ejecuta `cld`, porque `IA32_FMASK` ya limpia DF. Busca bajar `k7.k6_entry_version_0` y `k7.k6_entry_clock_0`.
- **Dos páginas menos de carga.** `begin_response` queda fuera de línea (estaba copiada en 21 operaciones, unos 4,3 KB). Las estadísticas de cerrojos van agrupadas por procesador en lugar de una línea por clase (2,5 KiB menos). Compilado, son 569 344 bytes contra 577 536. Busca bajar `k7.size_kernel_load`.

Medido con todo lo anterior en `build/k7/check-20261028T170958`, sin otro cambio sobre `2f7e14f` y con Thalyx en `3ac575f`:

- Las puertas K1–K5 pasan en las tres ejecuciones. La puerta K6 da 24/24 con autocomprobación 27/27, y las 7 pruebas de EXP-13 pasan, así que `k7.behavior_gates` y `k7.behavior_tests` (419) pasan.
- Pasan 22 de las 48 métricas, una más que en el segundo cierre: `k7.size_kernel_load` pasa con 569 344 bytes.
- `k7.k6_entry_version_0` no pasa por la cuantización. El brazo nativo cae en 50 ns en las seis rondas, contra 60 de Linux, y la razón da 0,83352. El umbral del baseline, 0,833494, es exactamente 50,01 / 60, y la desigualdad es estricta. Hace falta que la mayoría de las muestras caiga en 40 ns. Hoy cae entre un 41 % y un 46 % en cinco rondas, y ninguna en la sexta.
- `k7.k6_entry_clock_0` no pasa. Cae en 60 ns en cinco rondas y en 70 en una.
- Fallan las mismas 26 métricas que en el segundo cierre, sin `k7.size_kernel_load`. Otras dos variantes del camino propio (`fx-e6`, `fx-e7`) no movieron nada más allá de la variación entre arranques.

Observado sin corregir. La entrada de IPC que sigue a una entrada relevada corre unos 420 ns más lenta en cada llamada, durante toda la entrada (≈1 640 ciclos de mediana). Las entradas relevadas son `closure.unit`, `engine.load` y `engine.cancel`. El plan se sortea con la misma semilla en cada campaña, así que esas posiciones son siempre las mismas. En `2f7e14f` explican:

- los extremos altos de los intervalos de `k7.k6_ipc_lineage_0`, `_16` y `_28`;
- los de `k7.k6_ipc_call_256`;
- las rondas lentas de `k7.k6_sched_wake_0`.

En `d6ac7f1` (`check-20261028T122146`) y en el baseline, esas mismas posiciones medían lo normal. Medido con `closure.unit`, `ipc.call:0` e `ipc.lineage:0`, cuatro rondas nativas por campaña. La causa es la espera de [ADR-020](../decisions/ADR-020-auditor-settles-after-handoff-and-scope-walks-out-of-line.md):

| Campaña | Espera antes de `HANDOFF_DONE` | Resultado |
| --- | --- | --- |
| `fx-o1` | Tres ventanas | La entrada que sigue es lenta en 4 de 4 |
| `fx-o5` | Una ventana | Lenta en 4 de 4 |
| `fx-o3` | Hasta media ventana después de la frontera | Lenta en 3 de 4 |
| `fx-o2` | Ninguna | Normal en 4 de 4 |
| `fx-o4` | Tres ventanas, y `select_cpu` prefiere un procesador que sondea antes que el propio | Sigue lenta, y la IPC normal también sube un poco. Descartado. |

Quitar la espera no es una corrección: es lo que evita el `LIMIT_EXHAUSTED` de `no_loss` en la puerta K6. El mecanismo no está identificado. Una hipótesis, no medida: la IPC es rápida cuando el auditor está retenido por su ventana y no compite con ella. Con espera, el auditor empieza la entrada con presupuesto. En `fx-o2` los procesadores 1 a 3 registraron entre 1 591 y 2 377 `budget_stalls`, y en `fx-o5` entre 44 y 1 418, en una sola ronda.

Observado sin mecanismo en este cambio:

- `tx_request`, `tx_validate` y `tx_observe` corren en el anfitrión sin llamar al link. Sus umbrales piden que K1 − L1 quede por debajo de 1,0, 0,55 y 1,5 µs. Midieron [9,4, 27,2], [0,57, 6,1] y [1,05, 7,9] µs.
- `ipc.call` e `ipc.caps` necesitan entre un 3 % y un 8 % menos por llamada.
- `scale.compute` y `scale.ipc` con 1 y 3 pares.
- `closure.unit`, cuya retirada sigue necesitando dos intentos.
- `engine.load`, `engine.infer` con el prompt 4, `engine.cancel` y `audit.drain`.

## Reanudar aquí

**Último hito terminado y pusheado:** **K6 completo en su alcance** —protocolos emparejados, campaña de referencia, EXP-12, ocho cuellos medidos y corregidos, parámetros V0 revisados, inventario físico, puerta K6— en la rama `feat/k6-comparison-hardening-reference`. K5 quedó completo en `feat/k5-thalyx-port-tools`.

**EXP-13, rama `feat/exp13-k1-final` (2026-10-27):** el tercer backend de Thalyx sobre este kernel ejecuta; las cuatro puertas de EXP-13 pasan PROVEN bajo KVM y la campaña causal corrió: seis rondas, los tres brazos, cerrada. Para llegar ahí se corrigió, además del link, un defecto del kernel: `establish_supervisor` instalaba el segundo dispositivo asignado en la tabla de otro dominio (`ebb3311`). Sobre ese cambio, las puertas K1–K5 pasan en TCG (`c5d2ece`); K6 no se volvió a correr. Desde `c5d2ece` el brazo K1 tiene dos imágenes —con trazas para las puertas, `TRACE_OFF` para medir— y las cuatro puertas pasan en las dos. Una tercera imagen, sólo para medir (`60ef827`), calla además las notas por operación de K4 con la feature `quiet-io`, que sólo la etapa `thalyx` activa; sin ella K4 es byte a byte el de antes fuera de la depuración, y K4 y K5 pasan en TCG. Las cuatro puertas pasan sobre ella. Tres pilotos de una ronda congelaron la configuración (`K1 − L1` de +2 604, +1 012 y +29,7 ms con trazas, sin trazas y con la imagen de medida). **La campaña** —seis rondas pareadas, KVM, imagen de medida, kernel en `50b3449`, Thalyx en `9d3ccce`— dio, en latencia extremo a extremo por transacción con IC 95 % sobre rondas: `K1 − L0` = −8,2 ms [−12,2, −6,1], `L1 − L0` = −36,9 ms [−38,1, −35,2], `K1 − L1` = +27,7 ms [+24,6, +31,8], este último casi entero en la fase `open` (+23,0 ms). **Veredicto:** la migración ayudó, por el rediseño administrado; una ventaja específica del kernel en latencia no se demostró; la única diferencia declarada a favor del kernel es la garantía de escritor exclusivo. Límites: QEMU/KVM con lanzamiento, validación y reloj en el host, ~5 registros seriales por transacción, una máquina, y la tabla de transporte vacía, que es no medida. El análisis y los digests de los 288 crudos están en [`evidence/exp13/campaign-1/`](../evidence/exp13/campaign-1/). [La evidencia](../evidence/exp13-k1-final.md).

**Qué está verde, medido en este árbol y con el mismo binario de kernel en las seis fases:** puerta K1 13/13, puerta K2 21/21 con autocomprobación 28/28, puerta K3 28/28 con autocomprobación 57/57, puerta K4 31/31 con autocomprobación 48/48, formato K4 12/12, puerta K5 47/47 con autocomprobación 88/88, puerta K6 24/24 con autocomprobación 27/27, protocolos K5 PASS, esquema K6 PASS, ABI 4/4, vault PASS, modelos PASS, `fmt` limpio, construcción del workspace sin advertencias. Las puertas K1–K5 pasan además en KVM.

**Una advertencia sobre cómo medir:** las puertas se ejecutan en serie; K3 y K5 tienen criterios que dependen de carreras reales y un anfitrión cargado las mueve. La puerta K5 sobre KVM pasó en cuatro de cinco ejecuciones seguidas. Una campaña K6 necesita `THALYX_ACCEL=kvm` y no debe compartir la máquina con otra ejecución.

**Qué demostró exactamente K6:** que este kernel y Linux, sobre la misma máquina virtual y desde la misma fuente, pueden compararse con un veredicto acotado por una equivalencia declarada antes de medir; que la inferencia del mismo modelo con la misma respuesta byte a byte cuesta un 10 % más aquí; que una llamada IPC con recibo, origen estampado y objeto de respuesta cuesta 0,41 de un socket con credenciales; que el cómputo escala en cuatro procesadores y el IPC no; y que ocho cuellos que ninguna puerta anterior podía ver aparecen en cuanto se mide, y se corrigen con la cifra en la mano.

**Qué no demostró:** nada sobre hardware físico; nada sobre escalabilidad del IPC más allá de medir que no la hay; ninguna superioridad; y no el tamaño de muestra que su propio estimador pide para dos filas. Los límites de K3, K4 y K5 se heredan enteros.

**Siguiente paso exacto al reanudar:** no hay una fase K7 definida en [la ruta](phases.md). El cerrojo de la máquina ya se partió en `perf/ipc-scalability` ([la campaña](../evidence/k6-performance-campaign.md), [OQ-04](open-questions.md)); lo que queda de esa pregunta es abaratar la contabilidad de ámbito por la cadena de ancestros sin permitir exhaustion falsa, no un rediseño del cerrojo. Lo que sigue pendiente, en orden de cuánto pesa: esa contabilidad; arrancar en la máquina inventariada ([OQ-05](open-questions.md)); y diagnosticar la cola del cierre.

## Siguiente trabajo

Según [la ruta](phases.md), las seis fases están ejecutadas en su alcance y este árbol es la primera referencia sobre KVM. Lo que K4 deja abierto y K5 y K6 heredan —el perfil de durabilidad declara que la supresión la hace el driver del invitado, y el de DMA que no hay unidad de remapeo programada— necesita hardware físico, que está inventariado y no arrancado. El cerrojo único de la máquina que K6 dejó medido y sin mover se partió después, en `perf/ipc-scalability`; lo que esa rama deja medido y sin mover es el coste de la contabilidad de ámbito por la cadena de ancestros. [La campaña](../evidence/k6-performance-campaign.md).

No hay una elección técnica pendiente que deba devolver el diseño al usuario. [Las preguntas abiertas](open-questions.md) especifican qué dato falta y con qué decisión conservadora avanzar.
