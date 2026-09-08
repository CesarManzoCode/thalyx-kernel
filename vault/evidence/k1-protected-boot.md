---
id: EVD-004
kind: evidence
status: observed
---
# K1 — Arranque protegido: qué se ejecutó y qué demuestra

Esta nota registra la primera ejecución real de código de este proyecto. Describe lo que se observó, no lo que la arquitectura promete. La distinción importa especialmente aquí: es la primera vez que el repositorio puede confundir «el diseño dice» con «el sistema hizo».

## Qué se ejecutó

Una imagen UEFI construida solo desde fuentes, arrancada en QEMU con OVMF, con la salida serie capturada literalmente y evaluada después por un comprobador independiente.

| Artefacto | Comando | Resultado |
|---|---|---|
| Imagen | `python3 tools/build_image.py` | `build/thalyx-k1.img`, manifiesto con digest de cada artefacto y herramienta |
| Ejecución | `python3 tools/run_k1.py` | `exit_status=33` (`k1.terminal status=complete`), 354 registros, ~1.5 s |
| Veredicto | `python3 tools/check_k1.py` | `K1 GATE PASSED: 13 of 13 criteria met` |

Toolchain fijada: `rustc 1.98.1`, targets `x86_64-unknown-uefi` y `x86_64-unknown-none`, QEMU 11.1.1 con OVMF, perfil de CPU `qemu64,+smep,+smap,+pdpe1gb`, un solo núcleo, 512 MiB.

Digests de la ejecución registrada:

```text
image   6096a050595533f996ff442366d3b659c640099dc7aab7ab29013c66dcd2e021
kernel  754453c7104aaae493e42070cae60c5d959961238b548f176ebfa1c03e4d550d
loader  3f79e2b066df4e9b0a064a8c185e058e17ef2b191ed0b79d13eb771a342a2e96
package f382855faaf039c510c36f9e43038f79b488cdd6822b4f350d1df834e30da950
```

La imagen es reproducible byte a byte: dos construcciones de las mismas fuentes, incluida una tras borrar el directorio de staging, producen el mismo digest. Para conseguirlo se fijan el número de serie del volumen FAT y las marcas de tiempo de los directorios, que de otro modo dependen del reloj. Los contadores temporales de una ejecución —ticks, número de preempciones— **no** son reproducibles: dependen del ritmo de emulación TCG. El criterio de la puerta es una propiedad observada, no una cifra exacta.

## Qué observó la ejecución

El loader validó kernel y paquete, construyó tablas, salió de boot services y entregó el control. El kernel aceptó el registro de arranque, tomó posesión de los marcos físicos y de sus propias tablas, instaló GDT/IDT/TSS con pilas de emergencia, midió el reloj contra el PIT y armó el timer del LAPIC a 2000 Hz.

Después construyó cuatro dominios en cuatro espacios de direcciones distintos y los ejecutó en ring 3, confirmado desde el marco interrumpido de cada uno: `cs=0x23`, `ss=0x1b`, `cpl=3`, `iopl=0`. El timer les quitó la CPU sin que la pidieran: las preempciones registradas son de código de usuario, con `trigger=timer` y `voluntary=0`, sobre los cuatro dominios.

Cada dominio plantó un patrón FP/SSE derivado de su propio identificador —los dos dominios creados desde la misma imagen plantaron valores distintos— y verificó ese patrón intacto después de perder la CPU, en cada intervalo.

Dos dominios ejecutaron accesos ilegales deliberados y anunciados previamente. `trespasser` leyó la base del mapa del kernel, presente en su espacio pero sin bit de usuario, de modo que el fallo es una violación de privilegio y no una traducción ausente. `wxprobe` escribió sobre su propio texto ejecutable, mapeado y suyo, denegado únicamente por el bit de escritura que el kernel no concede a una página ejecutable. Ambos fallos fueron clasificados como `user_fault`, terminaron solo a su dominio y devolvieron sus marcos a cero cargados. El kernel siguió ejecutando y emitió 194 registros posteriores.

Los dos `worker` supervivientes continuaron avanzando su propio contador después del último fallo, hasta terminar voluntariamente. El paquete incluía además un módulo malformado cuyo primer segmento cargable apunta a espacio de kernel; el validador lo rechazó por el mismo camino que aceptó a los demás, antes de ejecutarlo.

## Cómo se decide la puerta

El estado de salida solo dice que el kernel se quedó sin dominios ejecutables. Un kernel que imprimiera desde ring 0 y se detuviera produciría el mismo estado, y [la ruta](../roadmap/phases.md) excluye explícitamente ese caso. Por eso el veredicto no lo emite ni el kernel ni el script de ejecución, sino un comprobador que lee los registros del propio kernel y decide cada criterio por separado.

El comprobador se validó con controles negativos: confirmaciones de ring 3 eliminadas, preempción reetiquetada como voluntaria, contención reetiquetada como reanudación, un log truncado en el fallo, un arranque sin ningún registro de usuario, estado FP reportado como alterado y todos los dominios plantando el mismo patrón. Cada control hace fallar el criterio que le corresponde y no otros. Un comprobador que no falla nunca no es evidencia.

## Qué queda demostrado y qué no

| Invariante | Alcance cubierto por esta ejecución | Lo que falta |
|---|---|---|
| [INV-01](../validation/invariants.md) | Dos probes adversarios contenidos: privilegio y W^X, con el dominio terminado y sus marcos devueltos. | Canales laterales, reutilización de RAM, DMA y perfiles de dispositivo. |
| [INV-21](../validation/invariants.md) | Una imagen inválida no llegó a ejecutarse; los dominios se activan tras construir mapas y pila. | Omitir sistemáticamente cada requisito y comprobar el rechazo de todos. |
| [INV-22](../validation/invariants.md) | Patrones FP/SSE distintos por dominio, intactos tras preempción. | Corrupción bajo fallos repetidos, SMP y features FP más amplias. |

[EXP-01](../validation/experiments.md) queda **parcialmente** ejecutado. Su parte de K1 —primer ring 3 tras protección, probes ilegales sin daño a otro dominio, temporizador que preempta un bucle— se observó. Su parte de K2 no.

Esta ejecución **no** demuestra: capacidades, autoridad, IPC, ámbitos de trabajo, contabilidad de recursos, SMP, drivers, DMA, estado durable, hardware físico ni rendimiento. Nada de eso está implementado. La ejecución es una sola, en un emulador, con un núcleo.

El plano de diagnóstico usado aquí es el mecanismo temporal que [la ruta](../roadmap/phases.md) autoriza para K1, no el plano de recibos de [observabilidad](../architecture/observability.md): no reserva nada, no es alcanzable por capacidad, puede fusionar registros repetidos y ninguna operación se rechaza porque no se pudiera escribir. No debe presentarse como historia auditada. Se retira cuando exista un supervisor en K2. La salida por el puerto de depuración del emulador está confinada a un módulo por la misma razón.

Que el kernel sobreviva a dos fallos previstos no dice nada sobre fallos imprevistos. Que Rust seguro cubra la mayor parte del código no elimina la confianza en compilador, linker y firmware, según [los invariantes](../validation/invariants.md).

## Cómo repetirlo

```sh
python3 tools/build_image.py          # imagen + manifiesto de digests
python3 tools/run_k1.py               # arranque en QEMU + captura serie
python3 tools/check_k1.py             # veredicto por criterio
```

Si QEMU, OVMF o mtools no están instalados en el sistema, `THALYX_TOOL_PREFIX` apunta a un árbol que los contenga; `python3 tools/toolchain.py` informa de qué binario resolvió cada uno.
