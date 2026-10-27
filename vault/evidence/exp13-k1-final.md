---
id: EVD-011
kind: evidence
status: observed
---
# EXP-13 — K1: el tercer backend, y la campaña causal

Esta nota es el lado de Thalyx-Kernel de EXP-13, la comparación final entre tres
brazos que ejecutan **la misma revisión real de Thalyx** sobre la misma vertical
semántica:

- **L0**, `linux-current`: Thalyx sobre Btrfs, la línea base.
- **L1**, `linux-managed`: la misma transacción sobre el modelo administrado de
  Thalyx-Kernel, en Linux, sobre `thalyx-managed` en loopback.
- **K1**, `thalyx-kernel-managed`: la misma transacción sobre **este kernel** —el
  servicio de estado de K4 sobre el driver de bloque de K4, sobre un medio real,
  con la vida del trabajo en un ámbito del kernel y el transporte en una función
  virtio-console—, conducida por el Thalyx real que corre en el host y habla su
  protocolo administrado por un puerto de consola.

La revisión de Thalyx es la de la rama `feat/exp13-k1-final` sobre `76386f5`; la
de este kernel es `feat/exp13-k1-final` sobre `fde5bee`. Las puertas de abajo se
observaron con este kernel en `c0fe9e1` y Thalyx en `c8b9c27`, y se repitieron con
el kernel en `c5d2ece` sobre las dos imágenes de K1 y en `60ef827` sobre la de
medida. La campaña corrió con este kernel en `50b3449`, la imagen de medida
`fb45ad1c…` de `60ef827`, y Thalyx en `9d3ccce`.

## Qué es K1, y qué corre dónde

La disciplina de K5 se hereda entera: **construir no es ejecutar**, y una
afirmación de ejecución nativa sólo vale si la decide un registro del kernel. Por
eso lo primero que dice esta nota es qué corre dónde, sin redondear.

Del kernel (nativo, K1 lo reclama):

- **El estado versionado.** El servicio de estado de K4 (`user/k4store`), sin
  cambios de mecanismo, sobre el driver de bloque de K4 (`user/k4disk`), sobre el
  mismo medio de bloques. K1 no hace crecer un segundo mecanismo de persistencia:
  usa el que existe, que era la mitad interesante del problema.
- **La vida del trabajo.** Un ámbito del kernel por transacción de Thalyx, creado
  por el dominio *link* bajo un ámbito padre con `SCOPE_CREATE`. El grant por el
  que viaja una publicación se deriva bajo la vida de ese ámbito; cercarlo hace
  que el kernel rechace ese grant en su siguiente uso. `WorkControl` es esto.
- **El transporte.** Una función virtio-console que el kernel asignó y que el
  link conduce desde usuario (`user/k5link/src/virtio.rs`), el mismo transporte
  virtio moderno que K3 levantó para bloques. Los mensajes administrados viajan
  enmarcados por longitud, la gramática de `thalyx-bridge`.

Del host (declarado, K1 **no** lo reclama como nativo):

- **El coordinador semántico.** `exec.rs` de Thalyx —qué hace un paso rechazado,
  qué decide un commit, qué autoriza un rollback— corre en el host, idéntico a
  L1. La frontera de plataforma quedó bien cortada en el Sprint 1: el único
  cambio en `exec.rs` para K1 es una rama de despacho.
- **El lanzamiento y la validación.** `Check::Rust` compila con `cargo` y QuickJS
  ejecuta el programa, sobre el host, por `run_foreign`, igual que en L1. K5
  demostró una herramienta nativa sobre un candidato sellado, pero **no**
  `Check::Rust`; K1 no finge lo contrario. El perfil lo dice: `launch =
  linux_confined_process_host_side`.
- **El reloj.** El de la transacción, que corre en el host. Los registros del
  kernel llevan su propio reloj para lo que el kernel cobra.

El cliente administrado es **el mismo** `thalyx_platform::managed::client::Managed`
que usa L1, sin un byte de cambio; lo único distinto es el `Transport` debajo. Que
el cliente no distinga este almacén del de loopback es toda la afirmación de K1.

## Cómo aterriza el modelo de Thalyx sobre el de K4

Thalyx tiene *líneas*: secuencias de generaciones, cada una una identidad de
contenido `c1-…` de un árbol, más objetos sin publicar y evidencia que sobrevive
al abandono. K4 publica una *raíz*: un manifiesto sobre un árbol de a lo más doce
nombres, cuya clausura una publicación hace durable. El link guarda el estado de
cada línea dentro de una sola raíz de K4:

```text
raíz ── O   todos los objetos de Thalyx, como un árbol 12-ario de objetos de K4
        │   (un objeto mayor que el techo de K4 es un árbol de trozos de K4)
        ├── X   el índice: digest y clase de Thalyx → digest y forma de K4
        └── L<línea>   por línea: H sus generaciones, E su evidencia, R su recibo
```

Una publicación de Thalyx es una publicación de K4 que mueve la `H` de una línea,
con CAS sobre la generación de la raíz **y** decidida contra la generación de la
línea; guardar evidencia mueve su `E`. Dos publicaciones no se entrelazan porque
el link sirve una petición a la vez y K4 admite un `PREPARE` a la vez, y un corte
entre `PREPARE` y `COMMIT` lo resuelve la recuperación de K4 exactamente como en
su matriz. El detalle está en `user/k5link/src/managed.rs`.

Tres precisiones que fijó la primera ejecución (2026-10-27):

- **Una línea es su número.** El nombre `L…` de cada línea en la raíz codifica
  el índice de la línea —el puerto por el que llegan sus peticiones, o la línea a
  la que el arnés ató ese puerto— y la recuperación la devuelve a ese índice. Es
  lo único que la nombra, así que es lo que sobrevive al reinicio.
- **Lo que aún no es durable, el link lo guarda.** `READ` de K4 sólo sirve lo
  alcanzable desde una raíz publicada o retenida: `NOT_FOUND` ahí significa «no es
  parte de nada que este almacén sirva», y ese contrato de K4 no se tocó. Thalyx
  lee objetos que puso y todavía no publicó —el árbol de una siembra se comprueba
  entero, un `get` puede nombrar cualquier cosa que puso—, así que el link guarda
  sus bytes en un área acotada de 1 MiB hasta que una publicación de raíz los hace
  durables, y los sirve desde ahí. No añade ninguna escritura al medio: las únicas
  siguen siendo las publicaciones de raíz, que el grant de un trabajo cercado no
  puede hacer. Un `put` que no cabría se rechaza con una palabra, antes de tocar
  K4.
- **Los nombres de un árbol van ordenados.** K4 rechaza un árbol no canónico; el
  de una línea se arma `H`, `E`, `R` y se ordena antes de escribirlo, como ya se
  ordenaba la raíz.

## La garantía más fuerte que K1 declara sobre L1

El perfil de `linux-managed` declara `exclusive_store_writer:
detected_by_digest_not_prevented` y `holds = false`: nada en Linux impide que
otro proceso del usuario escriba `objects/`; el almacén recalcula el digest y
rechaza lo que no coincide, pero eso es *detectar*. El perfil de K1 declara
`exclusive_store_writer: prevented_kernel_owns_the_medium` y `holds = true`: no
hay `objects/` en un sistema de archivos, hay un medio del que sólo el servicio de
estado —el único dominio al que el kernel dio el dispositivo— puede escribir.
Prevenirlo es para lo que sirve un kernel dueño del medio. Es la única diferencia
de garantía que K1 declara, y la única razón por la que un veredicto de K1 puede
ser mejor que el de L1 sin ser el mismo.

## Las puertas semánticas y adversarias

Corren desde el host, contra el kernel arrancado, y son parte de la suite
`crates/thalyx-cli/tests/exp13_equivalence.rs` de Thalyx:

| Puerta | Qué establece | Cómo |
|---|---|---|
| Corpus de equivalencia | K1 significa lo mismo que L1, salvo lo que un caso declare, sobre los 16 casos deterministas | `thalyx_kernel_managed_means_what_linux_managed_means`: un guest fresco por caso, el agente con guion sobre un puerto, comparación semántica contra L1 corrido al lado |
| Publicación / fallo de validación / abandono | Éxito, un chequeo que falla y pone el árbol atrás, y un fallo guardado a petición | Casos del corpus, ejecutados en K1 como en todo backend |
| Publicación obsoleta | Dos principales sobre una generación; el segundo `PUBLISH` es rechazado como obsoleto | `thalyx_kernel_refuses_a_stale_publication`: dos puertos sobre una línea (`bind`), CAS del kernel |
| Cancelación / cierre | Un trabajo cercado no publica, y lo rechaza el kernel | `thalyx_kernel_fences_a_work_so_it_cannot_publish`: `fence` por el puerto de control, el grant derivado bajo la vida del ámbito se rechaza; `admit` lo confirma |
| Recuperación durable | Una versión publicada sobrevive a que la máquina se apague y arranque de nuevo | `thalyx_kernel_recovers_a_published_version_after_a_reboot`: mismo medio, dos arranques |

Cada puerta es `NOT PROVEN` —no un fallo— cuando el entorno no nombra imagen y
lanzador del kernel, igual que el brazo Btrfs es `NOT PROVEN` sin subvolumen.
`THALYX_REQUIRE_K1=1` las convierte en fallo.

### Resultado — 2026-10-27

**Las cuatro puertas: PROVEN**, bajo KVM, con `THALYX_REQUIRE_K1=1`, serializadas,
con el kernel en `c0fe9e1`, Thalyx en `c8b9c27` y la imagen `build/thalyx-k5.img`
(etapa `thalyx`, sha256 `679cfb81e491b8d26bbf2ff671ebc80a72425d11cbb31403d06c48ae5d99cf22`):

| Puerta | Resultado |
|---|---|
| `thalyx_kernel_managed_means_what_linux_managed_means` | PROVEN, 16 de 16 casos con el mismo significado que L1, sin ninguna diferencia declarada |
| `thalyx_kernel_refuses_a_stale_publication` | PROVEN: el kernel rechazó la publicación contra una generación que se movió |
| `thalyx_kernel_fences_a_work_so_it_cannot_publish` | PROVEN: la publicación de un trabajo cercado la rechazó el kernel (`SCOPE_CLOSED` sobre el grant derivado) y `admit` lo confirma |
| `thalyx_kernel_recovers_a_published_version_after_a_reboot` | PROVEN: generación 2 y la misma raíz después de apagar y arrancar sobre el mismo medio |

La suite entera dio 7 aprobadas, 0 fallidas y 2 ignoradas por diseño (la campaña
y la grabación de la línea base). La orden, desde `~/thalyx`:

```
THALYX_K1_IMAGE=$HOME/thalyx-kernel/build/thalyx-k5.img \
THALYX_K1_LAUNCHER=$HOME/thalyx-kernel/tools/run_k1_link.py \
THALYX_ACCEL=kvm THALYX_REQUIRE_K1=1 THALYX_BTRFS_SCRATCH=/mnt/exp13 \
cargo test -p thalyx-cli --test exp13_equivalence -- --nocapture --test-threads=1
```

**Cómo se llegó.** Antes, las cuatro fallaban con «the guest did not answer its
control port within 180 s». Ese síntoma escondía once defectos en fila, cada uno
diagnosticado con el log serie del guest o con una captura cruda del puerto antes
de cambiar nada:

1. **Pila de `k5store`** (`5558c12`). Con `large-index` el `Store` vive en el marco
   de `run` con unos 79 KiB, contra las ocho páginas (32 KiB) que el kernel mapea:
   el sondeo de pila tocó la página de guarda (`vector=14 error=0x6`, `cr2 == rsp`).
   Medido en el desensamblado; la cadena estática más profunda es de unos 104 KiB.
   Ahora trae una pila de 128 KiB.
2. **Capacidad de dispositivo en otro dominio** (`ebb3311`, kernel). El bucle de
   dispositivos de `establish_supervisor` llamaba `index` a su variable y tapaba el
   índice del dominio supervisor; con un dispositivo el error no se veía (ambos
   valen 0). Con virtio-console al lado, la segunda capacidad fue a la tabla del
   dominio 1 y el supervisor no encontró la consola (paso 400).
3. **`k5link` sin guion de enlace** (`e6bce33`). Era el único programa sin
   `build.rs`: quedó en `0x200000` con segmentos a media página y el cargador ELF
   lo rechazó (`SegmentUnaligned`, paso 403).
4. **Faceta sin `DERIVE`** (`3d9faac`). El link deriva de su faceta el grant de
   cada trabajo; el supervisor la acuñaba sin `DERIVE` e instalarla con él fue
   rechazado (`INSUFFICIENT_RIGHTS`, paso 416).
5. **Pila de `k5link`** (`e6bce33`). Marco de `run` de unos 156 KiB, cadena de unos
   203 KiB: trae 256 KiB, como `k5work`.
6. **`hello` ilegible** (`c0fe9e1`). El nombre del almacén salía con comillas
   anidadas (`"k4-"…""`).
7. **Objetos sin publicar ilegibles** (`c0fe9e1`). Ver arriba: el área pendiente.
8. **El cerco nombrado como otro error** (`c0fe9e1`). La primera de las llamadas de
   una publicación de raíz que toca el grant cercado convertía `SCOPE_CLOSED` en un
   estado de K4; ahora cualquiera de ellas lo reporta como trabajo cerrado.
9. **Las líneas no llegaban al medio** (`c0fe9e1`). Ninguna línea servida se marcaba
   en uso, así que ninguna raíz llevaba su `H`, y la recuperación las reconstruía en
   ranuras que ningún puerto alcanza: tras reiniciar, generación 0.
10. **Evidencia nunca guardada** (`c0fe9e1`). El árbol `H, E, R` no era canónico.
11. **Cinco segundos por trabajo cerrado** (`c0fe9e1`). Se cerraba el handle del
    ámbito antes de cercarlo y retirarlo con él, y la espera era un `signal_wait`
    sobre ese handle, que no es una señal: unos veinte millones de `SCOPE_RETIRE`
    por transacción.

Y dos del arnés de Thalyx (`7600354`): la sesión de las puertas adversarias
escribía `view/src/lib.rs` sin crear `view/src`, y la limpieza de rutas sólo
reconocía el espacio de trabajo privado de L1 (`state/managed/…`), no el de K1
(`state/kernel-managed/…`).

### Las dos imágenes de K1 — 2026-10-27, kernel en `c5d2ece`

Las puertas de arriba corrieron con trazas, y una medida con trazas mide las
trazas (abajo, el piloto). Desde `c5d2ece` hay dos imágenes de la etapa `thalyx`,
del mismo árbol y con el mismo link, que difieren sólo en la bandera del módulo
supervisor:

| Imagen | Para qué | Orden | sha256 |
|---|---|---|---|
| `build/thalyx-k1-trace.img` | evidencia de las puertas: trazas y resúmenes | `build_image.py --phase k5 --stage thalyx --name thalyx-k1-trace` | `dae2d7894cd14928846fe1c99e9212e69d1b6d5b79180e037fe115dd84edb3a2` |
| `build/thalyx-k1-notrace.img` | medida de la campaña: sólo resúmenes, `TRACE_OFF` como K6 | `build_image.py --phase k5 --stage thalyx --trace off --name thalyx-k1-notrace` | `00a150d096eb3dc116387b9cab9020eccc0fe357fb98d9aa9f4777aa1d30f093` |

`--name` existe para que ninguna de las dos sea `build/thalyx-k5.img`, que
`run_gates.py` reconstruye con otras etapas. En el mismo cambio el link dejó de
anotar cada trama que entra y sale (`FRAME_IN`/`FRAME_OUT`): una nota es un
registro de resumen, que `TRACE_OFF` no retiene, y dos por petición dejaban el
puerto serie en el camino que la campaña cronometra. `FRAMES` sigue contándolas,
una vez, al final.

**Las cuatro puertas, sobre las dos imágenes: PROVEN**, bajo KVM, con
`THALYX_REQUIRE_K1=1`, serializadas, con Thalyx en `9d3ccce` (código en
`c8b9c27`). En las dos: equivalencia 16 de 16 sin diferencias declaradas, obsoleta,
cerco y recuperación; la suite, 7 aprobadas, 0 fallidas, 2 ignoradas por diseño.
La suite tardó 240 s con trazas y 185 s sin ellas. Logs en
`build/exp13/gates-k1-trace.log` y `build/exp13/gates-k1-notrace.log`.

**Las puertas K1–K5 del kernel, TCG, sobre `c5d2ece`: PASS** (`run_gates.py
--platform tcg`): K1 13/13, K2 21/21 con autocomprobación 28/28, K3 28/28 con
57/57, K4 31/31 con 48/48, K5 47/47 con 88/88. Eso cierra el pendiente del cambio
en `k2boot.rs` para K1–K5; K6 no se volvió a correr. Log en
`build/exp13/run-gates-tcg.log`.

### La imagen de medida — 2026-10-27, kernel en `60ef827`

Una tercera imagen, sólo para medir: `build/thalyx-k1-measure.img`
(`fb45ad1ccb128755c99107063842992905185fd984b91da30077281d09d72b2c`), la de sin
trazas más `--k4-io-notes off`, que construye `k4disk` y `k4store` con la feature
`quiet-io`. Esa feature calla las notas que K4 escribe por operación de bloque y
por registro añadido —`RING_ACCEPTED`, `DISK_READ`, `DISK_WRITE`, `DISK_FLUSH`,
`RECORD_WRITTEN`—; fallos, latches y errores se siguen anotando. `build_image.py`
sólo la acepta en la etapa `thalyx`. Contra la de sin trazas difieren `k4disk` y
`k4store`, nada más; el desensamblado pierde justo las llamadas al sistema de
esas notas (cinco en `k4disk`, una en `k4store`).

**K4 no cambia.** Sin la feature, `k4disk.elf` y `k4store.elf` son byte a byte los
de `c5d2ece` fuera de la información de depuración (comparados con
`llvm-objcopy --strip-debug` contra un árbol de `c5d2ece`). La primera versión del
cambio no lo era: `.text` igual, pero `.rodata` cambiaba en las líneas de las
ubicaciones de pánico, porque el ayudante movía líneas; ahora los ayudantes van al
final de su archivo. Sobre el código final, en TCG: **K4 31/31 con
autocomprobación 48/48, K5 47/47 con 88/88** (`build/exp13/run-gates-tcg-k4k5.log`).

**Las cuatro puertas de K1 sobre la imagen de medida: PROVEN**, bajo KVM, con
`THALYX_REQUIRE_K1=1`: equivalencia 16 de 16 sin diferencias declaradas, obsoleta,
cerco y recuperación; la suite en 132 s (`build/exp13/gates-k1-measure.log`).

**Qué no establece.** Las puertas de EXP-13 corrieron bajo KVM, no TCG, conducidas
desde el host. El lanzamiento y la validación siguen siendo del host, como declara
el perfil.

## La campaña causal

`exp13_campaign` (un test `#[ignore]` de la misma suite) corre tantas rondas
pareadas como se le pida, cada ronda todos los brazos que la máquina puede correr,
en orden sorteado, y escribe cada observación con su traza de plataforma.
`dev/exp13/analyze.py` toma la latencia extremo a extremo de cada transacción
(`whole_ns`), los totales por fase y el transporte que movió cada corrida, y
reporta tres diferencias con intervalo de confianza bootstrap sobre las rondas
pareadas:

- **K1 − L0**: el efecto total de la migración.
- **L1 − L0**: el efecto del rediseño administrado, todavía en Linux.
- **K1 − L1**: la contribución específica del kernel.

No se colapsan en una cifra: si K1 gana a L0 pero no a L1, el resultado es que el
rediseño ayudó y una ventaja específica del kernel no quedó demostrada.

### Resultados

**La campaña — 2026-10-27, seis rondas, los tres brazos, imagen de medida.**
KVM, kernel en `50b3449`, imagen `build/thalyx-k1-measure.img`
(`fb45ad1ccb128755c99107063842992905185fd984b91da30077281d09d72b2c`, la misma de
las puertas de arriba), Thalyx en `9d3ccce`; los tres brazos con el mismo binario
`target/debug/thalyx`, corridas entre las 16:12 y las 16:22. La orden es la del
paso `k` de `build/exp13/comandos.sh`. Los 288 archivos crudos (16 casos × 6 rondas
× 3 brazos) viven en `build/exp13/campaign-1/`, que no se versiona; el análisis
—`exp13-summary.md` y `exp13-results.json`, tal como los escribió
`dev/exp13/analyze.py`— está copiado en [`exp13/campaign-1/`](exp13/campaign-1/), y
`digests.json` a su lado fija el SHA-256 y el tamaño de la imagen, de los dos
archivos del análisis y de cada uno de los 288 crudos, como
[`k6/baseline/`](k6/baseline/) lo hace para K6.

Todas las cifras son milisegundos de latencia extremo a extremo de la transacción
(`whole_ns`, reloj del host): mediana de las diferencias pareadas por ronda, con
IC 95 % bootstrap de 10 000 remuestreos **sobre rondas pareadas** —no sobre casos,
como en los pilotos—. Negativo quiere decir que el primer brazo fue más rápido.

| Efecto (agregado, 16 casos × 6 rondas = 96 pares) | Mediana (ms) | IC 95 % (ms) |
|---|---|---|
| K1 − L0, la migración entera | **−8,2** | [−12,2, −6,1] |
| L1 − L0, el rediseño administrado | **−36,9** | [−38,1, −35,2] |
| K1 − L1, la contribución del kernel | **+27,7** | [+24,6, +31,8] |

Por caso, con seis pares cada uno. Las tres primeras columnas son la mediana de
latencia de cada brazo; las otras, la mediana de la diferencia pareada con su IC
cuando el veredicto lo necesita. Con seis rondas el IC de un caso es grueso: sirve
para el signo, no para la magnitud.

| Caso | L0 | L1 | K1 | K1 − L0 [IC 95 %] | L1 − L0 | K1 − L1 [IC 95 %] |
|---|---|---|---|---|---|---|
| `a-confined-check-that-may-not-run` | 39,7 | 3,9 | 27,1 | −14,0 [−18,4, −8,9] | −35,7 | +23,1 [+20,9, +29,8] |
| `a-failing-check-puts-it-back` | 33,4 | 3,8 | 26,4 | −7,2 [−17,6, −5,1] | −29,4 | +22,6 [+20,1, +24,7] |
| `a-failure-kept-on-request` | 26,2 | 4,1 | 46,3 | +17,2 [+7,2, +22,2] | −22,0 | +42,1 [+32,9, +44,2] |
| `a-loop-runs-out-of-calls` | 38,5 | 5,9 | 31,5 | −8,2 [−11,1, −2,5] | −32,5 | +25,4 [+20,1, +32,1] |
| `a-program-cannot-settle-its-own-boundary` | 33,2 | 6,1 | 44,0 | +7,7 [+3,6, +14,8] | −27,1 | +38,0 [+31,5, +41,7] |
| `a-program-that-looks-before-it-changes` | 48,3 | 10,0 | 60,1 | +11,8 [−5,7, +16,6] | −38,5 | +49,6 [+35,8, +55,3] |
| `a-rust-check` | 91,7 | 26,1 | 48,9 | −42,9 [−51,6, −37,8] | −65,5 | +22,8 [+18,3, +27,0] |
| `a-step-outside-the-workspace-is-refused` | 41,0 | 3,5 | 22,8 | −16,8 [−22,2, −6,2] | −37,3 | +19,3 [+16,9, +31,4] |
| `a-verb-that-is-not-exposed-is-not-reachable` | 33,4 | 3,0 | 23,6 | −9,2 [−13,6, −3,8] | −30,4 | +20,6 [+16,7, +28,5] |
| `a-verdict-about-an-older-tree` | 42,1 | 6,8 | 26,2 | −14,9 [−19,9, −11,4] | −35,4 | +19,2 [+17,1, +26,1] |
| `an-assertion-stops-the-program` | 44,1 | 5,3 | 30,5 | −12,0 [−22,0, −7,5] | −38,7 | +25,2 [+18,0, +31,9] |
| `context-decides-a-rename` | 45,7 | 8,7 | 44,4 | −1,2 [−4,9, +12,4] | −37,0 | +35,9 [+32,9, +49,2] |
| `it-worked-and-was-put-back` | 43,2 | 4,0 | 27,5 | −14,1 [−22,2, −9,8] | −39,1 | +23,6 [+17,8, +28,9] |
| `steps-edit-and-check` | 34,4 | 4,4 | 40,7 | +5,0 [+2,2, +13,0] | −29,9 | +35,9 [+31,0, +42,5] |
| `the-program-asks-for-the-model` | 156,5 | 18,1 | 40,3 | −115,5 [−122,7, −105,4] | −138,1 | +22,0 [+16,7, +27,7] |
| `two-transactions-in-a-row` | 74,9 | 9,0 | 68,7 | −6,2 [−10,7, +6,0] | −65,9 | +59,7 [+56,7, +69,4] |

`K1 − L1` es positivo en los 16 casos, con el IC entero sobre cero en todos.
`L1 − L0` es negativo en los 16. `K1 − L0` es negativo en el agregado, pero no en
todos los casos: en tres el IC queda entero sobre cero —`a-failure-kept-on-request`
(+17,2), `a-program-cannot-settle-its-own-boundary` (+7,7) y `steps-edit-and-check`
(+5,0)—, y en otros tres cruza el cero.

**`K1 − L1` por fase**, extraído de `exp13-results.json` (`phase_ns`, en ns ahí).
Mediana de la diferencia pareada e IC 95 % sobre rondas; los pares de cada fase son
los casos que la recorren. Las medianas de las fases no se suman a la del total:
la mediana de una suma no es la suma de las medianas, así que los ~4,7 ms entre
+23,0 y +27,7 no se atribuyen a ninguna fase.

| Fase | Pares | Mediana (ms) | IC 95 % (ms) |
|---|---|---|---|
| `open` | 96 | **+22,954** | [+21,851, +24,288] |
| `settle` | 96 | +0,193 | [+0,139, +0,270] |
| `end` | 96 | +0,022 | [+0,020, +0,024] |
| `request` | 96 | +0,007 | [+0,001, +0,016] |
| `validate` | 66 | +0,004 | [+0,002, +0,006] |
| `launch` | 12 | +0,004 | [−0,010, +0,010] |
| `observe` | 96 | +0,003 | [+0,002, +0,003] |
| `program` | 48 | −0,076 | [−0,138, −0,047] |
| `candidate` | 96 | −0,128 | [−0,149, −0,116] |

Casi toda la diferencia de K1 sobre L1 está en `open`. No se optimizó nada: la
campaña es lo que el protocolo congelado midió.

**Transporte: no medido.** La tabla de llamadas de transporte del resumen sale
vacía —`K1-L1 | None | [None, None]`, cero pares para los tres efectos—. No es un
cero: ninguna observación de la campaña trae la cuenta que esa tabla lee, y no se
afirma nada sobre cuántas llamadas mueve cada brazo.

**Los pilotos.** Antes de la campaña corrieron tres pilotos de una ronda. Los dos
primeros dijeron que la configuración del brazo K1 no se podía congelar; el tercero
es el que la congeló.

**El piloto — 2026-10-27, una ronda, los tres brazos.** Mismas revisiones e imagen
que las puertas; artefactos crudos en `build/exp13/pilot-r1/` y el análisis en
`build/exp13/pilot-r1-analysis/` (el `build/` de este repo no se versiona). La
orden, desde `~/thalyx`:

```
THALYX_EXP13_REPORT=$HOME/thalyx-kernel/build/exp13/pilot-r1 THALYX_EXP13_ROUNDS=1 \
THALYX_BTRFS_SCRATCH=/mnt/exp13 THALYX_K1_IMAGE=$HOME/thalyx-kernel/build/thalyx-k5.img \
THALYX_K1_LAUNCHER=$HOME/thalyx-kernel/tools/run_k1_link.py THALYX_ACCEL=kvm \
cargo test -p thalyx-cli --test exp13_equivalence -- --ignored --nocapture exp13_campaign
python3 dev/exp13/analyze.py $HOME/thalyx-kernel/build/exp13/pilot-r1
```

Mediana de la latencia extremo a extremo (`whole_ns`) sobre los 16 casos, con una
sola ronda —el intervalo bootstrap es sobre casos, no sobre rondas, y no sostiene
nada—: **L1 − L0 = −36,5 ms**, **K1 − L1 = +2 604 ms**, **K1 − L0 = +2 527 ms**.

Eso **no** es un veredicto sobre el kernel, y la razón está medida:

- El primer intento del piloto no tenía latencia de K1: el arnés buscaba la traza
  de plataforma en los directorios del host, y la evidencia de K1 vive en el
  almacén del guest. Se corrigió (`c8b9c27`, Thalyx: se lee del guest con el mismo
  cliente administrado) y la corrida sin trazas se conserva como resultado
  negativo en `build/exp13/pilot-r1-null-k1-traces/`.
- En una transacción de K1, `open` cuesta 2,2 s (L1: 1,8 ms) y `settle` 313 ms. La
  línea de tiempo del guest muestra notas consecutivas a ~2,7 ms y la siembra —una
  publicación de raíz, decenas de llamadas a K4— en 867 ms. El `diag.summary` de
  una corrida de la puerta del cerco: 955 registros, `write_ns = 2 182 649 795`,
  `trace=on`, `trace_withheld=0`; unos **2,3 ms por registro**, escritos de forma
  síncrona por el puerto serie, a menudo con el cerrojo tomado. El paquete de la
  etapa `thalyx` no lleva `TRACE_OFF`, que es justo lo que `diag.rs` dice que una
  medida necesita («a measurement taken with tracing on is a measurement of the
  tracing») y lo que el paquete K6 sí lleva. Además el link anota cada trama que
  entra y sale (`FRAME_IN`/`FRAME_OUT`), y esas notas son registros de resumen, que
  `TRACE_OFF` no calla.

Lo que ese piloto dejó decidido —imagen de medida con `TRACE_OFF`, sin notas por
trama en el link, las cuatro puertas sobre ella y un piloto nuevo— está hecho;
abajo.

**El segundo piloto — 2026-10-27, una ronda, los tres brazos, imagen sin trazas.**
Kernel en `c5d2ece`, Thalyx en `9d3ccce`, `build/thalyx-k1-notrace.img`. Crudos en
`build/exp13/pilot-r1-notrace/`, análisis en `build/exp13/pilot-r1-notrace-analysis/`,
órdenes exactas en `build/exp13/comandos.sh`. Sigue sin ser un veredicto: una
ronda, intervalo sobre casos.

| Efecto (mediana, ms) | Con trazas | Sin trazas |
|---|---|---|
| L1 − L0 | −36,5 | −36,3 |
| K1 − L1 | +2 604 | +1 012 |
| K1 − L0 | +2 527 | +971 |

`K1 − L1` por fase (mediana de la diferencia, ms; las otras cinco fases suman
menos de 1 ms en cada corrida):

| Fase | Con trazas | Sin trazas | IC 95 % sin trazas (sobre casos) |
|---|---|---|---|
| `open` | +2 140,3 | +897,3 | [+892,5, +908,8] |
| `settle` | +312,6 | +116,3 | [+107,6, +510,1] |
| `candidate` | +73,3 | −0,1 | [−0,1, +0,1] |
| `end` | +6,2 | +0,1 | [+0,05, +0,07] |

Lo que queda sigue siendo, en buena parte, el puerto serie. Dos corridas del
guest cuyo `diag.summary` se alcanzó a copiar (el arnés borra los logs del guest
al terminar; se copiaron en un bucle a `build/exp13/pilot-r1-notrace-serial/`):
`trace=off`, 609 y 565 registros, `write_ns` de 1,51 s y 1,39 s, unos 2,5 ms por
registro, con 1 159 y 1 030 trazas retenidas. De los 609, 388 son notas de
programa, y 319 de ellas son las notas por operación de K4 que `TRACE_OFF` no
calla porque son resúmenes: `RING_ACCEPTED` (138), `DISK_READ` (83), `DISK_WRITE`
(44) y `DISK_FLUSH` (11) de `k4disk`, `RECORD_WRITTEN` (43) de `k4store`. Las puertas
K4 se deciden desde esas notas, así que esta tanda no las tocó. Cuántas de ellas
caen dentro de la ventana de una transacción no está medido: el `whole_ns` es del
reloj del host y estas cifras son de la corrida entera del guest, arranque
incluido.

Lo que el segundo piloto dejó abierto —si las notas por operación de K4 salen del
camino medido de K1— se resolvió con la imagen de medida de arriba, sin quitarle a
la puerta K4 lo que lee.

**El tercer piloto — 2026-10-27, una ronda, los tres brazos, imagen de medida.**
Kernel en `60ef827`, Thalyx en `9d3ccce`, `build/thalyx-k1-measure.img`. Crudos en
`build/exp13/pilot-r1-measure/`, análisis en `build/exp13/pilot-r1-measure-analysis/`,
logs serie del guest en `build/exp13/pilot-r1-measure-serial/`. Sigue sin ser un
veredicto: una ronda, intervalo sobre casos.

| Efecto (mediana, ms) | Con trazas | Sin trazas | Medida | IC 95 % medida (sobre casos) |
|---|---|---|---|---|
| L1 − L0 | −36,5 | −36,3 | −36,0 | [−37,7, −29,5] |
| K1 − L1 | +2 604 | +1 012 | **+29,7** | [+19,2, +34,5] |
| K1 − L0 | +2 527 | +971 | −6,3 | [−17,9, +1,5] |

`K1 − L1` por fase en la imagen de medida: `open` **+22,0** [+17,9, +25,5],
`settle` +0,2 [+0,08, +11,7], `candidate` −0,1, `program` −0,1; las otras cinco, a
menos de 0,03 ms. Lo que en los dos pilotos anteriores era `open` y `settle` era,
casi entero, el puerto serie.

**Registros seriales por transacción.** Ventana: de la nota `WORK_OPENED` del link
a la siguiente nota del puerto de control, que es la primera petición del host
después de la transacción; el host lee antes la traza de plataforma, así que la
ventana es una cota superior. Contados con `build/exp13/count_tx_records.py`:

| Imagen | Ventanas completas | Mínimo | Mediana | Máximo |
|---|---|---|---|---|
| Sin trazas | 10 de 17 | 168 | 219 | 472 |
| Medida | 17 de 17 | 5 | **5** | 11 |

Los cinco de una transacción con publicación son uno por capa: `WORK_OPENED` del
link, `PUBLISHED`, `EFFECT_ADMITTED` y `ROOT_HEAD` de `k4store`, y `AUDIT_EFFECT`
del supervisor; diez u once cuando la transacción publica dos veces. El
`diag.summary` de las ocho corridas que lo alcanzaron dice 285 a 301 registros por
corrida entera, `write_ns` de 0,60 a 0,65 s —unos 2,1 ms por registro—, casi todos
del arranque. A ese costo, cinco registros son unos 10 ms de una transacción: el
orden de un tercio de los +29,7 ms que quedan. Eso es una cuenta, no una medida
de la ventana; cuánto de `K1 − L1` es todavía el puerto serie no está medido.

El protocolo se congela tras un piloto: una configuración común para los tres
brazos —mismo corpus, mismo host, mismas condiciones de VM— y no se afina un brazo
por separado. La disciplina de medida es la de K6: rondas pareadas, artefactos
crudos conservados, digests, revisiones y línea de órdenes preservadas, e
intervalos sobre rondas pareadas.

### El veredicto causal

La forma del veredicto estaba fijada de
antemano para que los datos no elijan la pregunta: una afirmación de que
«Thalyx-Kernel mejora a Thalyx» exige corrección semántica y garantías
sostenidas (las puertas de arriba), ninguna ejecución oculta acreditada a K1 (el
lanzamiento y la validación son host-side y están declarados), ninguna regresión
material en los resultados primarios de Thalyx, y al menos una mejora primaria
sostenida. `K1 − L1` es la única cifra que puede sostener «el kernel específico
mejora»; `K1 − L0` sin `K1 − L1` sostiene «la migración ayudó», que es una
afirmación distinta y más débil.

**El veredicto, con ese marco y la campaña de arriba:**

- **La migración ayudó, y ayudó por el rediseño administrado.** `K1 − L0` =
  −8,2 ms [−12,2, −6,1]: K1 es más rápido que la línea base en el agregado.
  `L1 − L0` = −36,9 ms [−38,1, −35,2]: el rediseño, todavía en Linux, explica más
  que esa mejora entera. Lo que la migración ganó lo ganó el modelo administrado,
  no el kernel. En tres casos de dieciséis K1 queda más lento que L0 con el IC
  sobre cero; «ayudó» es una afirmación del agregado.
- **Una ventaja específica del kernel en latencia no se demostró.** `K1 − L1` =
  +27,7 ms [+24,6, +31,8], positivo en los dieciséis casos: en esta configuración
  el brazo del kernel es más lento que el mismo modelo en Linux, y casi todo está
  en `open` (+23,0 ms). Con los límites de abajo —QEMU, transporte por consola
  virtual, registros seriales— esa cifra no se atribuye al kernel en sí; lo que sí
  sostiene es que «el kernel específico mejora la latencia» no tiene datos a favor.
- **La única diferencia declarada a favor del kernel es la garantía de escritor
  exclusivo** ([arriba](#la-garantía-más-fuerte-que-k1-declara-sobre-l1)):
  `prevented_kernel_owns_the_medium` contra `detected_by_digest_not_prevented`.
  Es una garantía declarada y sostenida por las puertas, no una cifra de esta
  campaña.
- Lo demás del marco: corrección semántica y garantías, sostenidas por las cuatro
  puertas sobre la misma imagen; ninguna ejecución oculta acreditada a K1, porque
  lanzamiento, validación y reloj son del host y están declarados.

## Límites reales que quedan

Los de la campaña, que acotan el veredicto de arriba:

- **QEMU/KVM, con lanzamiento, validación y reloj en el host.** El Thalyx real
  corre en el host y conduce el kernel arrancado en QEMU por una consola
  virtio; el lanzamiento y la validación de la revisión son host-side, y
  `whole_ns` es del reloj del host, de modo que cada transacción de K1 incluye ir
  y volver por la consola virtual. No hay una cifra de K1 medida desde dentro del
  guest.
- **~5 registros seriales por transacción.** La imagen de medida todavía escribe
  unos cinco registros de resumen por transacción con publicación (diez u once si
  publica dos veces), a unos 2,1 ms cada uno: del orden de 10 ms. La cuenta es del
  tercer piloto, sobre la misma imagen; en la campaña no se contó, y cuánto de
  `K1 − L1` es todavía el puerto serie no está medido.
- **Una máquina.** Una sola máquina anfitriona, una corrida de seis rondas; ni
  otra máquina ni otra campaña la repiten.
- **La tabla de transporte, vacía (`None`), es no medida**, no cero.

Los heredados:

- Todo es QEMU: TCG para las puertas canónicas, KVM para la campaña. La máquina
  física está inventariada y no arrancada; los límites de K3/K4/K5 —sin
  aislamiento de DMA, durabilidad con supresión del driver y no corte de energía,
  un modelo pequeño— se heredan enteros.
- El lanzamiento y la validación de la revisión real de Thalyx son host-side por
  necesidad: no hay `cargo` ni frontend de Rust dentro del kernel. K1 no reclama
  lo contrario, y esa es la parte del veredicto que se mantiene honesta.
- La campaña con inferencia en vivo (motor residente, pesos idénticos) es posible
  reutilizando el motor de K5, pero no está cableada a este arnés en esta rama; la
  vertical medida de K1 es `contexto → agente → hacer/QuickJS → herramientas →
  validación → congelar → publicar/abandonar → evidencia`, sin el track de motor.

Relacionado: [la frontera con Thalyx](../integration/thalyx.md), [el port de K5](k5-thalyx-port.md),
[la campaña K6](k6-comparison-hardening.md), [el estado actual](../roadmap/current-state.md).
