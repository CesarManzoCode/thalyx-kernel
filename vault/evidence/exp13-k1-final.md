---
id: EVD-011
kind: evidence
status: designed
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
de este kernel es `feat/exp13-k1-final` sobre `fde5bee`. Los dos repos quedan
fijados en la campaña.

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
        └── L<vista>   por línea: H sus generaciones, E su evidencia, R su recibo
```

Una publicación de Thalyx es una publicación de K4 que mueve la `H` de una línea,
con CAS sobre la generación de la raíz **y** decidida contra la generación de la
línea; guardar evidencia mueve su `E`. Dos publicaciones no se entrelazan porque
el link sirve una petición a la vez y K4 admite un `PREPARE` a la vez, y un corte
entre `PREPARE` y `COMMIT` lo resuelve la recuperación de K4 exactamente como en
su matriz. El detalle está en `user/k5link/src/managed.rs`.

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

**NO MEDIDO en esta rama.** La implementación, el arnés y el análisis están
completos y listos para ejecutarse; los números los produce la corrida física en
la máquina de Cesar, bajo `THALYX_ACCEL=kvm`, y se pegan aquí desde
`exp13-summary.md` que `analyze.py` escribe. Hasta entonces esta sección declara
la medida pendiente y no una cifra inventada.

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
