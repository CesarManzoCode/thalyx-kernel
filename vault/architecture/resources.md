---
id: ARC-007
kind: contract
status: designed
---
# Recursos y planificación

## Contabilidad por trabajo

Un ámbito es el principal de consumo; un dominio es la frontera de memoria. La distinción responde a servidores residentes y operaciones delegadas, no a una categoría especial de «agente». [Resource Containers y seL4 MCS](../research/sources.md) aportan evidencia de esa separación y de sus costes.

Los límites del hijo no sustituyen los del padre: cada reserva debe caber en todos los ancestros. Solo autoridad de recursos puede crear hijos, aumentar techos o transferir patrocinio. Crear muchos hijos no multiplica capacidad del padre.

| Recurso | Unidad y responsable | Agotamiento |
|---|---|---|
| Memoria física | Bytes/páginas reales, un patrocinador por página. | Rechazo de reserva; sin overcommit oculto. |
| Metadatos kernel | Slabs/páginas y número de objetos, mapas, handles y tickets. | Rechazo antes de hacer visible un objeto parcial. |
| CPU | Nanosegundos ejecutados, agregado del ámbito y ancestros. | No elegible hasta reposición; deuda de sobrepaso registrada. |
| Paralelismo | Número máximo de hilos simultáneos, incluidos workers prestados. | Espera planificada; sin presupuesto duplicado. |
| Colas | Mensajes, bytes y tickets pendientes. | Contrapresión o error explícito. |
| Persistencia | Bytes reservados, datos vivos, retención y espacio de cierre, en el servicio. | Rechazo de admisión antes de quedarse sin espacio para commit/recuperación. |
| Red y dispositivo | Solicitudes, bytes y buffers pendientes, en brokers/driver. | Colas acotadas; no confundir bytes solicitados con bytes físicamente enviados. |

Las métricas derivadas se etiquetan como estimaciones. El kernel no inventa energía por token, uso semántico de caché ni coste remoto exacto.

## Planificador V0

Se elige planificación preemptiva con selección justa jerárquica entre ámbitos elegibles y round-robin entre sus hilos. Los pesos y techos los fija autoridad de recursos. Un cliente no gana CPU creando más hilos. No se introduce un predictor aprendido en el mecanismo de protección.

La cuota inicial usa ventanas **fijas alineadas**, de periodo común P = 10 ms, y presupuesto Q en tiempo agregado de CPU. Q puede exceder P cuando el ámbito tiene paralelismo mayor que uno; siempre está limitado por el padre y los núcleos permitidos. El tamaño de quantum inicial es 1 ms, recortado por saldo y frontera de ventana. El ámbito raíz, y el de sistema bajo él, tienen Q = P por procesador en línea desde K6; hasta entonces la implementación les daba P a secas, y una máquina de cuatro procesadores admitía el trabajo de uno. [ADR-010](../decisions/ADR-010-k6-parameters-and-wake-policy.md).

Cuándo corre un hilo recién despertado es una política aparte, medida en K6: quien despierta suele estar a punto de ceder el procesador, y el despertar se delega a otro solo si no lo hace en un plazo de gracia de 10 µs. Es un parámetro V0 con su trade registrado en la misma decisión.

Para cada ventana W y ámbito S, el objetivo de cobro es:

```text
sum(execution_time of S and its descendants in W) <= Q(S) + measured_overrun(S, W)
```

No es una cota de ventana deslizante: dos ventanas permiten ráfagas cercanas a 2Q alrededor de su frontera. No se publicita como reserva de tiempo real ni garantía de latencia. Si Thalyx requiere una cota deslizante, la alternativa concreta es un servidor esporádico con refills acotados; se medirá antes de aceptar esa complejidad.

Antes de despachar, el kernel reserva saldo en todos los ancestros y un slot de paralelismo. Al desprogramar cobra tiempo real y devuelve reserva no utilizada. En SMP, la reserva impide que dos núcleos gasten el mismo saldo. El temporizador se arma al menor de quantum, saldo y fin de ventana.

Un retraso de interrupción o una sección no preemptible puede exceder la reserva. Se mide ese exceso y se descuenta de la siguiente reposición; nunca se borra al cambiar de ventana o migrar. No se afirma una cota física dura antes de acotar secciones críticas, firmware y hardware.

V0 mantiene cuentas globales sencillas con sincronización acotada. Fragmentación de cuentas por CPU y migración NUMA son optimizaciones posteriores; deben reconciliar reservas y deuda sin duplicar presupuesto.

## Servidores, inversión de prioridad y mantenimiento

Una llamada síncrona bloquea al cliente y presta el contexto efectivo al servidor. Un ticket asíncrono permite continuar trabajo con ese mismo ámbito, sujeto al límite agregado de paralelismo. El servidor conserva autoridad sobre su memoria; el préstamo no es una elevación de permisos.

El mantenimiento no atribuible exactamente se cobra al ámbito de servicio. Se reservan cuotas separadas para recibir cancelaciones, resolver fallos y finalizar protocolos ya admitidos. Ese trabajo conserva referencia causal al cliente cuando existe y aparece como gasto de recuperación, no como coste ordinario oculto.

No se permite mantener un lock de servicio y bloquear esperando un cliente cuya ejecución depende del mismo lock. La selección justa no cura ese deadlock. Los servicios críticos usan operaciones breves, colas acotadas y protocolos asíncronos para dispositivos.

## Recuperación bajo saturación

La configuración reserva una fracción explícita de CPU, memoria, handles y buffers para supervisor, control humano y cierre. No se concede esa reserva a tareas normales. La admisión de una operación que necesita cierre durable incluye una reserva en el servicio para terminar o abortar con el sistema lleno.

Interrupciones y trabajo kernel no atribuibles se cobran a una cuenta del sistema visible. La tasa de interrupciones se limita o enmascara según dispositivo. Un driver puede agotar su servicio, pero no debe consumir sin límite todas las estructuras de control.

No hay una garantía universal de progreso si hardware o un componente del TCB se bloquea. Sí hay un comportamiento definido: aislamiento del fallo cuando sea posible, retención de recursos peligrosos y diagnóstico de qué impide el cierre.

## Memoria de caché y costes compartidos

Los pesos compartidos se cobran al servicio residente; los buffers por inferencia al trabajo correspondiente; entradas de caché tienen patrocinador y política de expulsión. Una caché no puede crecer solo porque el cliente ya terminó.

Las métricas publican ambos ejes: coste incremental de la petición y coste residente amortizado mediante política declarada. Comparar solo el primero favorecería artificialmente servicios precargados; comparar solo RSS sumado contaría páginas compartidas varias veces.
