---
id: ADR-021
kind: decision
status: accepted
---
# Una ranura de pila del kernel que se vuelve a mapear se invalida antes de escribir por ella, y su retiro se sella después de quitar las entradas

**Problema.** En la medida de K7 `build/k7/check-20261028T142453`, el brazo nativo de la ronda 1 de la campaña K6 (`build/k6/campaigns/k7-check-20261028T142453/r01-native`) terminó con un pánico del kernel después de 13 de 32 entradas: `kernel exception vector=14 error=0x10 cr2=0x0 rip=0x0 rsp=0xffff80800024af50 cs=0x8`. Un pánico así tumba la ronda entera, y con ella seis criterios de la puerta K6 (`ran`, `same_plan`, `kernel_record`, `trace_declared`, `paired` y `audit`): la puerta dio 18/24. Es el único `kernel.panic` en todas las campañas guardadas bajo `build/k6/campaigns/`.

**Evidencia.** Observado en el registro serie y en el código del árbol medido:

- `error=0x10` con `cr2=0` y `rip=0` en `cs=0x8` es una búsqueda de instrucción en la dirección cero, en modo kernel.
- `rsp` cae en la ranura 14 de pilas del kernel (`KSTACK_AREA` más 0x4af50, ranuras de cinco páginas), 0xb0 bytes bajo su tope. `prepare_user_thread` deja el primer contexto de un hilo en tope − 0xe8 (`StoppedContext` mide 232 bytes, lo afirma un `assert`). El cambio de contexto saca seis registros y hace `ret`, y eso deja `rsp` exactamente en tope − 0xb0. El procesador entró a un hilo **que nunca había corrido** y leyó cero donde `prepare_user_thread` había escrito `resume = thalyx_thread_start`.
- 61 ms antes, el registro muestra `thread.stopped_late` del hilo `k6idle` de un dominio parado por autoridad. Un dominio recolectado devuelve sus ranuras de pila (`release_kernel_stack`), y la creación siguiente las reutiliza (`allocate_kernel_stack` toma la primera libre).
- Las entradas del medio del kernel son globales: un cambio de `CR3` no las retira. `release_kernel_stack` hace `invlpg` solo en el procesador que libera, y las demás se ponen al día en su siguiente `tlb::refresh_local`. El procesador que despacha un hilo refresca antes de cambiar de pila (`sched.rs`, antes de `switch_space`). El que lo **crea** no refresca: escribe el primer contexto con la traducción que tenga.
- Si el procesador que crea conservaba la traducción de la ranura de su ocupante anterior, el contexto se escribe en el marco viejo, que está en cuarentena. El que despacha lee el marco nuevo, que `alloc` entrega en cero, y `ret` salta a cero.
- `release_kernel_stack` tomaba el sello de retiro (`retire_stamp_kernel`) *antes* de quitar las entradas. Un procesador que refrescara entre el sello y el `unmap` contaba como invalidado mientras las entradas seguían ahí para volver a cargarse.

**Elección.**

- `allocate_kernel_stack` hace `invlpg` de cada página justo después de mapearla, en el procesador que va a escribir por ella. Así no queda ninguna copia vieja de esa dirección en el procesador que escribe el primer contexto. Los demás procesadores ya refrescaban antes de correr el hilo.
- `release_kernel_stack` quita e invalida localmente todas las entradas de la ranura, y *después* toma el sello y pone los marcos en cuarentena con él. Un procesador que alcanza el sello lo hace después de que las entradas desaparecieron.

**Autocomprobación.** La puerta K6 lee cada ronda de los tres brazos: un pánico tumba `ran`, y con él los criterios que dependen de la ronda. La corrida que valida este cambio es la siguiente medida de K7.

**Diseñado y comprobado solo en parte.** La lectura del pánico como una traducción global vieja es una inferencia: encaja con el desplazamiento exacto del marco, con el orden del registro y con el código, pero ninguna traza registra el contenido del TLB. Ocurrió una vez en los 219 arranques nativos guardados bajo `build/k6/campaigns/`. Una campaña sin pánico no prueba que la carrera haya desaparecido. El defecto es anterior a K7: `release_kernel_stack` y `allocate_kernel_stack` no cambiaron en K7. Que K7 lo haya hecho visible, por el despacho, la recolección o el reparto del tiempo ocioso, es una hipótesis.

**Alternativas descartadas.**

- *Esperar en la asignación a que todos los procesadores pasen el sello de la ranura:* se asigna bajo el cerrojo de la máquina, y `tlb::wait_for` exige no tener ninguno.
- *Conservar mapeadas las páginas de una ranura libre para el siguiente hilo:* quita el problema de reutilizar direcciones, pero cambia cuántos marcos tiene el kernel con dominios muertos, que las puertas K2–K5 cuentan.
- *Escribir el primer contexto por el mapa directo del marco:* también evita la traducción vieja, pero solo en ese punto. El `invlpg` cubre cualquier escritura del procesador que crea.

**Consecuencias.** Cuatro `invlpg` más por hilo creado. Ningún parámetro, techo, interfaz ni registro cambia.

**Revisión.** Si vuelve a aparecer un `kernel.panic` con `rip=0` y `rsp` en tope − 0xb0 de una ranura, la causa no es la que este registro describe, o hay otra escritura por la ranura antes del despacho.

**Referencias.** [Memoria](../architecture/memory.md), [ADR-015](ADR-015-targeted-seal-wait-and-table-walks.md), [ADR-020](ADR-020-auditor-settles-after-handoff-and-scope-walks-out-of-line.md), [estado actual](../roadmap/current-state.md).
