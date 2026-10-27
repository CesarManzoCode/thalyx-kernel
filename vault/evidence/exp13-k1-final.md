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
observaron con este kernel en `c0fe9e1` y Thalyx en `c8b9c27`; la campaña fija
sus propias revisiones cuando corra.

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

**Qué no establece.** Las puertas corrieron bajo KVM, no TCG; son las de EXP-13,
conducidas desde el host, no las puertas K1–K5 del kernel, que esta tanda no
volvió a correr (el cambio en `k2boot.rs` es idéntico por construcción con un solo
dispositivo, que es lo que esas fases arrancan, pero no se ejecutó). El
lanzamiento y la validación siguen siendo del host, como declara el perfil.

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

**La campaña: NO MEDIDA.** Sólo corrió el piloto, y el piloto dice que la
configuración del brazo K1 todavía no se puede congelar.

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

Lo que el piloto deja decidido para congelar el protocolo, **sin ejecutar
todavía**: construir la imagen de medida del brazo K1 con `TRACE_OFF`, como K6;
quitar del camino de cada petición las notas por trama del link, o contarlas sin
escribirlas; volver a correr las cuatro puertas sobre esa imagen, porque las de
arriba corrieron con trazas; y repetir el piloto antes de la campaña.

El protocolo se congela tras un piloto: una configuración común para los tres
brazos —mismo corpus, mismo host, mismas condiciones de VM— y no se afina un brazo
por separado. La disciplina de medida es la de K6: rondas pareadas, artefactos
crudos conservados, digests, revisiones y línea de órdenes preservadas, e
intervalos sobre rondas pareadas.

### El veredicto causal

**Pendiente de la corrida física.** La forma del veredicto está fijada de
antemano para que los datos no elijan la pregunta: una afirmación de que
«Thalyx-Kernel mejora a Thalyx» exige corrección semántica y garantías
sostenidas (las puertas de arriba), ninguna ejecución oculta acreditada a K1 (el
lanzamiento y la validación son host-side y están declarados), ninguna regresión
material en los resultados primarios de Thalyx, y al menos una mejora primaria
sostenida. `K1 − L1` es la única cifra que puede sostener «el kernel específico
mejora»; `K1 − L0` sin `K1 − L1` sostiene «la migración ayudó», que es una
afirmación distinta y más débil.

## Límites reales que quedan

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
