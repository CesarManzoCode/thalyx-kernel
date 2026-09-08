---
id: ARC-003
kind: contract
status: designed
---
# Autoridad, delegación y revocación

## Modelo

Una capacidad referencia un objeto y un grant. El grant contiene derechos permitidos, vencimiento monotónico opcional, padre y ámbito de vida. Toda copia conserva ese linaje. Una derivación solo reduce derechos, adelanta vencimiento o añade restricciones. Se prohíbe derivar una vida independiente para escapar del padre.

El árbol de autoridad no se confunde con el árbol de recursos. Un servicio duradero puede recibir trabajo de varios ámbitos; mantiene capacidades propias y usa el contexto efectivo de la petición para cobrar su ejecución. Para admitir una operación deben estar vivos **tanto** la cadena de autoridad invocada como el ámbito efectivo de trabajo. Un grant acotado por otro ámbito agrega una condición de vida; no cambia quién paga.

Un derecho de delegación permite distribuir parte de la propia autoridad. No permite fabricar derechos, quitar expiraciones ni elevar límites. Los derechos de inspección, uso, derivación, administración y destrucción son distintos por tipo. El consumidor define qué acción humana autoriza concederlos.

## Punto de admisión

La admisión se linealiza bajo sincronización que incluye el estado del grant y del ámbito:

```text
admit(handle, operation, effective_scope):
    validate local slot, generation, type and operation
    validate every inherited grant restriction and scope fence
    check monotonic deadlines, queue capacity and resource reservations
    reserve required control receipt and outstanding-work record
    publish admission and its immutable origin
```

No se valida y después se encola sin protección contra una barrera concurrente. Una admisión que gana la carrera queda registrada antes de la barrera; una que la pierde falla. El reloj se comprueba dentro de ese punto. Las operaciones diferidas de servidor no vuelven verdadero un grant ya vencido.

El ámbito efectivo del pseudocódigo procede del contexto de hilo/ticket instalado por el kernel. No es un ID que el cliente pueda sustituir en el descriptor de syscall para cobrarle a otro ámbito.

## Tres resultados de revocar

1. **Barrera establecida:** no se admiten operaciones nuevas bajo el grant o ámbito cerrado. Incluye capacidades copiadas y derivadas.
2. **Drenaje concluido:** se detuvieron ejecuciones y accesos directos revocados y se resolvieron las obligaciones admitidas que pertenecen a ese cierre. Incluye confirmación de TLB y DMA cuando corresponda.
3. **Recursos retirados:** se liberó memoria y metadatos o se transfirió su patrocinio con autorización explícita.

El API separa `fence`, `drain_status` y `retire`. Un timeout de espera significa drenaje incompleto; no convierte el estado en éxito. La barrera es una operación breve. Recorrer descendientes, detener otros núcleos o esperar un dispositivo no ocurre dentro de su sección crítica.

Un vencimiento impide admisiones nuevas desde su deadline, aunque el handler del timer todavía no haya recorrido objetos. Los accesos directos ya mapeados y las instrucciones en otro núcleo requieren parada/desmap y confirmación posterior. No se afirma que cada load/store quede interrumpido exactamente en el nanosegundo de expiración; el estado y la evidencia distinguen deadline, barrera observada y drenaje.

Una capacidad revocada para mapear memoria también invalida los mapas cuya autoridad depende de ella. El kernel mantiene referencias inversas de esos mapas; no basta invalidar el handle. Una copia previa de los bytes en otra memoria autorizada no puede «desaprenderse». Revocar acceso no es borrar información ya revelada.

## Operaciones en curso y servicios

Una operación admitida antes de la barrera puede terminar después. El contrato no promete ausencia de todos los efectos posteriores a la hora de revocación. La garantía fuerte empieza **después del drenaje dentro del perímetro local declarado**.

Al cerrar un ámbito no se permiten nuevas llamadas ordinarias bajo su presupuesto. Los servicios cancelan las peticiones todavía abortables. Una publicación que ya cruzó su admisión de servicio puede necesitar finalizar un protocolo de disco: para ello el servicio reserva de antemano capacidad de cierre en su propio ámbito de recuperación. El ticket conserva origen y obligación pendiente; el coste de recuperación se registra en esa cuenta separada. Esta excepción de contabilidad no devuelve autoridad al cliente ni permite que el cliente cree nuevas peticiones.

Las operaciones internas de ese servicio usan sus propias capacidades y son parte de su TCB. El kernel no interpreta el payload para saber si escribir un bloque completa una publicación legítima. El servicio de estado debe registrar la admisión de publicación, antes de la barrera, mientras la invocación siga válida; si no lo hizo, aborta. [Persistencia](persistence.md) define ese punto adicional.

Una solicitud de revocación de publicación puede esperar a que todos esos tickets se resuelvan. Si el servidor cae, recuperación consulta el registro durable antes de informar el resultado. Si no puede resolverlo, informa `UNKNOWN` o drenaje pendiente según el caso; nunca inventa rollback.

## Confused deputy y canal humano

Los servidores reciben identidad de origen y ámbito estampados por kernel, separados del mensaje no confiable. Deben comprobar que la autoridad de la petición cubra la operación y el objeto de aplicación. No se permite que un nombre de tarea en JSON sustituya una capacidad.

La faceta inmutable de endpoint vincula la capacidad a la tabla de objetos autorizados del servicio. El broker valida el verbo solicitado dentro de esa faceta y su política activa. Conocer un digest, path o ID no selecciona por sí mismo otra autoridad. Se distinguen la administración que crea facetas y la derivación ordinaria que solo conserva/restringe una existente.

El broker sigue siendo confiable para el uso de sus propias capacidades: un servidor malicioso podría publicar con autoridad administrativa. Reducir esa autoridad, separar sus roles y auditar sus decisiones disminuye el radio de fallo; no constituye seguridad de flujo de información completa.

La ruta humana de aprobación vive fuera del canal de salida del agente, con recursos reservados. Firmar un módulo autentica un origen, no concede permiso. Una etiqueta de provenance no es una autorización. Ninguna regla se obtiene interpretando una respuesta generada como si viniera del usuario.

## Límites de seguridad

Se protege contra código arbitrario en dominios no confiables y contra uso de capacidades inexistentes, vencidas o excedidas. No se promete aislamiento temporal absoluto, ausencia de canales laterales de caché, defensa ante firmware hostil ni integridad frente a un servicio que pertenece al TCB de la propiedad atacada. La [matriz de TCB](../validation/invariants.md) explicita esas dependencias.
