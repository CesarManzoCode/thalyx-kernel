---
id: VAL-002
kind: plan
status: planned
---
# Experimentos y criterios de evidencia

## Obligaciones de implementación

EXP-01 está ejecutado; EXP-02, EXP-03, EXP-04, EXP-05 y EXP-06 lo están **en su alcance K3**, que es multiprocesador con un dispositivo virtio moderno y sin estado durable, sobre un emulador y **sin unidad de remapeo programada**. El resto está **pendiente**. Los modelos de diseño que ya se pueden ejecutar tienen nombres MODEL-01/02 y se reportan por separado.

| ID | Experimento | Criterio de corrección | Fase |
|---|---|---|---|
| EXP-01 | Arranque y dos dominios adversarios; faults y FP. | Primer ring 3 después de protección; probes ilegales fallan sin dañar otro dominio; temporizador preempta un loop. | K1–K2. **Ejecutado**: [K1](../evidence/k1-protected-boot.md) y [K2](../evidence/k2-objects-authority-work.md). |
| EXP-02 | Caps: derivar, mover, copiar, expirar, reciclar slots y reiniciar. | Ninguna autoridad amplificada ni handle antiguo reinterpretado; operaciones cerradas rechazadas. | K2–K3. **Parte K3 ejecutada**, con cuatro procesadores y llamadas concurrentes en vuelo; sin reinicio con handles persistidos: [K2](../evidence/k2-objects-authority-work.md), [K3](../evidence/k3-smp-devices.md). |
| EXP-03 | Cierre concurrente con RPC, servidor muerto y operaciones recibidas. | Barrera separada de drenaje, contadores no falsamente cero, resultado posterior admitido solo cuando corresponde. | K2–K4. **Parte K3 ejecutada**, con llamantes en procesadores distintos; sin servidor muerto ni estado durable: [K2](../evidence/k2-objects-authority-work.md), [K3](../evidence/k3-smp-devices.md). |
| EXP-04 | Fan-out, CPU SMP, memoria, tickets y mantenimiento. | Conservación de cargos y presupuesto agregado, deuda visible, recuperación conserva sus reservas. | K2–K3. **Parte K3 ejecutada**: reserva agregada antes del despacho en cuatro procesadores, techo que rechaza, deuda arrastrada y exceso de ejecución medido contra la latencia de preempción real. Sin hardware físico: [K3](../evidence/k3-smp-devices.md). |
| EXP-05 | Alias y TLB remotos, DMA tardío, reset, zeroing y estado FP. | No reutilización peligrosa; sello solo después de retirada; perfil DMA honesto. | K3. **Ejecutado salvo el DMA tardío**: invalidación acusada por los cuatro procesadores, sello publicado contra un escritor vivo, reclamación diferida por generación, estado FP que viaja con la migración, reset confirmado leyendo el transporte, y perfil débil declarado con el fuerte rechazado. **Nada impide físicamente una escritura del dispositivo en esta plataforma**: [K3](../evidence/k3-smp-devices.md). |
| EXP-06 | IPC malformado y agotamiento de colas/handles/logs. | Sin efectos parciales de transferencia, sin allocation ilimitado y cierre disponible. | K2–K3. **Parte K3 ejecutada**, con cuatro procesadores y varios llamantes; sin varios servidores concurrentes: [K2](../evidence/k2-objects-authority-work.md), [K3](../evidence/k3-smp-devices.md). |
| EXP-07 | Versiones, herramientas que cambian inputs, CAS y ABA. | Validación ligada a versión; conflicto correcto; no publicación con permiso de escritura privada. | K4 |
| EXP-08 | Caídas en cada escritura/flush/respuesta, reintentos y compactación. | ACK recuperable; raíz/política/recibo coherentes; sin doble efecto por retry ni liberación temprana. | K4 |
| EXP-09 | Evidencia incompleta, remoto incierto, saturación y servicio de control perdido. | No falsa auditabilidad, rollback ni causalidad; diagnóstico nombra el límite. | K4–K5 |
| EXP-10 | Thalyx completo en una vertical nativa con motor y herramienta reales. | Misma semántica esperada y fallos honestos; residencia/consumo medidos, ninguna dependencia oculta en host. | K5 |
| EXP-11 | Backends Linux y nativo con perfiles equivalentes. | Rechazo del perfil insuficiente y fixtures de contrato compartidos; diferencias registradas. | K5–K6 |
| EXP-12 | Rendimiento y escalabilidad con controles emparejados. | Muestras, condiciones e intervalos; ninguna conclusión de velocidad sin equivalencia. | K6 |

## Cómo producir un resultado útil

Cada informe identifica pregunta, hipótesis rival, commits de kernel/consumidor, build, hardware o emulador, configuración, inputs y seed. Incluye comando, salida cruda o artefacto con hash, observación, interpretación y límites.

Los resultados posibles son `PASS`, `FAIL`, `NOT_RUN`, `INCONCLUSIVE` y `UNSUPPORTED_PROFILE`. Un ensayo que no pudo ejecutarse no pasa. Un assert ausente en un smoke test no prueba una propiedad adversaria.

Las pruebas de recursos deben incluir gasto posterior al cierre y el del servicio, no solo duración del cliente. Las de persistencia deben distinguir caída del proceso, caída de la VM y pérdida de energía/dispositivo. Las de aislamiento deben incluir vías alternativas de acceso.

## Modelos ejecutables incluidos

MODEL-01 explora un espacio finito de delegación, dos invocaciones, admisión de efecto, barrera y resolución. Busca deliberadamente el contraejemplo a «nunca hay commit después de fence», a la vez que comprueba que no se admiten efectos nuevos después de la barrera y no se retira un ámbito con tickets pendientes.

La variante rival que reutiliza un check anterior sin sincronizar admisión debe producir la secuencia check → fence → admisión indebida. Así se contrasta el mecanismo conjunto con una separación tentadora pero incorrecta.

MODEL-02 enumera cortes y subconjuntos de escrituras persistidas de un protocolo abstracto de publicación. Comprueba dependencias antes de commit y supervivencia de un ACK; variantes incorrectas deben producir contraejemplos. Incluye una demostración mínima de ABA en comparación por contenido.

No incluyen kernel Rust, instrucciones CPU, hardware, checksums reales, múltiples almacenes, toda la lógica de dedupe o un disco real. Su valor es detectar contradicciones del contrato temprano, no certificar implementación. [Código y resultados](../../research/models/README.md).

## Qué refutaría la arquitectura práctica

Coste de IPC que domina workloads relevantes; crecimiento no acotado de metadatos; limpieza que depende sistemáticamente de matar servicios compartidos; port de herramientas que requiere entregar autoridad ambiental a todo el sistema; imposibilidad de reproducir perfiles entre backends; una propiedad esencial que no encuentra mecanismo bajo fallos.

Esos resultados obligan a revisar el mecanismo o la frontera afectados. No se esconden mediante un benchmark de otra carga ni se interpreta la ambición del proyecto como evidencia.
