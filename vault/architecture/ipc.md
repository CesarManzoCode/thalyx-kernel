---
id: ARC-005
kind: contract
status: designed
---
# IPC: control, datos y trabajo delegado

## Contrato básico

V0 ofrece `call`, `receive`, `reply`, envío asíncrono acotado y señales coalescentes. Un endpoint tiene propietario, capacidad de cola y política de admisión establecida por autoridad, no por cada mensaje. No hay broadcast ilimitado.

El fast path copia hasta 256 bytes y transfiere hasta cuatro capacidades. Un payload mayor usa un objeto de memoria validado con offset y longitud, preferentemente sellado. Estos números son parámetros iniciales consultables, sujetos a medición; todo overflow se rechaza. No se parsean CBOR, JSON, nombres de archivo o contratos de Thalyx en el kernel.

La admisión reserva conjuntamente bytes de cola, registro de invocación, espacio para capacidades del receptor y recursos de control requeridos. Si falta algo, no se transfiere ninguna capacidad ni se consume parcialmente el mensaje. El emisor puede esperar con deadline o recibir `WOULD_BLOCK`; no se asigna memoria ilimitada para representar una espera.

## Identidad y respuesta

El receptor obtiene un encabezado que el kernel construye: época, identificador de invocación, dominio emisor, ámbito efectivo de origen, padre causal local, derechos transferidos y estado de cancelación. El payload separado puede contener una identidad de tarea de aplicación, pero no sobreescribe ese encabezado.

El encabezado incluye también la [faceta del endpoint](objects.md) y la referencia interna al grant usado en admisión. El servidor resuelve su objeto lógico desde esa faceta, no desde un ID pretendidamente autorizado en el payload. `begin_effect` vuelve a comprobar el mismo linaje; no admite sustituirlo por una capacidad nueva para revivir una petición cercada.

La respuesta utiliza un objeto de una sola consumición vinculado al emisor y a esa invocación. Responder dos veces es error. La muerte o el timeout del emisor no libera por adelantado memoria que el receptor siga usando.

Hay dos hechos distintos: «la llamada fue admitida» y «su respuesta llegó». Un timeout después de admisión no prueba ausencia de efecto. Las APIs de publicación usan identificador idempotente de aplicación y consulta de resultado, definidos en [persistencia](persistence.md).

## Trabajo y CPU

En una llamada síncrona, el cliente bloquea y el worker adopta el ámbito efectivo de la invocación. No se duplica presupuesto: todos los hilos que usen ese ámbito compiten contra sus mismos saldos y límite de paralelismo. El cambio de contexto preserva dominio y tabla de capacidades del servidor.

Un servidor puede retener una petición como `WorkTicket` acotado para I/O asíncrono. El kernel conserva el cargo de cola y el responsable de cierre; un worker se vincula explícitamente al ticket para trabajo atribuible. No basta copiar un ID a una cola privada y olvidar la obligación del kernel.

La memoria base del servidor y sus operaciones de mantenimiento tienen una cuenta propia. El kernel cobra tiempo ejecutado, reservas y metadatos que observa; no sabe qué proporción de una caché compartida beneficia a cada cliente. El servidor reporta esa política aparte, sin presentarla como medición exacta del kernel.

La recuperación después de una barrera puede usar la reserva de cierre del servicio, conservando el origen causal del ticket. Se registra como coste de recuperación y se limita por servicio; no es una ruta gratuita para continuar trabajo arbitrario del cliente.

## Orden, concurrencia y bloqueo

El endpoint conserva orden de admisión en su cola, pero varios workers pueden completar fuera de orden. No se promete FIFO entre endpoints ni orden global entre núcleos. Los clientes que necesiten serialización la expresan en el servicio correspondiente.

V0 limita anidamiento síncrono a ocho niveles y rechaza una cadena que vuelva a un dominio ya presente. Esto evita un tipo concreto de ciclo RPC; no detecta todos los deadlocks entre locks privados, tickets asíncronos y varias cadenas. Los servicios deben evitar llamadas bloqueantes con locks que necesite otro worker y mantener un grafo de dependencias documentado.

El emisor no recibe ejecución directa del servidor sin pasar por planificación y cobro. Handoff de IPC es una optimización posterior compatible con esos puntos. La prioridad de un cliente no eleva un servidor por encima de los techos autorizados.

## Colas y contrapresión

Las cuotas incluyen mensajes pendientes, tickets recibidos, capacidades en tránsito, memoria de payload y notificaciones de fallo. Aceptar recepción no libera el cargo hasta transferirlo de forma autorizada o terminar la invocación. El servicio no puede eludir una cola acotada acumulando tickets sin cuota.

Una señal guarda bits y secuencia, no un evento por cada ocurrencia. El receptor debe consultar el estado fuente y volver a armarla evitando la carrera consultar/dormir. Para eventos que no admiten pérdidas se usa una cola con reserva y número de secuencia.

El canal de cancelación y los recibos de liberación disponen de slots reservados independientes del tráfico ordinario. Saturar logs o colas no debe impedir cerrar autoridad. Se prueba específicamente la inversión donde un cliente espera a un servidor que espera a un log lleno.

## Datos no confiables

El kernel copia metadatos de usuario antes de validarlos; no valida un descriptor en memoria mutable para volver a leerlo después con significado distinto. Accesos a buffers de usuario pueden fallar y nunca provocan una lectura arbitraria de kernel. Longitudes, offsets, alineación y suma se comprueban con aritmética sin overflow.

Compartir un buffer mutable es un protocolo explícitamente débil: el receptor debe copiarlo o tolerar cambios concurrentes. Las peticiones que autorizan acciones destructivas requieren metadatos copiados y entradas selladas/versionadas. La seguridad no se apoya en que un cliente mantenga voluntariamente estable una página.
