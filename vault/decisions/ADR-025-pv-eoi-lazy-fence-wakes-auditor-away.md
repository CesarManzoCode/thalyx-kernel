---
id: ADR-025
kind: decision
status: accepted
---
# Fin de interrupción paravirtual de KVM; los despertares de una valla esperan en el procesador que la puso; el lector del log se despierta lejos del escritor; el auditor de K6 duerme media ventana

**Problema.** La verificación de kiln al cierre de la ronda `k7-r02` (`build/k7/check-20261029T145738`, kernel en `ad1a267`) dejó en `FAIL` 24 criterios. Las puertas y los tests pasan. Esta decisión ataca cuatro grupos:

- `k7.k6_scale_compute_1` a `_4`. Las razones nativo / `linux-nomitig` miden entre 0,9944 y 0,9968. Los umbrales piden que el límite inferior pase de 0,99723, 0,99726, 0,99708 y 0,99546.
- `k7.k6_closure_unit_0`, con 0,411 [0,397, 0,426] contra un umbral de 0,266. Es una regresión: el baseline midió 0,278 y 0,290.
- `k7.k6_ipc_lineage_0`, `_16` y `_28`. Sus medianas pasan, pero una o dos rondas de seis corren unos 420 ns más lentas por llamada y suben el límite superior del intervalo.
- `k7.k6_scale_compute_4` además, que va 0,2 % por detrás de `_3`.

**Evidencia.** Observado en los artefactos de `build/k7/` y `build/k6/campaigns/`, y en el código de `ad1a267`:

- *El tick.* Cada procesador recibe un tick cada 500 µs (`TICK_HZ = 2000`, `timer.armed` en el registro serie). La CPU de K6 es `qemu64,+x2apic` bajo KVM en un Ryzen 5 5600G. En modo x2APIC, el `EOI` es una escritura a un MSR, y sin virtualización del APIC el anfitrión la intercepta. Linux, sobre la misma CPU virtual, registra el fin de interrupción paravirtual de KVM (`kvm_guest_apic_eoi_write`) cuando `CPUID 0x4000_0001` ofrece `KVM_FEATURE_PV_EOI`, y QEMU lo ofrece por defecto con KVM. El kernel no lo usaba. A 2 000 ticks por segundo por procesador, una salida más por tick es del orden de la distancia que mide `scale.compute`. No se midió cuánto cuesta una salida en este anfitrión.
- *El auditor ocioso.* Mientras nadie admite nada, el auditor de K6 despierta cuando vence su espera en el log (`AUDIT_SLEEP_NS`, 500 µs), mira el relevo, el estado del cliente y el reloj, y vuelve a dormir. Con cuatro hilos de cómputo en cuatro procesadores, cada despertar desplaza a uno de ellos. Que esto explique el 0,2 % de `scale.compute_4` es una hipótesis.
- *La valla.* En las campañas de `ad1a267`, la mediana nativa de `closure.unit` es de 16,7 a 18,7 µs, contra 12,2 a 13,2 en el baseline. El mínimo no cambió (9,2 contra 9,7 µs): lo que creció es la cola, entre 0 y 15 µs. `cancel_waits` despierta con `WakeHint::Any` al hilo bloqueado del dominio, y lo manda a un procesador ocioso. Desde [ADR-016](ADR-016-quiet-idle-and-one-sweep-retirement.md), ese procesador está parado y no sondeando, así que hay que interrumpirlo. El hilo corre y vuelve a esperar. La terminación, unos microsegundos después, lo puede encontrar en ese procesador, lo tiene que interrumpir otra vez y no se puede recoger hasta que el procesador lo deja. La primera muestra de cada ronda necesita ahora dos intentos de retiro en cuatro de seis rondas. ADR-016 ya anticipaba esto.
- *Las rondas lentas de IPC.* El estado actual las atribuye a la espera de [ADR-020](ADR-020-auditor-settles-after-handoff-and-scope-walks-out-of-line.md) después de un relevo: sin espera, `fx-o2` midió lo normal en 4 de 4. `fx-o4`, que hacía que `select_cpu` prefiriera un procesador que sondea antes que el propio, siguió lenta. El auditor duerme en el log y lo despierta la admisión número 16 desde su última lectura, por medio de `select_cpu`. Si corrió por última vez en el procesador del cliente y la cola de ese procesador está vacía, `select_cpu` lo vuelve a poner ahí: el cliente cede el procesador al auditor cada 16 llamadas, y como el auditor vuelve a correr ahí, también se le vuelve a despertar ahí. La espera de ADR-020 hace dormir al auditor ventana tras ventana, y lo despierta el tick de un procesador ocioso, que puede ser el del cliente bloqueado. Con los demás procesadores parados después de un tick sin trabajo, no hay ninguno que sondee, y eso explicaría por qué `fx-o4` no salió del modo lento. Es una hipótesis: no hay traza de dónde corrió el auditor en esas entradas.

**Elección.**

*Fin de interrupción paravirtual.* `arch::x86_64::kvm` registra en cada procesador, con `MSR_KVM_PV_EOI_EN`, una palabra de la imagen del kernel. Lo hace solo si la firma de `CPUID 0x4000_0000` es `KVMKVMKVM` y `0x4000_0001` ofrece la función. `lapic::end_of_interrupt` borra el bit 0 de la palabra con un `btr`. Si estaba puesto, no escribe el registro, y el anfitrión retira el vector en su siguiente salida. Si no, escribe `EOI` como antes. El BSP la registra al armar su timer y cada AP al armar el suyo. `timer.armed` y `smp.ap_online` dicen `pv_eoi=0|1`. Bajo TCG y en hardware no hay firma, y nada cambia.

*Despertares perezosos de la valla.* `WakeHint::Lazy` encola al hilo en el procesador que despierta y no interrumpe a nadie. `cancel_waits` lo usa. Un hilo así encolado y detenido por la terminación antes de correr no está en ningún procesador, y se recoge en la salida de la misma terminación. Uno que la valla no detiene corre cuando quien puso la valla se bloquea, cuando el tick ve que esperó un quantum detrás de él, o cuando un procesador ocioso lo roba. Se espera que `closure.unit` vuelva a su suelo, unos 9 a 10 µs.

*El lector del log, lejos del escritor.* `WakeHint::Away` es `Any` sin el paso de `select_cpu` que prefiere el procesador propio a un ocioso. Cualquier procesador ocioso, parado o no, va antes. `logops::wake_reader` lo usa. La primera vez cuesta una interrupción. Después, el procesador donde corrió el auditor sondea 200 µs, y los siguientes despertares, cada 16 admisiones, lo encuentran ocioso donde corrió por última vez.

*El auditor de K6 duerme media ventana.* `AUDIT_SLEEP_NS` pasa de 500 µs a 5 ms. Los recibos siguen terminando la espera de 16 en 16. Cambia solo el periodo de sus vueltas sin admisiones y lo que tarda en ver un relevo, y ninguna entrada mide ninguna de las dos cosas.

*Tamaño.* La carga del kernel sigue en 569 344 bytes, medida sobre el ELF compilado. Para lograrlo, `lapic::end_of_interrupt` es una función fuera de línea que llaman los cinco manejadores, y `enable_pv_eoi` también. `select_cpu` recibe un parámetro en lugar de tener una variante, así que sigue en línea dentro de `wake`, y las palabras de los procesadores comparten una línea (32 bytes de `.bss`). Al texto le sobran 17 bytes antes de su última página.

**Autocomprobación.** `pv_eoi=1` en `timer.armed` y en cada `smp.ap_online` de un arranque KVM dice que el mecanismo se activó. `pv_eoi=0` en TCG dice que no se activó donde no debía. La puerta K6 sigue leyendo `no_loss`, `audit` y cada error de cada entrada: si el lector del log tardara más en despertar, se vería ahí. La primera muestra de `closure.unit` sigue escribiendo cuántos intentos de retiro hicieron falta (`AUX`, cantidad 4); con este cambio deberían ser uno.

**Diseñado y no medido.** Por la regla del mini sprint, este cambio se escribió sin correr puertas ni campañas. Solo se compiló el workspace y se midió la carga del kernel. Las hipótesis son:

- que el anfitrión intercepte el `EOI` (sin AVIC, o con AVIC inhibido para x2APIC en Zen 3) y que la salida pese lo bastante para mover `scale.compute`;
- que `closure.unit` crezca por la interrupción y la expulsión que la valla provoca, y no por otra parte del retiro;
- que las rondas lentas de IPC sean el auditor pegado al procesador del cliente.

**Alternativas descartadas.**

- *Bajar el tick a 1 kHz:* el quantum de 1 ms quedaría en un solo tick. El exceso de un despacho que no entra al kernel subiría a 1 ms, y las tolerancias de `budget` de K2 y de `quota.share` de K6 se calibraron con 500 µs.
- *Un temporizador de un disparo:* quita más ticks, pero cambia la forma en que se cobra y se expropia, que es un contrato.
- *Revertir el sondeo tras un tick sin trabajo ([ADR-016](ADR-016-quiet-idle-and-one-sweep-retirement.md)):* arreglaría la valla por accidente, y la arreglaría solo a veces, cuando el procesador destino estuviera sondeando. Además se pierde lo que ADR-016 ganó en `scale.ipc`.
- *Que la valla no despierte a nadie:* un hilo de un ámbito con valla tiene que enterarse de que su espera se canceló. Lo que cambia es dónde y cuándo corre, no si se despierta.
- *`Away` para todos los despertares `Any`:* `sched.wake` y los despertares de K5 eligieron el procesador propio por una medida ([ADR-010](ADR-010-k6-parameters-and-wake-policy.md)). Solo el lector del log tiene trabajo propio que no es el de quien escribe.

**Consecuencias.** Un tick o una interrupción entre procesadores bajo KVM cuestan una salida menos. Un hilo despertado por una valla corre más tarde que antes, como mucho un quantum, salvo que quien puso la valla se bloquee antes. Un auditor puede costar una interrupción más la primera vez que se le despierta lejos del escritor. Un relevo de K6 puede tardar hasta 5 ms más en empezar.

**Revisión.** Si `scale.compute` no se mueve, hay que mirar si `pv_eoi=1` aparece en el registro. Si aparece, hay que contar las salidas del anfitrión por tick. Si `closure.unit` sigue lejos, hay que mirar los intentos de retiro de la primera muestra. Si las rondas lentas de IPC siguen, hace falta registrar en qué procesador corre el auditor durante esas entradas.

**Referencias.** [ADR-010](ADR-010-k6-parameters-and-wake-policy.md), [ADR-016](ADR-016-quiet-idle-and-one-sweep-retirement.md), [ADR-020](ADR-020-auditor-settles-after-handoff-and-scope-walks-out-of-line.md), [ADR-024](ADR-024-log-reads-shared-auditor-settles-until-dispatched-probe-spread.md), [Recursos](../architecture/resources.md), [estado actual](../roadmap/current-state.md). Fuentes del protocolo: en Linux, `arch/x86/kernel/kvm.c` (`kvm_guest_apic_eoi_write`, `kvm_guest_cpu_init`), `arch/x86/kvm/lapic.c` (`apic_sync_pv_eoi_to_guest`) y `Documentation/virt/kvm/x86/msr.rst` (`MSR_KVM_PV_EOI_EN`). Limitación: se citan sin una revisión fijada, y falta anclarlas a un commit.
