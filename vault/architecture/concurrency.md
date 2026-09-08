---
id: ARC-010
kind: contract
status: designed
---
# Concurrencia, linealización y fallos

## Puntos que fijan el significado

| Operación | Punto de linealización | Trabajo posterior permitido |
|---|---|---|
| Derivar capacidad | Instalación del hijo mientras padre y ámbito siguen abiertos. | Publicar respuesta; ningún hijo escapa del linaje. |
| Admitir IPC | Validación conjunta y registro de invocación/reservas. | Entrega, trabajo y respuesta; el cierre puede cancelar o drenar. |
| Admitir efecto de servidor | Marca del ticket validando de nuevo grant y vida. | Finalizar el protocolo aceptado con reserva de cierre. |
| Poner barrera | Cambio monotónico de estado visible para todas las admisiones posteriores. | Recorrer, detener, desmapear, drenar y reclamar. |
| Sellar memoria | Todos los alias de escritura y DMA retirados y confirmados. | Lecturas y nuevos mapas de solo lectura. |
| Transferir capacidades | Instalación completa del conjunto y consumo de movimientos. | Entregar la respuesta; sin estados parciales accesibles. |
| Publicar raíz durable | Commit completo según log; durante ejecución normal se expone tras flush confirmado. | Responder, recoger versiones y resolver reintentos. |
| Retirar recursos | Último acceso peligroso resuelto y cuenta transferida/liberada. | Reutilizar identificadores solo con generación distinta. |

La recuperación puede determinar que un commit llegó al medio aunque la ejecución viva no recibiera confirmación. Eso no contradice la linealización de publicación: la operación pudo haber ocurrido en la ventana de resultado desconocido.

## Estrategia inicial de locks

K1 puede usar un núcleo y un lock global corto para metadatos. Antes de SMP se documenta un orden de locks por rangos y se comprueba en debug. No se adopta lock-free para parecer escalable.

El orden inicial es: metadatos de admisión/árboles → cuentas de recursos de ancestro a descendiente → objetos por ID creciente → colas locales. La reclamación separa el marcado lógico del recorrido físico. Ninguna espera a usuario, disco, otro servidor o acuse de TLB ocurre manteniendo esos locks.

Desmapear publica trabajo de shootdown bajo sincronización, suelta los locks y espera confirmación. El handler remoto confirma sin adquirir locks que el iniciador conserve. Las páginas pasan a una lista de retiro y no vuelven al allocator hasta completar el periodo de seguridad.

La publicación de una raíz usa un turno de transacción de usuario que puede abarcar I/O; no es un spinlock kernel ni un mutex que el driver o el worker de respuesta necesiten. Lectores conservan raíz anterior; el receptor de completions y la recuperación tienen ejecución independiente.

## Interleavings obligatorios

| Carrera | Resultado permitido |
|---|---|
| Derivación vs barrera del padre | Hijo anterior también cercado, o derivación rechazada. Nunca un hijo vivo independiente. |
| IPC vs expiración | Admisión anterior identificada, o error de vencimiento. |
| Recepción vs muerte del emisor | Mensaje aún no recibido cancelado, o ticket retenido por receptor con origen muerto. |
| CAS vs publicación rival | Una gana; la otra recibe conflicto de generación. |
| Barrera vs admisión de publicación | Solo una publicación con admisión de efecto anterior puede finalizar. |
| Desmapear vs acceso en otro núcleo | La página no se reutiliza hasta confirmar retirada de traducciones. |
| Cancelación vs respuesta | Una resolución local de invocación, con resultado semántico consultable si hubo efecto durable. |
| Reset de driver vs DMA | No se reutilizan buffers mientras no exista prueba del protocolo de quiescencia. |
| GC vs lectores/commit | Se retiene la versión/arena o se drena antes de reclamar; jamás un root con dependencias libres. |

## Errores y memoria insuficiente

Las reservas se adquieren antes de publicar objetos. Un fallo de asignación revierte reservas todavía privadas, no intenta reparar una estructura parcialmente visible. Los caminos de fallo tienen espacio preasignado para un resultado pequeño.

La muerte de un cliente no produce recursión ilimitada en una interrupción. El trabajo de limpieza se procesa por lotes. Una retención persistente aparece en `drain_status`, con responsable y recurso, para no convertir un leak en un falso éxito de cancelación.

Una excepción del kernel es un fallo del sistema, no un error ordinario de usuario. Panic registra información mínima dentro de límites, detiene otros núcleos cuando sea posible y reinicia o entra en diagnóstico. No continúa sobre invariantes de memoria desconocidos.

## Qué se debe demostrar antes de optimizar

Las pruebas de estados finitos validan reglas abstractas de carrera, no orden de memoria ni implementaciones de locks. Se requieren pruebas de SMP, fault injection y revisión de cada bloque `unsafe` que publique/reclame objetos. Herramientas de comprobación de concurrencia pueden aplicarse a componentes aislados, pero una compilación sin warnings no prueba ausencia de races.

La especificación x86-TSO consultada excluye, entre otras cosas, cambios de tablas de páginas y ciertos accesos especiales. No se usa como excusa para inferir que TLB, MMIO o DMA siguen automáticamente el modelo de memoria ordinaria.
