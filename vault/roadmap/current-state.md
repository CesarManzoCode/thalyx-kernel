---
id: STATE-001
kind: evidence
status: observed
---
# Estado actual

**2026-09-10 · Fundación 0.1.0 · K0, K1, K2, K3 y K4 completos. K5 en curso. K6 pendiente.**

> **Punto de reanudación.** Este documento es el checkpoint. La sección [Reanudar aquí](#reanudar-aquí) dice exactamente dónde empieza el trabajo siguiente; no hace falta reauditar K0, K1, K2, K3 ni K4.

## Qué existe

Un vault de 45 notas con constitución, 13 contratos de arquitectura, glosario, reconstrucción de Thalyx, 32 fuentes primarias anotadas, nueve decisiones, 23 invariantes, alternativas, integración Linux/nativa, experimentos y ruta de implementación. Se incluyen herramientas documentales y dos modelos finitos de investigación con controles negativos y resultado versionado.

Existe además un kernel que arranca, un sistema de capacidades que se ejecuta sobre él, una ejecución en cuatro procesadores con un dispositivo de bloque real conducido desde usuario, y un almacén de versiones que sobrevive a la ejecución que lo escribió. El workspace tiene loader UEFI, protocolo de arranque, kernel con `arch/x86_64`, interfaz V0 generada desde un esquema, trece programas de usuario y dos bibliotecas que comparten —el runtime y el formato durable—, con toolchain fijada y construcción reproducible desde un solo script para las cuatro fases.

La imagen K1 arranca en QEMU, ejecuta dominios en ring 3, los preempta con timer, contiene sus fallos ilegales y sobrevive. [Qué se ejecutó exactamente](../evidence/k1-protected-boot.md). La imagen K2, con el mismo kernel y otro paquete, construye un supervisor con un manifiesto explícito de capacidades y deja que ese supervisor cree todo lo demás por la interfaz. [Qué se ejecutó exactamente](../evidence/k2-objects-authority-work.md). La imagen K3, otra vez con el mismo kernel, arranca cuatro procesadores, reparte trabajo entre ellos con presupuesto agregado, retira traducciones con acuse de todos, y le entrega a un dominio de usuario una función virtio-blk moderna con la que lee, escribe y hace flush. [Qué se ejecutó exactamente](../evidence/k3-smp-devices.md). La imagen K4, con el mismo kernel una cuarta vez, publica versiones inmutables sobre un medio de bloques que persiste entre arranques, y la matriz corta la ejecución en cada punto donde una publicación puede cortarse para arrancar de nuevo sobre lo que el corte dejó. [Qué se ejecutó exactamente](../evidence/k4-durable-state.md).

La base de evidencia de Thalyx está fijada al commit `0492f72e487e2463b0d7b938365a8b3383364cb9`: inventario de 407 commits alcanzables, 96 archivos del vault, 38 rutas de código/configuración de evidencia y 15 revisiones históricas seleccionadas. Inventariar no significa haber ejecutado ni auditado exhaustivamente cada archivo.

El repositorio Thalyx se mantuvo sin modificaciones durante este trabajo.

## Qué está decidido

Microkernel de capacidades con ámbitos de trabajo; dominios de memoria como frontera adversaria; autoridad por grants/facetas; IPC con origen y cargo; barrera/drenaje/retirada separados; memoria sellada; CPU con cuotas agregadas y recuperación reservada; estado versionado y publicación durable en usuario; x86_64/UEFI y Rust; ABI propio; port de fuente y Linux permanente.

Las decisiones son contratos para implementar, no resultados del sistema. [Registro de decisiones](../decisions/README.md).

## K2

### Interfaz V0

`abi/schema/v0.json` es la única fuente de la interfaz. `tools/gen_abi.py` deriva de ella los bindings Rust y C y las fixtures; `tools/check_abi.py` comprueba que lo generado coincide byte a byte con el esquema, que ningún número significa dos cosas, que las 325 aserciones de disposición en C se cumplen y que 15 fixtures decodifican en 669 desplazamientos. Cuatro de cuatro comprobaciones pasan.

La tabla asigna cinco entradas de kernel —consulta de versión, invocación, consulta de límites, salida y lectura del reloj monotónico— y 51 operaciones repartidas en nueve tipos de objeto. La quinta entrada la añadió K2 al ejecutar: seis operaciones toman un plazo monotónico absoluto y no había forma de leer el reloj para expresarlo.

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

Todas ejecutadas en el estado actual del árbol, con **el mismo binario de kernel** en las tres fases: `fmt` limpio, ninguna advertencia de compilación, ABI 4/4, vault 43 notas PASS, modelos PASS, puerta K1 13/13, puerta K2 21/21 con autocomprobación 28/28, puerta K3 28/28 con autocomprobación 57/57 y en los dos perfiles de plataforma, y cero resultados inesperados en las ejecuciones K2 y K3 —ni una nota de resultado no esperado, ni una operación que debiera haber sido rechazada y no lo fuera—. Los dos fallos de usuario de K3 son los dos que la ejecución provoca a propósito. Las imágenes y el kernel se reconstruyen byte a byte desde un árbol limpio.

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
- **Hardware físico.** Todo es QEMU con TCG. El inventario de CPU, firmware, dispositivos y grupos de aislamiento sigue sin hacerse.
- **Más de un dispositivo, y rutas de interrupción legadas.** Una función virtio-blk moderna con MSI-X. No hay IOAPIC, INTx, hotplug, NUMA, suspensión, virtio-net ni GPU, y ninguno está a medias.
- **Durabilidad frente a un corte de energía.** K4 demuestra que el almacén sobrevive a que **el escritor** desaparezca en cualquier punto: la supresión la hace el driver del invitado y QEMU ve solo las escrituras emitidas. Qué habría sobrevivido a que se fuera la luz, y qué hace el flush de una controladora real con su caché de escritura, no se ha medido y por tanto no se declara.
- **Un almacén grande, o de trabajo.** Dos arenas de 256 bloques, 40 entradas de objeto, 24 ranuras de preparación, cinco workspaces. Los límites se ejercen y se rechazan con estados propios; nada de esto se parece a un almacén real en tamaño.
- **Thalyx sobre este kernel, rendimiento y prueba formal general.** Nada de eso está implementado o medido.

El plano de diagnóstico de K1 sigue presente, sigue sin ser el plano de recibos, y sus dos entradas de andamiaje permanecen para que la regresión de K1 se siga ejecutando. El primer supervisor no tiene supervisor: su fallo termina la ejecución. No se ha retirado ni reemplazado Linux.

## K5 — en curso

El port de Thalyx. Lo que sigue describe lo que ya se ejecuta; lo que todavía no existe está en [qué no existe](#qué-no-existe-todavía) y, con detalle, en [la evidencia](../evidence/k5-thalyx-port.md).

### El target nativo y su runtime

Hay un segundo target de usuario, `x86_64-thalyx`, definido en [`tools/build_native.py`](../../tools/build_native.py) y no como un triple: ELF64 estático a 0x400000 con tres segmentos separados R-X / R-- / RW-, SSE y SSE2 en hardware con `-mfpmath=sse` y **sin AVX** —el kernel guarda el área `FXSAVE` heredada de forma ansiosa y no habilita `XSAVE`—, sin protector de pila, sin tablas de desenrollado y con `-nostdinc`, de modo que la única libc en la ruta de búsqueda es `user/native`.

`user/native` es lo que un programa C pisa: asignación sobre objetos de memoria que el programa crea contra su propio ámbito y mapea en su propio dominio, hilos que el supervisor construye antes de activar y que bloquean en una señal real, tiempo desde el único reloj que hay, transporte acotado con el origen que estampa el kernel, tiempo civil en UTC porque no hay zona horaria que fingir, y una biblioteca matemática escrita aquí que **no** redondea correctamente y lo dice donde importa.

### Tres etapas ejecutadas

**`smoke`** establece el target: el kernel construye un dominio desde una imagen C, la activa y la planifica; el programa mide `.bss` a cero, lee límites y reloj, ejecuta un bucle de coma flotante sembrado por el plan que el anfitrión puso en la imagen y reporta un resultado que el anfitrión recalcula y exige idéntico, hace crecer un heap de 192 páginas en tres arenas leyendo de vuelta lo que escribió por los mapeos nuevos, y se detiene contra el techo de páginas de su ámbito con `LIMIT_EXHAUSTED` del kernel.

**`surface`** ejecuta la semántica de Thalyx sobre el estado administrado de K4 —el mismo servicio, el mismo driver, el mismo medio, sin cambios—: versión identificada, `contexto` respondido desde esa versión y declarando su propia cobertura, workspace privado por `FORK`, cambio real, publicación condicionada con CAS y evidencia releída por el mismo camino que usaría cualquiera. Todo pasa por **una sola superficie de verbos**. La validación de esta etapa es una afirmación que el trabajo hace sobre sí mismo, y eso es lo que su etiqueta de integración parcial significa.

**`work`** sustituye las dos cosas que faltaban. El programa lo ejecuta **QuickJS de verdad** —quickjs-ng 0.15.1, el que empaqueta `rquickjs 0.12`, que es el que ejecuta la revisión de Thalyx de referencia—, traído fijado y comprobado por digest en vez de vendorizado, porque [OQ-14](open-questions.md) sigue abierta. Y la validación la decide una **herramienta nativa real**: `user/ncheck`, lanzada en un dominio propio con un ámbito propio, a la que se le da exactamente un objeto de memoria **sellado** con el candidato, que compila cada nombre JavaScript con el parser de QuickJS y ejecuta las aserciones del propio candidato. Su código de salida decide la publicación. Lanzar es un servicio que el trabajo pide, no un poder que tenga: un dominio de trabajo no posee ninguna capacidad que construya un dominio.

### Puerta K5

`tools/check_k5.py` decide **27** criterios por separado, cuatro de ellos las regresiones K1–K4. El decisivo no lo narra el invitado: el medio lleva una versión publicada cuyo módulo lleva la semilla que el anfitrión eligió y escribió en la imagen, decodificada por el módulo que genera el esquema de K4. Dos más se apoyan en lo mismo: la herramienta leyó exactamente los bytes que el medio dice que la versión enlaza, y el registro de validación durable nombra la identidad de herramienta que realmente corrió. `--self-test` daña la evidencia de **38** formas y un criterio nombrado nota cada una.

Los protocolos entre los dos lenguajes salen de `abi/schema/k5-proto-v1.json`: `tools/gen_k5_proto.py` deriva el módulo Rust y la cabecera C con aserciones estáticas sobre cada desplazamiento, y `tools/check_k5_proto.py` regenera y compara.

### Lo que la ejecución corrigió

Nueve defectos que compilar no encuentra, descritos uno a uno en [la evidencia](../evidence/k5-thalyx-port.md). Dos de la etapa `smoke` —una imagen sin script de enlace y un objeto de memoria sin `MEMORY_MAP` en sus derechos máximos—. Cinco de la etapa `work`: un candidato sin `MEMORY_SEAL` en sus derechos máximos, un ámbito sin `SCOPE_CREATE` para fabricarlo, y tres de la misma familia —`JS_ParseJSON` y `JS_Eval` exigen terminador nulo y la región compartida no lo tiene, y un cuerpo de función compilado como script—. Y dos que no son de este código: un supervisor que no drenaba el plano de control hasta llenarlo, y el servicio de estado de K4 contestando `NOT_FOUND` cuando lo que le pasaba era que no podía leer el medio. Lo segundo era un defecto de K4 y está corregido.

## Reanudar aquí

**Último hito terminado y pusheado:** **K5 etapas `smoke`, `surface` y `work`**, en la rama `feat/k5-thalyx-port-tools`. K4 quedó completo en `feat/k4-durable-managed-state`.

**Qué está verde, medido en este árbol y con el mismo binario de kernel en las cinco fases:** puerta K1 13/13, puerta K2 21/21 con autocomprobación 28/28, puerta K3 28/28 con autocomprobación 57/57, puerta K4 31/31 con autocomprobación 48/48, formato K4 12/12, puerta K5 27/27 con autocomprobación 38/38, protocolos K5 PASS, ABI 4/4, vault 45 notas PASS, modelos PASS, `fmt` limpio.

**Una advertencia sobre cómo medir:** el criterio de K3 «el sello se publicó contra un escritor vivo» depende de una carrera real —el escritor tiene que fallar *después* del sello— y falla cuando el anfitrión está cargado con otra ejecución en paralelo. Ocho de ocho ejecuciones seriadas pasan; tres fallos observados ocurrieron todos con la matriz de K4 corriendo al lado. Las puertas se ejecutan en serie.

**Qué demostró exactamente K5 hasta aquí:** un programa C compilado por una toolchain cruzada del anfitrión para un target propio, cargado por el kernel, activado, planificado, con coma flotante en hardware que el anfitrión recalcula y exige idéntica, y un heap que crece por objetos de memoria hasta que el ámbito lo rechaza; la vertical semántica de Thalyx entera sobre el estado administrado de K4; QuickJS real ejecutando un programa que sale de la versión publicada, alcanzando el workspace solo por llamadas mediadas; y una herramienta nativa real que compila el candidato sellado en un dominio propio, ejecuta sus aserciones y decide la publicación, con lo que costó leído de la contabilidad del kernel.

**Qué no demostró todavía:** no hay motor de inferencia —`thalyx.model` responde `no_engine`, que es un hecho sobre la tabla de capacidades de ese rol—; no hay dos trabajos rivales, ni cancelación durante un servicio residente, ni un corte en publicación dentro de esta vertical; no hay comprobación de tipos ni `cargo` dentro del kernel. Los límites de K3 y K4 se heredan enteros.

**Siguiente paso exacto al reanudar:** la etapa `engine` de K5 —motor CPU residente real y la toolchain que la carga de referencia necesita—, y después la matriz de casos adversarios que EXP-10 pide. [La ruta](phases.md).

## Siguiente trabajo: terminar K5

Según [la ruta](phases.md), quedan las etapas `surface`, `work` y `engine`. Lo que K4 deja abierto y K5 hereda: el perfil de durabilidad declara que la supresión la hace el driver del invitado, y cualquier afirmación más fuerte necesita conocer el comportamiento de flush de un dispositivo real —el mismo tipo de dependencia que el perfil de DMA declara sobre la unidad de remapeo—.

No hay una elección técnica pendiente que deba devolver el diseño al usuario. [Las preguntas abiertas](open-questions.md) especifican qué dato falta y con qué decisión conservadora avanzar.
