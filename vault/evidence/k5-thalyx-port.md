---
id: EVD-008
kind: evidence
status: observed
---
# K5 — Port de Thalyx: qué se ejecutó y qué demuestra

Esta nota se escribe mientras K5 se ejecuta, etapa por etapa, y dice en cada momento qué está demostrado y qué no. K5 es el port: la semántica de Thalyx sobre los mecanismos de este kernel en vez de sobre los de Linux. Lo más fácil de fingir en un port es que haya habido un port, así que la disciplina de esta nota es una sola:

**Construir no es ejecutar.** Un compilador cruzado que produce una imagen no demuestra nada. Lo que demuestra algo es que el kernel construyó un dominio con esa imagen, lo activó, lo planificó, le cobró recursos y registró lo que hizo. Todas las afirmaciones de abajo se deciden desde los registros del kernel, y donde una se decide desde una nota del propio programa, el criterio lo dice en su título.

## Estado

| Etapa | Qué establece | Estado |
|---|---|---|
| `smoke` | El target nativo `x86_64-thalyx`: una imagen C real entra, se sostiene sobre la pila que el supervisor mapeó, habla con el kernel, hace coma flotante en hardware y hace crecer un heap con objetos de memoria que crea contra su propio ámbito. | **Ejecutado** |
| `surface` | Primera superficie nativa de Thalyx con fixtures y agente externo. Integración parcial, etiquetada como tal. | Pendiente |
| `work` | Programa acotado real y herramienta nativa real dentro del kernel. | Pendiente |
| `engine` | Motor CPU residente real y la toolchain que la carga de referencia necesita. | Pendiente |

## El target nativo

`x86_64-thalyx` no es un triple: es un conjunto de promesas, y está definido en un solo sitio, [`tools/build_native.py`](../../tools/build_native.py).

- ELF64 `ET_EXEC`, estático, sin intérprete, sin reubicaciones, entrada `_start`, tres segmentos `PT_LOAD` alineados a página y separados R-X / R-- / RW-.
- Base 0x400000, modelo de código pequeño, sin PIC. El cargador acotado del kernel no reubica.
- SSE y SSE2 en hardware, `-mfpmath=sse`, y **sin AVX**. Este kernel guarda y restaura el área `FXSAVE` heredada de forma ansiosa y no habilita `XSAVE`: un programa que usara registros anchos los perdería o los filtraría en un cambio de contexto. Es la única bandera cuya ausencia sería un fallo de corrección y no una decisión de rendimiento.
- Sin protector de pila, sin tablas de desenrollado, sin `.init_array`: nada por debajo desenrolla y nada ejecuta constructores.
- Una libc que es `user/native` y ninguna otra. `-nostdinc` es lo que hace eso cierto en vez de intencionado: las cabeceras del anfitrión no están en la ruta de búsqueda, así que un programa que busque una función POSIX falla al compilar aquí en vez de fallar al enlazar dentro del invitado.

La zona roja queda habilitada a propósito: la CPU cambia a la pila por procesador del TSS en cada interrupción y a la pila de `syscall` del kernel en cada entrada, así que nada escribe por debajo del puntero de pila de un programa de usuario.

## El runtime nativo

`user/native` es lo que un programa C pisa. No hay POSIX debajo y nada es una capa de compatibilidad:

- **Asignación.** `malloc` son objetos de memoria que el programa crea contra **su propio ámbito** y mapea en **su propio dominio**, ambos por capacidades que el supervisor instaló. El asignador es de etiquetas de frontera con listas por clase de tamaño y fusión en las dos direcciones. Un programa construido sin esas dos capacidades no puede crecer, que es la forma prevista de construir uno que no debe.
- **Hilos.** El kernel rechaza `DOMAIN_ADD_THREAD` sobre un dominio que ya corre, así que un programa nativo no crea hilos: su supervisor los crea antes de activarlo, todos entrando en el mismo trampolín con un índice, y el runtime los bloquea en una señal hasta que hay trabajo. Bloquear es un `SIGNAL_WAIT` real, no una espera activa. No hay segmento de almacenamiento local por hilo —poner `FS` necesita una instrucción que este kernel no habilita—, así que un hilo se identifica por su pila: las ranuras están alineadas a su propio tamaño y una máscara devuelve el índice.
- **Tiempo.** `CLOCK_QUERY` es la única fuente. No hay reloj de pared sincronizado y no se finge uno.
- **Transporte.** Mensajes acotados por `MAX_INLINE_PAYLOAD`, con el origen que el kernel estampa y capacidades prestadas como concesiones derivadas. Nada mayor viaja copiado: viaja en un objeto de memoria cuya capacidad lleva el mensaje, de modo que una transferencia grande es visible como autoridad.
- **Matemáticas.** `user/native/src/math.c` está escrito aquí y **no** redondea correctamente. Lo dice en su cabecera y se repite donde importa: el motor de inferencia pasa por `expf`, así que sus logits difieren de los de otra implementación en los últimos bits pase lo que pase —la `expf` vectorizada de llama.cpp difiere de la de glibc por la misma razón—. Por eso la evidencia del motor compara el token que elige un muestreador voraz y el margen con que ganó, y no un patrón de bits.
- **Salida formateada.** `printf` va al plano de diagnóstico, ocho bytes por nota. Es un canal de depuración, se puede fusionar, y nada durable se deriva de él.

## Etapa `smoke`: qué se ejecutó

Una quinta imagen UEFI construida desde las mismas fuentes con el mismo script y **el mismo binario de kernel** que las imágenes K1 a K4. Lo único que cambia es el paquete de arranque, que aquí lleva dos toolchains: el supervisor en Rust sobre el target del kernel y el programa C sobre el target nativo.

| Artefacto | Comando | Resultado |
|---|---|---|
| Runtime nativo | `python3 tools/build_native.py` | `build/native/libthalyx-native.a` y las imágenes de la etapa |
| Imagen | `python3 tools/build_image.py --phase k5 --stage smoke` | `build/thalyx-k5.img`, manifiesto con digest de cada artefacto y herramienta |
| Ejecución | `python3 tools/run_k5_stages.py` | `exit_status=33` (`k1.terminal status=complete`) |
| Veredicto | `python3 tools/check_k5.py` | `K5 GATE PASSED: 13 of 13 criteria met` |
| Autocomprobación | `python3 tools/check_k5.py --self-test` | `14 damages, each noticed by the criterion named` |

Plataforma: QEMU `q35` con acelerador `tcg`, cuatro procesadores, CPU `qemu64,+smep,+smap,+pdpe1gb,+x2apic`, 1024 MiB y OVMF.

Qué decidió cada criterio, y desde dónde:

- **El kernel construyó y ejecutó un dominio desde una imagen C.** `domain.created` nombra el objeto de imagen del que salió y el punto de entrada que la imagen declaró; `domain.activated` la puso a correr; y las notas del programa llevan el recuento de preempciones que el planificador le cobró. Seis preempciones: el programa fue interrumpido y continuó.
- **El runtime se levantó.** `.bss` a cero medido por el propio programa sobre un arreglo estático, la consulta de límites contestada, y el número de procesadores que el kernel le dijo al programa comparado contra los `smp.ap_online` que el kernel registró por su cuenta.
- **Coma flotante en hardware.** El programa ejecuta un bucle de sesenta y cuatro pasos con dependencia de acarreo, sembrado por el plan que el anfitrión construyó en la imagen, y reporta el resultado escalado. El anfitrión recalcula el mismo bucle en doble precisión y exige coincidencia: `1063483505` en los dos. Un programa que no hubiera hecho la aritmética no habría podido decir ese número, y el bucle atraviesa tres preempciones, que es lo que hace la comprobación también una comprobación del guardado y restaurado de estado FP.
- **El heap son objetos de memoria.** 192 páginas en el primer crecimiento, en tres arenas de 64; los bytes escritos a través de los mapeos nuevos se leen de vuelta y su suma sobre 256 desplazamientos muestreados es exactamente la esperada.
- **El techo lo pone el kernel.** El registro de arranque permite mucho más heap del que el ámbito puede cobrar, así que lo que detiene el crecimiento es `SCOPE_CREATE_MEMORY` rechazando: el objeto nunca se crea, el mapeo nunca ocurre y `malloc` no tiene qué devolver. La nota lleva el estado del kernel, `LIMIT_EXHAUSTED` (−9), y no un límite propio del programa. 512 páginas retenidas al llegar al techo.

Las regresiones K1, K2, K3 y K4 son cuatro criterios más de esta puerta, y las cuatro pasan sobre el mismo kernel: 13, 21, 28 y 31.

## Lo que la etapa `smoke` corrigió

Dos defectos que compilar no encuentra:

1. **Una imagen sin script de enlace.** El primer supervisor K5 se construyó sin el script que separa los segmentos, y el kernel lo rechazó con `segment_unaligned` antes de ejecutar una instrucción. El rechazo es el comportamiento correcto; el defecto era del paquete.
2. **Un objeto de memoria que nadie podía mapear.** El heap creaba sus arenas con derechos máximos de lectura y escritura y sin `MEMORY_MAP`, y el kernel rechazaba el mapeo por derechos insuficientes. `MEMORY_MAP` es el derecho a hacer un mapeo, no un permiso de página, y tiene que estar en los derechos máximos del objeto además de en la petición. Antes de la corrección el programa no podía asignar un solo byte y la primera señal era `malloc` devolviendo nulo.

## Lo que esta etapa **no** demuestra

- No hay todavía nada de Thalyx ejecutándose. `smoke` establece el target y el runtime, no la semántica.
- No hay QuickJS, ni herramienta de validación, ni motor de inferencia, ni estado administrado en esta etapa.
- Sigue sin haber aislamiento de DMA, hardware físico ni durabilidad frente a corte de energía: los límites de K3 y K4 se heredan enteros.

## Digests de la ejecución registrada

```text
thalyx-k5.img         0403e1197fc3f8cb555376e723f7a3326a6c04b40dfb8f368aba773225ac0a26
kernel.elf            8597eb87f8dfafad9c0112ee1479d60d398ca2a859ebc882da45166abb44b8b7
nsmoke.elf            ef6f46d5fd1c01f0832a455deb7cd859a218c34d67ce19c64e29eccad2874dd4
libthalyx-native.a    e0621cc37ead417d50e90f6f4b387f1da429d501b0286adc07422767ad91edfe
```
