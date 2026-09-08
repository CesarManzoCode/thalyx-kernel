---
id: ARC-002
kind: contract
status: designed
---
# Objetos, referencias y propiedad

## Tipos iniciales

| Objeto | Operaciones esenciales | Vida y cargo |
|---|---|---|
| `Domain` | Construir tabla y mapas, instalar supervisor, activar, terminar. | Patrocinador de memoria y metadatos explícito; una sola raíz de ámbito propietario. |
| `Thread` | Configurar registros, iniciar, bloquear, detener, recibir fallo. | Pertenece a un dominio; contexto efectivo propio o prestado durante una invocación. |
| `Scope` | Crear hijo, limitar, consultar, poner barrera, drenar, retirar. | Árbol de recursos sin ciclos, profundidad acotada; no se destruye mientras sostenga obligaciones. |
| `Grant` | Derivar, atenuar, poner barrera, consultar drenaje. | Nodo de restricciones compartido por copias; generación no reutilizada. |
| `MemoryObject` | Asignar páginas, mapear, desmapear, copiar, sellar. | Páginas patrocinadas una vez; mapas y tablas se cobran aparte. |
| `Endpoint` | Admitir petición, recibir, responder, cerrar. | Cola de capacidad fija; destinatario único por endpoint, varios workers permitidos. |
| `Invocation` / `WorkTicket` | Identificar trabajo admitido, devolver respuesta, mantener obligación asíncrona, resolver. | Estado creado por kernel; origen y linaje no editables por cliente. |
| `Signal` | Señalar y esperar bits acotados. | Notificación coalescente; no sustituye una cola de eventos sin pérdidas. |
| `Timer` | Programar vencimiento monotónico y notificación. | Cantidad y frecuencia sujetas a cuota. |
| `Interrupt` | Recibir y reconocer una interrupción asignada. | Capacidad emitida por autoridad de dispositivos. |
| `Device` / `DmaDomain` | Mapear MMIO autorizado y buffers, habilitar/quiescer. | Asignación exclusiva conforme a topología de aislamiento; páginas fijadas siguen cobradas. |
| `ControlLog` | Leer recibos de control y reconocer consumo. | Anillo acotado con capacidad de reserva; solo consumidores autorizados. |

No existe un tipo de kernel `Agent`, `Prompt`, `Transaction`, `File` o `KnowledgeGraph`. Un servicio puede implementar esos objetos detrás de endpoints.

## Creación sin privilegio ambiental

El primer supervisor recibe un manifiesto de capacidades de arranque. Crear un objeto exige un derecho de fábrica limitado por un ámbito y reservar sus recursos antes de hacerlo visible. No hay syscall «abrir cualquier recurso por nombre». El servicio de nombres entrega capacidades que ya posee; conocer un nombre o un digest no concede acceso.

La activación de un dominio es una transición única después de instalar mapas, canales de fallo, stack, límites y capacidades iniciales. Un dominio incompleto no ejecuta instrucciones de usuario. El supervisor no concede al hijo una capacidad de administrar al padre.

Las tablas locales usan handles de 64 bits: índice de 32 bits y generación de 32 bits, con cero inválido. Un slot cuya generación agotaría el espacio se retira permanentemente durante esa época. El kernel valida índice, generación, tipo y derechos. Los handles no se consideran secretos dentro de un dominio: código que comparte ese dominio comparte la frontera de autoridad.

Los objetos se identifican para diagnóstico mediante época de arranque y secuencia de 64 bits. Ante agotamiento se rechaza creación; nunca se permite wrap silencioso. Esas identidades no se aceptan como capacidades.

## Objetos de servicio sin nombres que concedan autoridad

Un endpoint puede servir muchos objetos de usuario. Dar al cliente permiso de enviar a ese endpoint no debe conceder acceso a todos ellos. Cada capacidad de envío puede incluir una **faceta**: un identificador opaco e inmutable de 64 bits, creado mediante el derecho administrativo `BIND` del propietario del endpoint.

El kernel entrega esa faceta en el encabezado autenticado de recepción. El servidor mantiene la relación `(época de endpoint, faceta) → objeto lógico, operaciones permitidas, versión de política`. Puede representar una raíz de solo lectura o el derecho de solicitar una publicación concreta. El kernel no interpreta esa tabla.

Copiar o derivar una capacidad de cliente conserva la faceta. El cliente no tiene `BIND` y no puede cambiarla proporcionando otro entero en el payload. Una faceta no se reutiliza para otro objeto en la misma época; al agotarse el espacio se rechaza creación. Reiniciar el servidor crea endpoint/época nuevos y requiere capacidades nuevas.

Crear una faceta distinta es una operación de administración del servicio, no una atenuación ordinaria: puede representar autoridad diferente y exige el derecho correspondiente. El servicio de autorización pide al propietario una faceta conforme a su política; no obtiene por defecto administración irrestricta del endpoint.

## Derechos por tipo

Todos los tipos distinguen inspección y cierre del handle. Derivar/transferir son permisos independientes; disponer de un objeto no implica administrarlo. Los nombres siguientes fijan operaciones, mientras que sus números se asignan en el esquema ABI de K2.

| Tipo | Derechos de uso | Derechos administrativos separados |
|---|---|---|
| Dominio/hilo | Esperar fallo/fin, consultar estado. | Instalar mapas/caps/registros durante construcción, activar, detener, establecer supervisor. |
| Ámbito | Ejecutar con contexto autorizado, consultar consumo, esperar cierre. | Crear hijos, cambiar techos, transferir patrocinio, poner barrera y retirar. |
| Grant | Usar/derivar su autoridad atenuada. | Poner barrera a ese linaje y consultar drenaje; no administrar ancestros ajenos. |
| Memoria | READ, WRITE, EXECUTE y MAP, sujetos a derechos máximos y W^X. | SEAL, retirar mapas administrados y aceptar/transferir patrocinio. |
| Endpoint | SEND/CALL o RECEIVE, más transferencia explícita de caps. | BIND, cambiar configuración de cola antes de activar, cerrar endpoint. |
| Invocación/ticket | Esperar/recibir resultado; responder o continuar según rol. | Admitir efecto una vez y resolver por el receptor/supervisor autorizado. |
| Señal/timer | Esperar o señalar; consultar vencimiento. | Armar/cancelar timer y vincular destino autorizado. |
| Dispositivo/DMA/IRQ | Usar únicamente rangos, colas, buffers e IRQ asignados. | Configurar aislamiento, habilitar bus mastering, resetear y liberar asignación. |

WRITE no implica READ por una regla global del kernel; el servicio de archivos puede definir esa relación en su propio contrato. READ de memoria tampoco implica el derecho a mapearla en cualquier dominio destino.

La plataforma limita combinaciones de mapas: con las tablas x86_64 V0, una página de usuario escribible o ejecutable también es legible. Por tanto, mapear WRITE o EXECUTE exige además READ; una capacidad sin READ no puede obtener ese mapa. Se rechaza la combinación insuficiente, sin añadir autoridad silenciosamente. Esto no impide una operación mediada de copia hacia memoria con WRITE que no revele sus bytes anteriores.

## Copiar, mover y prestar

Copiar una capacidad no copia el recurso ni crea un grant independiente: conserva el linaje de revocación. Derivarla crea restricciones adicionales, nunca elimina las previas. Transferirla entre dominios instala una entrada validada, no envía un entero en el payload. La publicación de mensaje y capacidades es todo o nada.

Mover una entrada invalida la del emisor solo cuando el kernel puede instalar el conjunto completo en el receptor. No se promete que mover una capacidad elimine otras copias legítimas existentes. La propiedad exclusiva de un buffer exige que el kernel haya comprobado/revocado todos sus alias relevantes o que se use una copia nueva.

Los tickets y respuestas son tipos distintos de capacidades generales. Una respuesta se consume una vez y está ligada a una invocación. Un ticket puede transferirse únicamente mediante una operación explícita que conserva origen, responsable de cierre y cargo; no se fabrica a partir de su identificador.

## Destrucción, ciclos y agotamiento

Cerrar el último handle no implica liberar inmediatamente un objeto: puede estar mapeado, en cola, activo en CPU o retenido por una obligación de servicio. La contabilidad distingue referencias de usuario, referencias internas y retenciones. Los grants y ámbitos muertos conservan tombstones hasta eliminar referencias dependientes.

El árbol de ámbitos y el árbol de grants son acíclicos por construcción. Los grafos de capacidades entre dominios pueden tener ciclos; no se depende de un recolector global para recuperar un ámbito. El propietario puede poner barrera a su subárbol, invalidar entradas y recorrer reclamación en pasos presupuestados. Las relaciones débiles de diagnóstico no prolongan vida.

V0 limita profundidad de ámbito a 16, profundidad de derivación a 32 y transferencia por IPC a 4 capacidades. Son límites de recursos y ABI consultables; no verdades arquitectónicas. Su cambio exige versión de interfaz o negociación explícita. Ante insuficiencia se responde un error preciso antes de efectos parciales.

La reclamación no recorre estructuras arbitrariamente grandes con interrupciones deshabilitadas. Un cierre devuelve un token de progreso y conserva los cargos de lo todavía retenido. [Autoridad](authority.md), [memoria](memory.md) y [recursos](resources.md) fijan cuándo esa retención deja de ser necesaria.
