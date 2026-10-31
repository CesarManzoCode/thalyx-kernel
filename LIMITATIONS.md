# Límites conocidos

Única fuente de los límites de Thalyx-Kernel que dejó al descubierto cada K. Cada entrada dice qué se observó y dónde está la evidencia; los límites anteriores a K7 siguen descritos en `vault/roadmap/open-questions.md` y en la nota de evidencia de cada fase.

## Desde K7

- **K7 cerró con 38 de sus 50 criterios.** Los 12 que no se cumplieron se quitaron del plan después de agotar cinco rondas: las fases `end`, `request`, `validate` y `observe` de la transacción, `entry.clock`, `scale.ipc` con 3 pares, `scale.compute` con 4 hilos, `engine.load`, `engine.infer` con el prompt 4, `engine.cancel`, y los dos criterios de comportamiento (tres corridas de las puertas y al menos 392 tests).
- **Las puertas K1–K6 no son estables en tres corridas seguidas.** En la medida final de K7 (kernel `2eca0c8`), la tercera de tres corridas TCG falló en la puerta K5 y en su autocomprobación, y la puerta K6 de la misma campaña dio 23/24 por `no_loss` (−9 en un banco nativo de la ronda 4). Durante K7 también cayeron, de forma intermitente, K2 `budget`, K3 (`probe`, `shootdown`) y K4 (`check_outbox`). La verificación de cierre corre cada puerta una sola vez.
- **No se sabe qué reserva produce el −9 (`LIMIT_EXHAUSTED`) de K6.** La imagen de medida no escribe qué recurso rechazó; que sea el log de control lleno mientras el auditor espera su ventana es una hipótesis (ADR-014, ADR-023, ADR-024).
- **Una transacción de Thalyx sigue siendo más lenta sobre este kernel que sobre Linux.** En EXP-13, K1 − L1 = +4,447 ms [+4,322, +4,932], casi todo en la fase `open` y en cada publicación extra de K4. Los almacenes de los dos brazos están en tmpfs: ninguno paga un flush del anfitrión, así que la comparación no mide durabilidad en disco.
- **Las fases de la transacción que corren en el anfitrión no bajan de 1 µs de diferencia.** `request`, `validate` y `observe` no tocan la máquina virtual; sus umbrales del baseline estaban por debajo de 1 µs.
- **`entry.version` y `entry.clock` están cuantizados a 10 ns.** Con medianas de 40 a 60 ns, un umbral estricto depende de qué fracción de muestras cae en cada escalón (ADR-022).
- **IPC con cuatro pares rinde 0,826 de Linux sin mitigaciones, y derivar una capacidad cuesta 3,07 veces lo de Linux.** Con 1 a 3 pares el nativo supera a Linux.
- **El cliente nativo de K6 gira 20 ms después de cada relevo, fuera de las muestras** (`tests/k6/plat_native.c`, ADR-026). El brazo Linux no tiene relevos. La causa que lo motivó, un núcleo del anfitrión con la frecuencia bajada mientras el procesador virtual del cliente está parado, se infirió y no se midió.
- **La mejora de `ipc.call` incluye un cambio en el servidor del benchmark.** Con el ABI 0.5, el servidor nativo de K6 cierra el ticket en la misma respuesta (`CLOSE_AFTER`) y hace dos entradas al kernel por llamada en lugar de tres.
- **La imagen de medida calla las notas por transacción** de `k4store` y `k5super` (`--k4-io-notes off`, ADR-011); ninguna puerta que corre sobre ella lee esas notas. Un `get` servido desde la copia en la arena de k5link no observa el medio en esa lectura; un medio dañado durante la corrida se ve en la siguiente lectura que va a K4 (ADR-012).
- **EXP-13 necesita `/mnt/exp13` montado, y el montaje loop se pierde en cada reinicio.** Montarlo requiere root.
- **El cierre usa 6 rondas de K6; la campaña recomienda 57** (`{"rounds_recommended": 57}` en la salida de `check_k6.py`).
- **El build del workspace da dos advertencias:** la feature `quiet-io` no está declarada en `thalyx-user-k6super`, que compila `k5super/src/audit.rs` (`unexpected_cfgs`), y `rust-lld` no encuentra `_start` al enlazar `bootx64` para `x86_64-unknown-none`.

## Desde K8

- **En la etapa `thalyx`, `k4store` no relee del medio lo que ya verificó en la misma ejecución** (ADR-032). Un medio que pierde o daña durante la ejecución un árbol, manifiesto, política, validación o recibo que esa ejecución escribió se ve al recuperar, no en la lectura. Las imágenes de las puertas K4 y K5 no tienen la caché.
- **El −9 de K6 sigue sin causa conocida.** Desde ADR-032 el kernel cuenta cada rechazo de admisión por recurso (`k2.admission_refusals`), pero ninguna campaña lo ha leído todavía.
