---
id: VOC-001
kind: contract
status: designed
---
# Vocabulario normativo

Los nombres ingleses identifican tipos y estados futuros; la explicación fija su significado.

| Término | Significado y exclusión |
|---|---|
| `Domain` / dominio | Espacio de direcciones y tabla de capacidades; frontera de protección. No equivale a tarea semántica ni usuario humano. |
| `Thread` / hilo | Contexto de registros ejecutable dentro de un dominio. Tiene ámbito de ejecución efectivo y estado de planificación. |
| `Scope` / ámbito | Identidad local de trabajo, presupuesto y cierre jerárquico. No es una transacción ni una lista de permisos humanos. |
| Tarea | Objeto de Thalyx que expresa propósito, entradas, plan y resultado. Puede crear varios ámbitos y transacciones. |
| Capacidad | Entrada validada por el kernel que referencia un objeto, derechos y una cadena de restricciones. Poseer un entero con el mismo valor en otro dominio no la reproduce. |
| `Grant` | Nodo de delegación y revocación asociado a capacidades. Conserva restricciones heredadas. |
| `Handle` | Índice y generación en una tabla local. No es una credencial exportable por red ni identidad durable. |
| Faceta de endpoint | Etiqueta inmutable de una capacidad, creada por la administración del endpoint y entregada por kernel. El servidor la vincula a objeto y operaciones de usuario. Un ID del payload no la reemplaza. |
| Admisión | Instante linealizable en que se aceptan autoridad, recursos y vida de una operación. No equivale a terminarla. |
| `Invocation` | Petición admitida con identidad, emisor real, ámbito efectivo, linaje y reserva. Existe hasta resolución y liberación. |
| `WorkTicket` | Referencia controlada a trabajo admitido que sigue pendiente en un servicio. Conserva cargo, linaje y obligación de cierre. |
| `Fence` | Barrera lógica que impide nuevas admisiones bajo una autoridad o ámbito. No borra efectos aceptados antes. |
| `Quiescent` | Estado sin ejecuciones, invocaciones, accesos revocados ni DMA pendientes dentro del alcance declarado del cierre. |
| `Retired` | Ámbito drenado cuyos recursos retenidos se liberaron o transfirieron explícitamente a otro patrocinador autorizado. |
| Versión de estado | Raíz lógica inmutable del servicio de estado. No es contador de escrituras en memoria ni timestamp de archivos. |
| Generación de publicación | Contador monotónico dentro de una época de un almacén; distingue volver a publicar el mismo contenido. |
| Digest | Identidad probabilística de contenido bajo un algoritmo especificado; no demuestra autorización ni durabilidad. |
| Witness | Evidencia de qué entradas se observaron y con qué cobertura. Un witness incompleto no prueba igualdad. |
| Publicación | Cambio serializado de raíz visible y política asociada. Su contrato durable se define en el servicio de estado. |
| Durabilidad | Supervivencia a fallos dentro de las hipótesis declaradas del almacenamiento y su protocolo de flush. |
| Cancelación | Solicitud y transición para detener trabajo. No significa revertir toda acción histórica. |
| Rollback | Descartar una versión privada no publicada. Después de publicar, restaurar exige una nueva publicación autorizada. |
| Efecto externo | Acción fuera del dominio transaccional local: envío de red, actuador o servicio remoto, entre otros. |
| Provenance | Relaciones observadas entre entradas, operaciones y resultados; no prueba de intención ni de corrección semántica. |
| TCB | Componentes cuyo fallo puede violar una propiedad especificada. Hay un TCB por propiedad, no una etiqueta única para todo el sistema. |
| Perfil | Conjunto explícito de garantías disponibles y requisitos de plataforma. Un perfil debilitado no satisface en silencio uno estricto. |
| Época de arranque | Identidad fresca que evita confundir objetos e invocaciones de distintos arranques. No ordena eventos remotos. |

El kernel usa tiempo monotónico local para vencimientos. Fechas humanas y expiraciones remotas requieren conversión y política externas. Una fecha de pared no se compara directamente con un contador de CPU.
