---
id: ARC-008
kind: contract
status: designed
---
# Estado, publicación y recuperación

## Dominio transaccional elegido

El kernel no persiste procesos ni capacidades. Un **servicio de estado administrado**, en un dominio separado, es el único escritor del almacén. Expone objetos inmutables por digest, workspaces privados y una raíz publicada versionada. La versión inicial serializa publicaciones de un almacén completo; subdividir raíces es una evolución que necesita declarar qué deja de ser atómico.

La raíz referencia contenido, manifiestos, política y recibo semántico de publicación. Un grant persistente es una regla de autorización que se reevalúa al arrancar, no un handle de kernel guardado en disco. Una nueva versión de módulo no hereda accidentalmente la autorización de otra.

Los clientes nunca reciben acceso escribible directo a bloques del almacén ni a una versión publicada. Un adaptador de archivos puede ofrecer paths, pero resuelve operaciones contra una raíz/capacidad concreta. El servidor de nombres no introduce un namespace global privilegiado.

## Identidades

| Identidad | Composición | Uso |
|---|---|---|
| Almacén | UUID persistente y época de formato/recuperación administrativa. | No confundir una copia restaurada con continuidad monotónica no interrumpida. |
| Contenido | Algoritmo y digest de bytes canónicos, longitud y tipo. | Deduplicación e integridad probabilística; no autorización. |
| Publicación | Almacén + generación de 64 bits estrictamente creciente. | CAS y prevención de ABA aun si se repite contenido. |
| Petición | Namespace de principal autorizado + secuencia de 64 bits. | Dedupe durable y consulta tras perder respuesta. |
| Validación | Versión de entradas + herramienta/configuración + cobertura + resultado. | Afirmación limitada sobre esas entradas. |

No hay wrap: se rechaza agotamiento y se requiere migración explícita de época. El principal persistente lo resuelve autorización, no un Domain ID de un arranque anterior.

## Contenido canónico y nombres

Se elige SHA-256 como algoritmo inicial identificado en cada formato. El digest cubre prefijo de dominio/formato, tipo, longitud y representación canónica; no se usa el mismo espacio sin etiqueta para datos crudos, árboles y recibos. El servidor valida referencias y tipos antes de aceptar una raíz. Deducir igualdad a partir de un hash presupone resistencia a colisiones; el hash no autentica al autor.

Los tipos lógicos iniciales son bytes, árbol, manifiesto, política y recibo. Un árbol ordena entradas por bytes de nombre, prohíbe nombres duplicados, NUL, separadores y componentes especiales, y referencia contenido por digest. El bit ejecutable y los metadatos que alteren interpretación forman parte de la versión. No se normaliza Unicode de forma implícita.

Paths solo existen en el adaptador de archivos: se resuelven desde una raíz autorizada y no pueden escapar mediante componentes o enlaces. V0 puede rechazar symlinks/hardlinks no soportados; no puede seguirlos fuera de la raíz ni anunciar compatibilidad POSIX completa. Timestamps de compatibilidad son metadatos explícitos, nunca el criterio de identidad.

Un digest conocido no autoriza leer su contenido. La capacidad de endpoint lleva una faceta que delimita raíces/objetos y operaciones; el servicio comprueba accesibilidad desde ellas. La deduplicación física no mezcla los permisos de dos consumidores sobre bytes iguales.

## API de servicio V0

- `read(version, object, range)`: entrega bytes identificados y una retención acotada.
- `fork(version)`: crea workspace privado con presupuesto; no cambia la raíz.
- `write(workspace, change)`: modifica solo estado privado, con generación local.
- `freeze(workspace)`: produce una raíz inmutable candidata; no publica.
- `publish(expected_generation, candidate, policy, validation, request)`: solicita una transición condicionada.
- `result(request)`: devuelve estado durable autorizado o incertidumbre explícita.
- `discard(workspace)`: libera estado privado cuando terminen lectores y operaciones pendientes.

La petición identifica todos los inputs que afectan el significado. Repetir una identidad con otro digest de petición es `CONFLICT`. Un usuario que puede preparar contenido no adquiere por eso derecho a publicar.

## Admisión de publicación y serialización

El servicio primero valida formato, objetos, evidencia requerida y presupuesto. Obtiene el turno de publicación sin mantener un lock que bloquee respuestas de dispositivos o recuperación. Comprueba generación esperada y política activa; una aprobación humana anterior solo sirve si sigue referida al objeto y versión correctos.

Después llama al kernel para marcar en la invocación su **admisión de efecto**, comprobando otra vez vida del grant/ámbito. Esa transición y una barrera concurrente se ordenan. Si gana la barrera, no comienza la publicación. Si gana la admisión, queda un ticket pendiente que puede completarse con la reserva de cierre del servicio.

Esta operación de kernel no entiende CAS ni discos: registra que un servidor recibió autoridad todavía viva para comenzar un efecto de esa invocación. El servidor es parte del TCB para respetar el resultado. La autoridad para publicar está en la faceta de la capacidad de endpoint y su registro de política en el servicio, no en texto de la petición.

El turno serializa el protocolo hasta resultado durable o fallo que obligue a recuperación. Otros lectores pueden seguir usando la raíz anterior. Un servidor caído no entrega el turno a otro escritor independiente sin recuperar primero. V0 elige un escritor único porque simplifica la prueba; su throughput es una limitación medible.

## Protocolo durable inicial

Se elige un log append-only de registros enmarcados y objetos inmutables, sobre un driver de bloque con flush explícito. Cada registro tiene versión, tipo, longitud acotada, secuencia, checksum/digest y vínculo con el registro precedente. El formato de bytes se fijará con fixtures al implementar almacenamiento; la semántica ya queda fijada.

1. Reservar espacio para objetos, metadatos, registros de cierre y recuperación. Verificar CAS y admisión de efecto.
2. Escribir `PREPARE` con identidad/digest de petición, generación esperada, candidato y política; hacer flush. A partir de aquí recuperación sabe que existe una petición aceptada.
3. Escribir los objetos y dependencias faltantes de la nueva raíz; hacer flush de todas sus dependencias.
4. Escribir un `COMMIT` que incluya generación siguiente, raíz, política y recibo, referenciando el `PREPARE`; hacer flush.
5. Publicar la nueva raíz en la memoria del servicio y responder `COMMITTED_DURABLE`.

Si falla antes de commit, se registra `ABORT` durable cuando sea posible. Si se pierde comunicación con el disco durante un flush, el servicio no continúa publicando sobre un estado que supone conocido: entra en recuperación. Un commit completo puede haber llegado al medio aunque el flush no haya respondido.

Los lectores vivos solo ven la nueva raíz después de confirmación durable. La recuperación puede encontrar un commit válido cuya respuesta nunca llegó. Por ello el cliente no interpreta un timeout como aborto y consulta `result`.

## Recuperación e idempotencia

Se reconstruye el prefijo válido del log y el último commit cuyas dependencias estén completas. Un `PREPARE` sin commit se resuelve como aborto y se registra antes de volver a admitir publicaciones. Un sufijo roto por caída se ignora/trunca bajo reglas del formato; corrupción en un prefijo que se suponía durable provoca fallo de integridad, no salto arbitrario a un registro posterior.

Cada principal mantiene una secuencia estricta y como máximo una publicación pendiente en V0. Se conserva un high-water mark durable de peticiones resueltas, además de resultados recientes. Secuencias por debajo de ese límite nunca se ejecutan como nuevas: si se expulsó el resultado, se devuelve `RESULT_EXPIRED`, que no afirma commit ni aborto. Una secuencia siguiente solo se acepta cuando la anterior se resolvió. Un checkpoint preserva esos límites aunque elimine detalles antiguos.

Antes de hacer durable `PREPARE`, una caída puede no dejar rastro; no hay efecto publicado y reintentar la misma secuencia es seguro. Después, recuperación conserva identidad y resultado. Este coste de serialización por principal se acepta inicialmente; ventanas de múltiples peticiones necesitan un protocolo adicional de dedupe, no una caché volátil.

La raíz recuperada, la política y el recibo proceden del mismo commit. Un índice SQLite, grafo o memoria de conocimiento se reconstruye o comprueba contra esa raíz; su propia transacción no se confunde con la transacción del almacén.

## Espacio, retención y recolección

No se promete historial infinito. Las raíces retenidas y lectores tienen cuotas y vencimientos de servicio. La primera versión RAM sirve para probar semántica; la primera versión durable usa log y rechaza admisión al alcanzar su reserva mínima.

La recolección inicial es una compactación de mantenimiento, no un GC concurrente sofisticado: detener nuevas publicaciones, drenar I/O, copiar objetos alcanzables y high-water marks a una segunda arena, escribir checkpoint y flush, cambiar uno de dos superblocks alternos con generación/checksum y flush. Solo entonces se puede reutilizar la arena anterior. Ambos superblocks válidos seleccionan la generación más reciente completa.

Los clientes usan identidades lógicas, nunca offsets físicos. Se drenan lecturas del índice anterior antes de liberar sus extents. La capacidad debe reservar espacio para una copia completa de lo retenido; si no cabe, se solicita liberar retenciones o ampliar almacenamiento, no se borra historia necesaria para recuperar.

## Efectos fuera del almacén

Descartar un workspace revierte exclusivamente cambios privados administrados. Restaurar después de publicar es una nueva publicación CAS con autorización y evidencia. Un envío de red, un mensaje remoto o una acción física no forman parte de esa atomicidad.

El patrón outbox registra una intención de envío en el mismo commit local; un broker la entrega usando identidad idempotente si el remoto la soporta. Puede haber reintentos, duplicados o resultado desconocido. Un ledger local o Raft no produce exactly-once sobre un remoto arbitrario.

## Hipótesis de durabilidad

El protocolo presupone un dispositivo/stack que cumple flush y un modelo de detección de registros incompletos. Checksums detectan daños con probabilidad elevada; no protegen frente a un driver malicioso que fabrique datos y hashes. Cifrado/autenticación y protección contra rollback físico requieren claves y anclaje externos todavía fuera de V0.

Se deben inyectar cortes en cada escritura, flush y respuesta. Los modelos finitos incluidos estudian el orden abstracto; no prueban el comportamiento de una controladora ni una implementación futura. [SQLite](../research/sources.md) aporta experiencia de protocolos reales, no una garantía automática por usar la palabra «journal».
