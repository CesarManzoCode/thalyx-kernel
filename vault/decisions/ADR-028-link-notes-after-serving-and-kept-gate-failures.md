---
id: ADR-028
kind: decision
status: accepted
---
# El link escribe sus notas al terminar, cuenta su propio tiempo, y una puerta que falla conserva su corrida

**Problema.** La verificación de kiln al empezar K8 (`build/k8/check-20261030T113155`, kernel `0c866fc`, Thalyx `3ac575f`) mide `K1 − L1` en **+4,546 ms [+4,382, +6,011]**. En cada caso, `open` cuesta en K1 unos 4,4 ms más que en L1, y cada publicación de `settle` unos 2,5 a 2,9 ms más. En la misma verificación, el `check_k4.py` de kiln falló `outbox` (30 de 31) y el `check_k6.py` falló `no_loss` (23 de 24). Los registros de la corrida de K4 que falló ya no existen: el `check_k5.py` siguiente volvió a correr K1–K4 sobre el mismo `build/` y la puerta K4 pasó.

**Evidencia.** Observado en los 96 registros serie de K1 de esa medida, que guarda `exp13-serial/`:

- *Cada nota del link cuesta casi 2 ms dentro de la transacción.* Una nota es un registro de resumen que el kernel escribe por COM1 de forma síncrona, dentro de la llamada, y `--trace off` no la retiene. ADR-011 ya estimó unos 2,1 ms por registro bajo KVM. Aquí, entre dos notas del link que solo tienen entre sí una ida y vuelta del puerto de control (`stats`, `stats`, `shutdown`), pasan 1,886 ms de mediana (192 intervalos, entre 1,728 y 2,373 ms).
- *Hay dos en `open` y una por publicación de `settle`.* Desde la primera `PUBLISHED` (`0x5105`) hasta la nota de control que sigue a la transacción, en las seis rondas, los once casos sin publicación en `settle` tienen `PUBLISHED` del `seed` y `WORK_OPENED` (`0x510a`) del `fork`. Los cinco que publican tienen además una `PUBLISHED`, y `two-transactions-in-a-row` tiene cinco notas. ADR-011 dejó `WORK_OPENED` y la nota de control a propósito, para delimitar la ventana de `k7.tx_serial_records`. Además, la `PUBLISHED` del link no estaba en la cuenta de esa decisión. K8 no usa esa métrica.
- *Las cuentas cierran.* Dos notas son unos 3,6 a 3,8 ms de los 4,4 ms de `open`. Una nota es unos 1,8 ms de los 2,5 a 2,9 ms de cada publicación. El resto es lo que este árbol no sabe repartir todavía entre el puerto `virtio-serial`, K4 y `virtio-blk`. El `stats` del link no da tiempos.
- *Nadie más lee esas notas.* La puerta K5 lee las notas de `k5pub`, no las de `k5link`. El arnés de Thalyx no lee el registro serie («the host knows every path without parsing anything this prints», `tools/run_k1_link.py`). Solo `tx_windows` de `measure_k7.py` las usaba para contar registros por transacción, y `check_k7.py` lee evidencia fija.
- *K4 `outbox` es la falla que k7 vio como `IndexError`.* En `cut-after-checkpoint`, el registro del outbox no quedó `DELIVERED`. Desde `2eca0c8` la puerta dice qué estado encontró, pero kiln solo muestra la última línea del paso («1 of 31 criteria not met») y la corrida se sobrescribió. `resolve_outbox` de `k4store` llama al broker antes de responder la publicación, así que el cliente no puede terminar antes y descarto esa carrera. Que el broker salga tras cuatro errores seguidos de `endpoint_receive`, o que la llamada venza a los 2 s, queda sin decidir.
- *K6 `no_loss`.* En `kiln-k6`, ronda 1 nativa, `scale.ipc:3` rechazó 23 301 trozos con `LIMIT_EXHAUSTED` (`AUX`, cantidad 1, `b=0x5b050001000a`), justo después de `engine.infer:4`. El mismo −9 en `scale.ipc` aparece en seis campañas más desde `k7-check-20261028T151453`, después de `engine.infer`, de otro `scale.ipc`, de `closure.unit` o de `mem.seal`. ADR-020, ADR-023, ADR-024 y ADR-025 lo atacaron sin medir qué reserva rechazó, y todos piden medirlo.

**Elección.**

1. *Notas retenidas* (`user/k5link/src/notes.rs`). Las notas que el link hace mientras atiende (`PUBLISHED`, `WORK_OPENED`, `WORK_CLOSED`, `FENCE_SEEN_NS`, `STORE_REFUSED`, `REFUSED`, `CONTROL` y `SHUTDOWN`) se guardan en orden, hasta 64. Se escriben después de responder a `shutdown`, antes de `FRAMES`. La nota 65 escribe primero todas las retenidas y después a sí misma, así que el orden se conserva y solo el tiempo vuelve al camino. Cada nota lleva el valor que tenía al hacerse, y `FENCE_SEEN_NS` lleva su propio instante. `LINK_UNEXPECTED` y los informes del runtime se siguen escribiendo en el momento, porque son las que un huésped muerto debe dejar. Las notas no se escriben en un rato ocioso: una transacción que empezara justo entonces las pagaría.
2. *Contadores en `stats`.* El link responde también `frames_in` y `frames_out` (tramas completas recibidas y respuestas enviadas), `serve_ns` (tiempo dentro de `Managed::serve` en los puertos de trabajo, desde la trama completa hasta la respuesta lista), `store_ns` (tiempo bloqueado en llamadas a K4, `Store::call`), `publish_ns` (la parte de `store_ns` en `PUBLISH`, donde se escribe el medio y se hacen los flushes) y `notes_deferred`. Thalyx los pasa tal cual a `observation.kernel`, y el checker de K8 los imprime. Con eso, la ronda siguiente separa el tiempo del huésped (`serve_ns`) del transporte (`whole − serve`) y K4 del resto del link.
3. *Una puerta que falla conserva su corrida* (`tools/run_gates.py`). Debajo de un paso `kN-gate` fallido se imprime cada criterio que falló, con su detalle. Si el veredicto es anterior al paso o no es JSON válido, se imprimen las últimas líneas del registro. Las entradas de cada puerta fallida (`run*`, `k4-cases`, `k5-runs`, `k5-cases`), su veredicto y `gate-logs` se copian a `build/gate-failures/<hora>-<plataforma>/`, sin los `OVMF_VARS`, y se guardan las ocho últimas. Ningún criterio, límite ni espera cambia.

**Autocomprobación.** Ningún criterio de ninguna puerta lee las notas del link, así que K1–K5 no deberían cambiar. Lo que se puede contradecir son las cifras. Con la siguiente medida, `notes_deferred` por caso debería ser al menos 2 en los once casos, 3 en los cinco que publican y 5 en `two-transactions-in-a-row`, y `open` de K1 debería bajar unos 3,7 ms.

**Diseñado y no medido.** Por la regla del mini sprint no se corrió ninguna puerta, prueba ni medida. Solo se compiló `thalyx-user-k5link` (`cargo build --release` para `x86_64-unknown-none`) y se formateó. Esto es lo que se espera, en cifras:

- `open`: de +4,42 ms a cerca de +0,7 ms en los 16 casos (−3,6 a −3,8 ms).
- `settle`: −1,8 ms en los cinco casos que publican, que quedarían entre +0,7 y +1,1 ms. Nada en los demás.
- Extremo a extremo: unos −3,7 ms en los once casos, −5,5 ms en los cinco y −9 ms en `two-transactions-in-a-row`. `k8.tx_e2e` bajaría de +4,5 ms a algo entre +0,5 y +1,1 ms. **No** cruzaría cero.

**Alternativas descartadas.**

- *Callar las notas en la imagen de medida, como `quiet-io` (ADR-011):* pierde lo que dicen. Retenerlas cuesta lo mismo en el camino y conserva el contenido en todas las imágenes.
- *Escribirlas cuando el link queda ocioso:* la primera petición de una transacción que llega en ese momento esperaría hasta 64 × 1,9 ms.
- *Hacer asíncrono el plano de diagnóstico del kernel:* cambia el orden que leen todas las puertas, y no hace falta para lo que cuesta aquí.
- *Subir el presupuesto del auditor de K6, o que una admisión espere celda en lugar de rechazar:* lo primero es subir un límite sin saber la causa. Lo segundo cambia el contrato del perfil auditado. Ninguno se elige sin medir qué reserva rechaza.

**Consecuencias.** Cada caso termina unos milisegundos más tarde, porque las notas retenidas salen después de `shutdown`. El arnés de Thalyx espera hasta 20 s a que el huésped salga. En el registro serie, las notas del link ya no caen entre los registros de los demás dominios en el momento en que ocurrieron. Si el huésped muere a mitad de una transacción, esas notas se pierden. `tx_windows` de `measure_k7.py` ya no delimita una transacción, y K8 no lo usa. La K6 `no_loss` y la causa de la K4 `outbox` siguen abiertas.

**Revisión.** Si `open` de K1 no baja al menos 3 ms, la nota no era el costo, y hay que leer `serve_ns` contra la fase. Si baja y `tx_e2e` sigue arriba de cero, el reparto que dan `serve_ns`, `store_ns` y `publish_ns` elige el siguiente cruce: `virtio-serial` si `whole − serve` domina, y H3 o H4 de RES-003 si domina `publish_ns`. Si `outbox` o una puerta K5 vuelve a fallar, la causa está en `build/gate-failures/` y en la salida de kiln, y se lee antes de cambiar nada.

**Referencias.** [ADR-011](ADR-011-serial-bursts-and-measurement-notes.md), [ADR-020](ADR-020-auditor-settles-after-handoff-and-scope-walks-out-of-line.md), [ADR-024](ADR-024-log-reads-shared-auditor-settles-until-dispatched-probe-spread.md), [ADR-027](ADR-027-faults-after-their-withdrawal-and-reply-closes-ticket.md), [RES-003](../research/k8-transactions.md), [Observabilidad](../architecture/observability.md), [estado actual](../roadmap/current-state.md).
