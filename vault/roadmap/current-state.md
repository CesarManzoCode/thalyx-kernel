---
id: STATE-001
kind: evidence
status: observed
---
# Estado actual

**2026-09-09 · Fundación 0.1.0 · K0, K1, K2 y K3 completos. K4–K6 pendientes.**

> **Punto de reanudación.** Este documento es el checkpoint. La sección [Reanudar aquí](#reanudar-aquí) dice exactamente dónde empieza el trabajo siguiente; no hace falta reauditar K0, K1, K2 ni K3.

## Qué existe

Un vault de 43 notas con constitución, 13 contratos de arquitectura, glosario, reconstrucción de Thalyx, 32 fuentes primarias anotadas, nueve decisiones, 23 invariantes, alternativas, integración Linux/nativa, experimentos y ruta de implementación. Se incluyen herramientas documentales y dos modelos finitos de investigación con controles negativos y resultado versionado.

Existe además un kernel que arranca, un sistema de capacidades que se ejecuta sobre él, y una ejecución en cuatro procesadores con un dispositivo de bloque real conducido desde usuario. El workspace tiene loader UEFI, protocolo de arranque, kernel con `arch/x86_64`, interfaz V0 generada desde un esquema y diez programas de usuario, con toolchain fijada y construcción reproducible desde un solo script para las tres fases.

La imagen K1 arranca en QEMU, ejecuta dominios en ring 3, los preempta con timer, contiene sus fallos ilegales y sobrevive. [Qué se ejecutó exactamente](../evidence/k1-protected-boot.md). La imagen K2, con el mismo kernel y otro paquete, construye un supervisor con un manifiesto explícito de capacidades y deja que ese supervisor cree todo lo demás por la interfaz. [Qué se ejecutó exactamente](../evidence/k2-objects-authority-work.md). La imagen K3, otra vez con el mismo kernel, arranca cuatro procesadores, reparte trabajo entre ellos con presupuesto agregado, retira traducciones con acuse de todos, y le entrega a un dominio de usuario una función virtio-blk moderna con la que lee, escribe y hace flush. [Qué se ejecutó exactamente](../evidence/k3-smp-devices.md).

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

Todas ejecutadas en el estado actual del árbol, con **el mismo binario de kernel** en las tres fases: `fmt` limpio, ninguna advertencia de compilación, ABI 4/4, vault 43 notas PASS, modelos PASS, puerta K1 13/13, puerta K2 21/21 con autocomprobación 28/28, puerta K3 28/28 con autocomprobación 57/57 y en los dos perfiles de plataforma, y cero resultados inesperados en las ejecuciones K2 y K3 —ni una nota de resultado no esperado, ni una operación que debiera haber sido rechazada y no lo fuera—. Los dos fallos de usuario de K3 son los dos que la ejecución provoca a propósito. Las imágenes y el kernel se reconstruyen byte a byte desde un árbol limpio.

Clippy deja **27 advertencias de estilo**, todas de tres familias —implementar a mano `is_multiple_of`, `Range::contains` y la división comprobada, e indexar un array con la variable de un bucle—. Diecisiete venían de antes de K3; las diez restantes están en archivos que K3 escribió o reescribió (`tlb.rs`, `acpi.rs`, `api/devops.rs`, `arch/x86_64/lapic.rs`, `pci.rs`, `device.rs`). Ninguna está arreglada, y ninguna fase ha afirmado que sus archivos estén limpios de clippy.

## Cobertura real de la interfaz

El kernel cuenta cuáles de las **59** operaciones asignadas alcanza el despacho y emite `k2.coverage` con el total, más un `k2.operation_untouched` por cada una que no se tocó. Cada puerta exige lo que su paquete debe recorrer, comparando el recuento contra la lista, de modo que la cobertura no puede bajar en silencio.

El paquete K2 alcanza **51 de 51** de las operaciones que existían cuando se escribió, y la puerta K2 lo exige nombrando explícitamente las ocho de dispositivo como las únicas que puede no tocar: cualquier otra sin tocar la hace fallar.

El paquete K3 alcanza **37 de 59**, y la puerta K3 exige las **26** sobre las que descansan sus afirmaciones —las ocho de dispositivo enteras, más mapear, desmapear, añadir hilo, activar, sellar, leer, escribir, cercar, drenar, retirar, consultar, llamar, recibir, responder, esperar y levantar señales, armar timer y cerrar capacidad—. Las 22 restantes pertenecen a caminos que este paquete no recorre y están nombradas una a una en el propio registro. K3 no es K2 con más procesadores y no pretende volver a recorrer la interfaz entera.

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
- **Estado durable.** No hay servicio de estado, ni versiones publicadas, ni recuperación tras caída. Es K4.
- **Thalyx sobre este kernel, rendimiento y prueba formal general.** Nada de eso está implementado o medido.

El plano de diagnóstico de K1 sigue presente, sigue sin ser el plano de recibos, y sus dos entradas de andamiaje permanecen para que la regresión de K1 se siga ejecutando. El primer supervisor no tiene supervisor: su fallo termina la ejecución. No se ha retirado ni reemplazado Linux.

## Reanudar aquí

**Último hito terminado y pusheado:** **K3 completo**. Rama `feat/k3-smp-devices`, sin fusionar.

**Qué está verde, medido en este árbol y con el mismo binario de kernel en las tres fases:** puerta K1 13/13, puerta K2 21/21, autocomprobación K2 28/28, puerta K3 28/28 en los dos perfiles de plataforma, autocomprobación K3 57/57, cobertura K2 51/51 y K3 37/59 con las 26 requeridas dentro, ABI 4/4, vault 43 notas PASS, modelos PASS, `fmt` limpio.

**Qué demostró exactamente la ejecución K3:** cuatro procesadores en línea de cuatro descritos con x2APIC, un procesador ausente que no contesta y no deja recursos reutilizados, cuatro hilos despachados en el mismo instante, 346 migraciones con estado FP, un solo reloj sin regresiones, admisión que nunca prometió más que el presupuesto con 45 220 rechazos por saldo, deuda arrastrada y nunca perdonada, una traducción retirada de un espacio vivo con acuse de los cuatro y el fallo de lectura posterior, un sello publicado contra un escritor vivo con el fallo de escritura posterior y los mismos bytes leídos dos veces, cuarentena de marcos condicionada a que todos hayan invalidado, y el camino de dispositivo entero: enumeración PCI, cuatro ventanas mapeadas sin caché fuera de la página de la tabla MSI-X, interrupciones escritas por el kernel y entregadas a la señal enlazada, lectura, escritura y flush de bloques reales, validador de anillo probado contra daño que el driver fabrica, perfil débil declarado como tal, perfil fuerte rechazado, y el dispositivo parado bajo su driver con el transporte releído y las tres operaciones que el driver recordaba rechazadas por sesión obsoleta.

**Qué no demostró, y está escrito así en la evidencia:** aislamiento de DMA, hardware físico, escalabilidad, más de un dispositivo, rutas de interrupción legadas y cobertura de caminos. [Detalle y límites](../evidence/k3-smp-devices.md).

**Siguiente paso exacto al reanudar:** K4, estado durable, según [la ruta](phases.md). Nada de K3 queda pendiente; lo que queda abierto son las preguntas que K3 no podía cerrar, y están registradas donde corresponde en lugar de en una lista de tareas.

**Bugs encontrados por ejecución en K3, todos corregidos:** seis del kernel, dos de la evidencia y dos del paquete. Los tres fatales comparten familia —suponer que el estado de un hilo basta para decir de quién es— y ninguno era visible con un procesador. Están descritos uno a uno en [la evidencia](../evidence/k3-smp-devices.md).

## Siguiente trabajo: K4

Estado durable, según [la ruta](phases.md): versiones inmutables, publicación con CAS, recuperación tras caída en cada punto de escritura, y el recibo que hace auditable lo publicado. El perfil de durabilidad tendrá que declarar sus dependencias con la misma honestidad con la que el perfil de DMA declara las suyas: sin conocer el comportamiento de flush del dispositivo no se declara durabilidad, igual que sin IOMMU programada no se declara aislamiento.

Lo primero que K4 debe romper es la suposición que K3 tiene derecho a hacer: que todo lo que importa cabe en memoria y desaparece con la ejecución.

No hay una elección técnica pendiente que deba devolver el diseño al usuario. [Las preguntas abiertas](open-questions.md) especifican qué dato falta y con qué decisión conservadora avanzar.
