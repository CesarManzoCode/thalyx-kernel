---
id: STATE-001
kind: evidence
status: observed
---
# Estado actual

**2026-09-09 · Fundación 0.1.0 · K0, K1 y K2 completos. K3–K6 pendientes.**

> **Punto de reanudación.** Este documento es el checkpoint. La sección [Reanudar aquí](#reanudar-aquí) dice exactamente dónde empieza el trabajo siguiente; no hace falta reauditar K0, K1 ni K2.

## Qué existe

Un vault de 41 notas con constitución, 13 contratos de arquitectura, glosario, reconstrucción de Thalyx, 32 fuentes primarias anotadas, ocho decisiones, 23 invariantes, alternativas, integración Linux/nativa, experimentos y ruta de implementación. Se incluyen herramientas documentales y dos modelos finitos de investigación con controles negativos y resultado versionado.

Existe además un kernel que arranca y un sistema de capacidades que se ejecuta sobre él. El workspace tiene loader UEFI, protocolo de arranque, kernel con `arch/x86_64`, interfaz V0 generada desde un esquema y siete programas de usuario, con toolchain fijada y construcción reproducible desde un solo script para las dos fases.

La imagen K1 arranca en QEMU, ejecuta dominios en ring 3, los preempta con timer, contiene sus fallos ilegales y sobrevive. [Qué se ejecutó exactamente](../evidence/k1-protected-boot.md). La imagen K2, con el mismo kernel y otro paquete, construye un supervisor con un manifiesto explícito de capacidades y deja que ese supervisor cree todo lo demás por la interfaz. [Qué se ejecutó exactamente](../evidence/k2-objects-authority-work.md).

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

## Evidencia ejecutada aquí

| Comprobación | Resultado y alcance |
|---|---|
| Reconstrucción de Thalyx | Lectura estática de código, vault, historial y pruebas existentes. Sin build ni ejecución de Thalyx. |
| MODEL-01 | 294 estados / 615 transiciones, con contraejemplos de las afirmaciones/variantes incorrectas. |
| MODEL-02 | 23 casos de caída del protocolo correcto; variantes incorrectas detectadas. |
| Caso ABA | Confirma la necesidad de generación para rechazar expectativas antiguas sobre contenido repetido. |
| Revisión arquitectónica | Hallazgos y correcciones registrados en [la auditoría](../validation/audit.md). |
| Integridad documental | PASS: 40 IDs y enlaces locales. [Comprobador](../../tools/check_vault.py). Además, 36 destinos de código/historia de Thalyx resueltos contra Git. |
| Esquema ABI | PASS en 4 comprobaciones. [Comprobador](../../tools/check_abi.py). |
| Puerta K1 | PASS en 13 criterios decididos por separado desde los registros del kernel, con controles negativos. [Detalle y límites](../evidence/k1-protected-boot.md). |
| Regresión K1 sobre el sustrato K2 | PASS en los mismos 13 criterios con los mecanismos K2 compilados dentro del kernel. |
| Puerta K2 | PASS en 21 criterios decididos por separado, la mayoría desde los registros del kernel. [Detalle y límites](../evidence/k2-objects-authority-work.md). |
| Cobertura de la interfaz | 51 de 51 operaciones alcanzadas por el despacho, ninguna nombrada como no tocada. Es criterio de la puerta. |
| Autocomprobación de la puerta K2 | 28 ejecuciones dañadas de una forma cada una, las 28 detectadas por el criterio que les corresponde. |

Todas ejecutadas en el estado actual del árbol, con los mismos binarios: `fmt` limpio, ninguna advertencia de compilación, ABI 4/4, vault 41 notas PASS, modelos PASS, puerta K1 13/13, puerta K2 21/21, cobertura 51/51, autocomprobación 28/28, y cero resultados inesperados en la ejecución K2 —ni una nota de resultado no esperado, ni una operación que debiera haber sido rechazada y no lo fuera, ni un fallo de usuario—. La imagen K2 y el kernel se reconstruyen byte a byte borrando `build/cargo`.

Clippy deja **18 advertencias de estilo preexistentes**, el mismo número en `HEAD` que con este trabajo aplicado: están en `arch/x86_64`, `diag.rs`, `mm/`, `scope.rs` e `ipcops.rs`. Las dos últimas son archivos K2, de modo que la afirmación anterior de «ninguna advertencia de clippy en ningún archivo K2» era falsa y queda corregida aquí. Ninguna la introdujo este trabajo y ninguna está arreglada.

## Cobertura real de la interfaz

El kernel cuenta cuáles de las 51 operaciones asignadas alcanza el despacho y emite `k2.coverage` con el total, más un `k2.operation_untouched` por cada una que no se tocó. La última ejecución alcanza **51 de 51**, y no emite ningún `k2.operation_untouched`. La puerta lo exige como criterio, comparando el recuento contra la lista, de modo que la cobertura no puede volver a bajar en silencio.

Las nueve que faltaban —`CAP_FENCE`, `CAP_DRAIN_STATUS`, `SCOPE_SET_LIMITS`, `DOMAIN_QUERY`, `DOMAIN_ADD_THREAD`, `DOMAIN_TERMINATE`, `SIGNAL_QUERY`, `LOG_READ` y `LOG_ACK`— están ejercidas cada una por un camino que tiene éxito, con controles negativos añadidos sobre él. Dos de ellas no se podían ejercer sin corregir el kernel primero: la puerta de resolución rechazaba el linaje cercado sobre el que `CAP_DRAIN_STATUS` existe para informar.

Tres cosas que este número **no** dice, y conviene tenerlas delante antes de usarlo:

- Cada operación está ejercida por **un** camino. Cobertura de interfaz no es cobertura de caminos, y la segunda es la que sigue abierta.
- El contador marca una operación al resolver su especificación, **antes** de comprobar derechos, así que una operación solo intentada y rechazada contaría. Aquí no pasa —las nueve tienen un camino con éxito— pero lo que lo garantiza son las comprobaciones de los programas sobre el resultado, no el contador.
- El hilo que `DOMAIN_ADD_THREAD` añade al dominio de repuesto nunca se ejecuta: el dominio se termina antes de activarse. La creación del hilo está ejercida completa; su ejecución no.

## Qué no existe todavía

Lo que K2 demuestra está acotado por lo que una vertical puede demostrar. Las 51 operaciones están ejercidas, y cada una lo está por un camino: los mecanismos tienen más caminos de los que una ejecución recorre. Que la cobertura de interfaz sea completa hace esa distinción más visible, no menos. EXP-02, EXP-03, EXP-04 y EXP-06 quedan ejecutados **en su alcance K2** —uniprocesador, sin dispositivos, sin estado durable— y sus partes de K3 y K4 siguen pendientes.

No existe: SMP, drivers propios, DMA, servicio de estado implementado, Thalyx sobre este kernel, pruebas de hardware físico, mediciones de rendimiento o prueba formal general. El plano de diagnóstico de K1 sigue presente, sigue sin ser el plano de recibos, y sus dos entradas de andamiaje permanecen para que la regresión de K1 se siga ejecutando. El primer supervisor no tiene supervisor: su fallo termina la ejecución. No se ha retirado ni reemplazado Linux.

## Reanudar aquí

**Último hito terminado y pusheado:** K2 está **completo y cerrado**. Rama `feat/k2-objects-authority-work`, árbol limpio, HEAD local igual al remoto. No queda trabajo K2 a medias, ni en el árbol ni pendiente de decidir.

**Qué está verde, medido en este árbol:** puerta K2 21/21, cobertura 51/51 sin ninguna operación sin tocar, autocomprobación 28/28, puerta K1 13/13 contra el mismo binario de kernel, ABI 4/4, vault 41 notas PASS, modelos PASS, `fmt` limpio, cero resultados inesperados en la ejecución, y la imagen K2 reconstruida byte a byte borrando `build/cargo`.

**Siguiente paso exacto al reanudar:** empezar K3. No hay nada que terminar antes. El paquete está descrito abajo y en [la ruta](phases.md); el primer movimiento es arrancar los procesadores de aplicación, porque todo lo demás de K3 depende de que exista más de un núcleo al que referirse.

**Bugs o bloqueos conocidos:** ninguno abierto. Cuatro cosas que conviene recordar porque ya mordieron, y que K3 va a volver a tocar:

* **Un control negativo debe fallar por el motivo que dice.** La comprobación de derechos ocurre antes que la del cuerpo y antes que la del linaje, así que un control tiene que hacerse con un handle que **sí** lleve el derecho, o solo se observa la falta de derecho. Lo mismo al revés: un rechazo que llega con el estado equivocado invita a arreglar lo que no está roto, y eso ya costó dos correcciones.
* **Las mutaciones del `--self-test` deben ser por patrón, no por literal.** Dos dejaron de dañar nada cuando cambiaron los contadores de la ejecución y pasaron en silencio.
* **Una operación que informa sobre un estado muerto no puede estar detrás de la puerta que ese estado cierra.** Fue el defecto que hizo inalcanzable `CAP_DRAIN_STATUS`. K3 añade barreras entre núcleos, donde la misma forma de error es fácil de repetir.
* **La cobertura se cuenta en el despacho, antes de los derechos.** Sirve para saber qué no se ha tocado; no sirve como prueba de que algo funciona. Lo que prueba eso son las comprobaciones de los programas sobre el resultado.

**Cómo repetir el estado actual:**

```sh
export PATH="$HOME/.cargo/bin:$PATH"                     # rustfmt y cargo de la toolchain fijada
export THALYX_TOOL_PREFIX=~/.cache/thalyx-tools/prefix   # si QEMU/OVMF/mtools no están en el sistema
python3 tools/build_image.py --phase k1 && python3 tools/run_k1.py && python3 tools/check_k1.py --json build/k1-gate.json
python3 tools/build_image.py --phase k2 && python3 tools/run_k2.py && python3 tools/check_k2.py
python3 tools/check_k2.py --self-test
python3 tools/check_abi.py && python3 tools/check_vault.py && python3 research/models/check_models.py
cargo fmt --all --check
```

El orden importa: la puerta K2 lee el veredicto K1 de `build/k1-gate.json`, así que K1 va primero o el criterio de regresión no tiene qué leer.

## Siguiente trabajo: K3

Ejecutar el paquete K3 descrito en [la ruta](phases.md): arranque de procesadores de aplicación, planificación con presupuesto agregado entre núcleos, sincronización, invalidación de TLB entre núcleos y reclamación diferida; después el primer driver propio con IRQ y buffers separados, y el perfil de aislamiento que declare honestamente sus dependencias de IOMMU.

Lo primero que K3 debe romper es la suposición que K2 tiene derecho a hacer y K3 no: que hay un solo núcleo y que, por tanto, un cerrojo de máquina y la ausencia de DMA bastan para que una barrera signifique algo.

No hay una elección técnica pendiente que deba devolver el diseño al usuario. [Las preguntas abiertas](open-questions.md) especifican qué dato falta y con qué decisión conservadora avanzar.
