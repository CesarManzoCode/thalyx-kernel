---
id: STATE-001
kind: evidence
status: observed
---
# Estado actual

**2026-09-08 · Fundación 0.1.0 · K0 y K1 completos. K2 sustancialmente completo y en pausa con puerta verde; queda ampliar la cobertura de operaciones. K3–K6 pendientes.**

> **Punto de reanudación.** Este documento es el checkpoint. La sección [Reanudar aquí](#reanudar-aquí) dice exactamente qué estaba en curso y cuál es el siguiente paso; no hace falta reauditar K0, K1 ni lo que K2 ya tiene puerta.

## Qué existe

Un vault de 40 notas con constitución, 13 contratos de arquitectura, glosario, reconstrucción de Thalyx, 32 fuentes primarias anotadas, ocho decisiones, 23 invariantes, alternativas, integración Linux/nativa, experimentos y ruta de implementación. Se incluyen herramientas documentales y dos modelos finitos de investigación con controles negativos y resultado versionado.

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

Veintisiete controles negativos se ejecutan dentro de esa vertical y los veintisiete son rechazados con el código esperado, en doce estados distintos. Ninguna operación que debía ser rechazada tuvo éxito.

La vertical cubre además lo que EXP-02 y EXP-06 piden en alcance K2: copiar una capacidad, cerrarla, ver la ranura liberada volver bajo otro handle que el antiguo no nombra, sostener una capacidad vencida, once peticiones malformadas rechazadas sin efecto parcial, y los límites de tabla, cola y log alcanzados con el cierre todavía disponible.

### Puerta K2

`tools/check_k2.py` decide veinte criterios por separado desde los registros del kernel; los que necesitan las notas de los programas lo dicen en su título. La regresión de K1 es uno de esos criterios, porque una puerta K2 que pasara con el arranque protegido roto mediría otra cosa. `--self-test` daña la ejecución de veinticinco formas distintas y comprueba que cada daño hace fallar al criterio que le corresponde.

### Lo que la vertical corrigió del sustrato

Ejecutar los mecanismos encontró diez defectos que compilar no encuentra, y los diez están corregidos: el creador de un objeto de memoria no recibía derechos comunes sobre lo que acababa de crear; un dominio no podía reclamarse receptor de un endpoint propio, de modo que ningún supervisor podía tener canal de fallos; la barrera no alcanzaba a los grants patrocinados por el ámbito cerrado, así que la autoridad delegada seguía funcionando después del cierre; cercar un ámbito con un hilo vivo lo dejaba permanentemente inejecutable y la ejecución no terminaba; el informe de drenaje contaba hilos despachados en lugar de hilos vivos, declarando quiescencia con un dominio todavía en pie; la retirada marcaba el ámbito y no liberaba nada; el despacho descartaba la respuesta de toda operación que rechazara, de modo que el informe de `SCOPE_RETIRE` nunca llegaba a quien `DRAIN_INCOMPLETE` manda mirarlo; el límite de paralelismo de un ámbito nunca se comprobaba; una vinculación de recuperación cargaba el ámbito del cliente cerrado en vez del del servicio, dejando inejecutable al hilo que debía terminar la obligación; y la interfaz tomaba plazos monotónicos sin ofrecer forma de leer el reloj. [Detalle](../evidence/k2-objects-authority-work.md).

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
| Puerta K2 | PASS en 20 criterios decididos por separado, la mayoría desde los registros del kernel. [Detalle y límites](../evidence/k2-objects-authority-work.md). |
| Autocomprobación de la puerta K2 | 25 ejecuciones dañadas de una forma cada una, las 25 detectadas por el criterio que les corresponde. |

Todas ejecutadas en el estado actual del árbol, con los mismos binarios: `fmt` limpio, ninguna advertencia de compilación ni de clippy en ningún archivo K2, ABI 4/4, vault 41 notas PASS, modelos PASS, puerta K1 13/13, puerta K2 20/20, autocomprobación 25/25, y cero resultados inesperados en la ejecución K2. La imagen K2 se reconstruye byte a byte desde un árbol limpio.

## Cobertura real de la interfaz

El kernel cuenta cuáles de las 51 operaciones asignadas alcanza el despacho y emite `k2.coverage` con el total, más un `k2.operation_untouched` por cada una que no se tocó. La última ejecución alcanza **42 de 51**.

Las nueve que faltan, con lo que haría falta para ejercerlas:

| Operación | Qué falta para ejercerla |
|---|---|
| `CAP_FENCE` | Cercar un grant concreto —la faceta del cliente— en lugar del ámbito entero. La barrera por grant es un camino distinto del de ámbito y no se ha recorrido. |
| `CAP_DRAIN_STATUS` | Leer el informe de drenaje de ese grant cercado. |
| `SCOPE_SET_LIMITS` | Estrechar los límites de un ámbito hijo ya creado, y comprobar que solo se puede dar menos. |
| `DOMAIN_ADD_THREAD` | Añadir un hilo a un dominio en construcción; el control negativo con punto de entrada no mapeado también la alcanzaría. |
| `DOMAIN_TERMINATE` | Terminar un dominio desde fuera. Lo limpio es un dominio de repuesto que se construye y se termina sin activarse nunca. |
| `DOMAIN_QUERY` | Consultar el estado, hilos y fallos de un dominio. |
| `SIGNAL_QUERY` | Consultar bits y secuencia de una señal. |
| `LOG_READ` | Leer recibos por capacidad. El contrato de observabilidad afirma que el log es alcanzable por capacidad y **nada lo ha leído todavía**; esta es la más importante de las nueve. |
| `LOG_ACK` | Reconocer recibos hasta una secuencia y ver que el log los suelta. |

Ninguna de las nueve está sin implementar: todas tienen manejador y se despachan. Lo que falta es ejecutarlas.

## Qué no existe todavía

Lo que K2 demuestra está acotado por lo que una vertical puede demostrar. Los mecanismos tienen más caminos de los que una ejecución recorre: 42 de las 51 operaciones están ejercidas, y las que lo están lo están por un camino cada una. EXP-02, EXP-03, EXP-04 y EXP-06 quedan ejecutados **en su alcance K2** —uniprocesador, sin dispositivos, sin estado durable— y sus partes de K3 y K4 siguen pendientes.

No existe: SMP, drivers propios, DMA, servicio de estado implementado, Thalyx sobre este kernel, pruebas de hardware físico, mediciones de rendimiento o prueba formal general. El plano de diagnóstico de K1 sigue presente, sigue sin ser el plano de recibos, y sus dos entradas de andamiaje permanecen para que la regresión de K1 se siga ejecutando. El primer supervisor no tiene supervisor: su fallo termina la ejecución. No se ha retirado ni reemplazado Linux.

## Reanudar aquí

**Último hito terminado y pusheado:** `feat(k2): measure interface coverage, and answer an ordinary request`. Rama `feat/k2-objects-authority-work`, árbol limpio, HEAD local igual al remoto.

**Qué se estaba implementando exactamente al pausar:** ampliar la vertical para alcanzar las nueve operaciones que la tabla de arriba enumera. Se llegó a añadir `DOMAIN_UNMAP` dentro de la publicación de la página sellada y la petición/respuesta ordinaria; lo demás no se empezó. Se retiró una edición a medias en `user/k2super/src/main.rs` que llamaba a tres funciones aún inexistentes, de modo que **no queda código a medias en el árbol**.

**Siguiente paso exacto al reanudar:** en `user/k2super/src/main.rs`, añadir tres funciones y llamarlas desde `run` justo después de `budget(own_scope, server_scope);`:

1. `receipts(log)` — `k2::log_read` sobre el log de control, comprobar que devuelve recibos y que sus orígenes son los que el kernel estampó; después `k2::log_acknowledge` hasta una secuencia y un segundo `log_read` que devuelva menos. Alcanza `LOG_READ` y `LOG_ACK`.
2. `grant_barrier(facet)` — derivar de la faceta, `k2::cap_fence` sobre esa derivación y `k2::cap_drain_status` sobre ella, comprobando que la faceta original sigue viva. Alcanza `CAP_FENCE` y `CAP_DRAIN_STATUS`, y demuestra que la barrera por grant no es la barrera por ámbito.
3. `spare_domain(own_scope, image)` — crear un dominio de repuesto desde la imagen del cliente en el ámbito `system`, `k2::domain_query` sobre él, `k2::domain_add_thread` con un punto de entrada no mapeado como control negativo (`INVALID_ARGUMENT`), y `k2::domain_terminate`. Nunca se activa. Alcanza `DOMAIN_QUERY`, `DOMAIN_ADD_THREAD` y `DOMAIN_TERMINATE`.

Falta también `SCOPE_SET_LIMITS`: estrechar los límites del ámbito `srv` antes de crear el servidor, y comprobar que pedir más de lo que el padre tiene se rechaza.

Los bindings de runtime para las nueve ya existen en `user/rt/src/k2.rs` (`log_read`, `log_acknowledge`, `cap_fence`, `cap_drain_status`, `domain_query`, `signal_query`, `scope_set_limits`, `domain_add_thread`, `domain_terminate`). No hay que escribirlos.

Después: subir el criterio de cobertura a la puerta —exigir que `k2.coverage` alcance las 51— con su mutación en `--self-test`, y actualizar la tabla de arriba, la evidencia y el CHANGELOG.

**Bugs o bloqueos conocidos:** ninguno abierto. Dos cosas que conviene recordar porque ya mordieron una vez:

* Un control negativo debe fallar por el motivo que dice. Varios rechazos llegaron con el estado equivocado porque la comprobación de derechos ocurre antes que la del cuerpo: si se prueba un descriptor malformado hay que hacerlo con una operación sobre la que el llamante **sí** tiene derecho, o solo se observa la falta de derecho.
* Las mutaciones del `--self-test` deben ser por patrón, no por literal. Dos dejaron de dañar nada cuando cambiaron los contadores de la ejecución y pasaron en silencio.

**Cómo repetir el estado actual:**

```sh
export THALYX_TOOL_PREFIX=~/.cache/thalyx-tools/prefix   # si QEMU/OVMF/mtools no están en el sistema
python3 tools/build_image.py --phase k1 && python3 tools/run_k1.py && python3 tools/check_k1.py --json build/k1-gate.json
python3 tools/build_image.py --phase k2 && python3 tools/run_k2.py && python3 tools/check_k2.py
python3 tools/check_k2.py --self-test
python3 tools/check_abi.py && python3 tools/check_vault.py && python3 research/models/check_models.py
```

## Siguiente trabajo (después de cerrar K2)

Ejecutar el paquete K3 descrito en [la ruta](phases.md): arranque de procesadores de aplicación, planificación con presupuesto agregado entre núcleos, sincronización, invalidación de TLB entre núcleos y reclamación diferida; después el primer driver propio con IRQ y buffers separados, y el perfil de aislamiento que declare honestamente sus dependencias de IOMMU.

Lo primero que K3 debe romper es la suposición que K2 tiene derecho a hacer y K3 no: que hay un solo núcleo y que, por tanto, un cerrojo de máquina y la ausencia de DMA bastan para que una barrera signifique algo.

No hay una elección técnica pendiente que deba devolver el diseño al usuario. [Las preguntas abiertas](open-questions.md) especifican qué dato falta y con qué decisión conservadora avanzar.
