---
id: ADR-011
kind: decision
status: accepted
---
# El puerto serie escribe por ráfagas de FIFO, y la imagen de medida calla las notas por transacción

**Problema.** El plano de diagnóstico escribe cada registro por COM1 de forma síncrona y por sondeo. Bajo KVM cada acceso a un puerto es una salida del invitado al emulador, y el driver hacía dos por byte: leer el estado de línea y escribir el dato. El baseline de K7 mide unos 15,6 µs por byte (`write_ns / bytes` de `diag.summary` en los arranques nativos de K6), unos 2,1 ms por registro en la imagen de medida, y casi todo el arranque hasta `LINK_READY` es escritura de registros. Además, en la imagen de medida de EXP-13 cada transacción con publicación deja cinco registros en su ventana —`WORK_OPENED` del link; `EFFECT_ADMITTED`, `PUBLISHED` y `ROOT_HEAD` de `k4store`; `AUDIT_EFFECT` del supervisor— y cada uno cuesta milisegundos dentro de la latencia que EXP-13 compara.

**Evidencia.** Observado: el baseline de K7, dos réplicas del mismo árbol (`1eacb11`), con sus crudos fuera del vault; [EXP-13](../evidence/exp13-k1-final.md), sección del tercer piloto, para el peso del serial en el arranque y en la transacción. Observado en el código: las pruebas de EXP-13 (`exp13_equivalence` de Thalyx) no leen el serial —el lanzador `tools/run_k1_link.py` no lo interpreta y la observación llega por la consola virtio—, y la imagen de medida (`--k4-io-notes off`) solo la usan esas pruebas y la campaña. Las notas que se callan las leen las puertas K4 y K5, que corren imágenes sin `quiet-io`.

**Elección.**

*Ráfagas de FIFO.* El kernel programa el 16550 con FIFO desde K1 (`FCR = 0xC7`), pero escribía como si no la tuviera. Un 16550A con FIFO activa solo informa el registro de transmisión vacío cuando la FIFO entera está vacía, así que tras un sondeo positivo acepta dieciséis bytes sin volver a preguntar. Ahora `Uart::init` lee `IIR` y confirma la FIFO (bits 7:6); `Uart::write_burst` sondea una vez por cada dieciséis bytes, o por cada byte si la FIFO no se confirmó, que es el comportamiento anterior. El plano de diagnóstico reúne cada registro en un búfer de 256 bytes en la pila antes de enviarlo, porque `write!` lo entrega en fragmentos de pocos bytes y cada fragmento habría empezado con su propio sondeo. Los bytes en el cable son los mismos —formato `THLX1`, saltos de línea expandidos a CRLF—; el registro sigue en el cable antes de que su escritor vuelva; la ruta de pánico sigue escribiendo byte a byte. `diag.summary` declara `burst=<n>`, los bytes por sondeo, después de los campos que ya tenía.

*Notas por transacción, solo en la imagen de medida.* La feature `quiet-io` de `k4store` calla también `EFFECT_ADMITTED`, `PUBLISHED` y `ROOT_HEAD` de cada publicación; una feature `quiet-io` nueva de `k5super` calla `AUDIT_EFFECT` por recibo de efecto. `tools/build_image.py --k4-io-notes off` activa las tres, y solo la etapa `thalyx` lo admite. Los rechazos, la recuperación, el `ROOT_HEAD` adoptado al recuperar y el total `AUDIT_EFFECTS` del informe del supervisor se escriben en las dos imágenes. `WORK_OPENED` (`0x510a`) y la nota de control (`0x510e`) de `k5link`, que delimitan la ventana, no se tocan.

**Diseñado, no medido.** Lo que se espera mover: `k7.serial_ns_per_byte` y, con él, `k7.boot_guest_ms` y `k7.boot_host_s`, porque cerca de la mitad de las salidas del invitado por byte eran lecturas de estado; y `k7.tx_serial_records`, porque la ventana de una transacción con una publicación queda en la nota `WORK_OPENED`. La escritura en el emulador hace además una llamada al sistema por byte en el anfitrión, que este cambio no toca, así que la reducción por byte será menor que la mitad. Las cifras las mide el cierre de K7; aquí no hay ninguna.

**Alternativas descartadas.** `rep outsb`: bajo KVM la instrucción de cadena pasa por el emulador de instrucciones, y una salida por iteración no ahorra nada que la ráfaga no ahorre ya. Un driver por interrupciones con cola: el plano tiene que sobrevivir a un pánico, y [ADR-010](ADR-010-k6-parameters-and-wake-policy.md) ya rechazó amortiguar registros para volcarlos después; aquí cada registro se escribe entero antes de volver. Quitar el `\r` de cada línea: cambia los bytes que las puertas reciben a cambio de un byte por registro. Callar las notas por transacción con `TRACE_OFF`: lo llevan paquetes K5 cuyas puertas las leen.

**Consecuencias.** Un puerto sin FIFO confirmada escribe como antes. Las puertas K1–K6 reciben los mismos registros con el mismo formato y, en `diag.summary`, un campo más. La imagen de medida dice menos por transacción, y lo que calla no lo lee ninguna puerta que corra sobre ella; el total de efectos auditados sigue en el log.

**Revisión.** Si una puerta empieza a leer el serial de la imagen de medida, o si una UART real sin FIFO fiable confirma los bits de `IIR`.

**Referencias.** [Observabilidad](../architecture/observability.md), [ADR-010](ADR-010-k6-parameters-and-wake-policy.md), [EXP-13](../evidence/exp13-k1-final.md), [estado actual](../roadmap/current-state.md).
