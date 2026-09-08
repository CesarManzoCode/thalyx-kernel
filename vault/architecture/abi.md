---
id: ARC-012
kind: contract
status: designed
---
# ABI y protocolos

## Tres fronteras distintas

| Frontera | Contenido | Independencia |
|---|---|---|
| Kernel ABI | Objetos, derechos, llamadas, memoria, recursos y errores de protección. | No depende de tipos Rust, CBOR ni nombres de Thalyx. |
| Protocolos de plataforma | Estado versionado, autorización, launch, archivos de compatibilidad y dispositivos. | Implementables en Thalyx-Kernel y Linux con perfiles declarados. |
| API semántica de Thalyx | `contexto`, `hacer`, `evidencia`, módulos, conocimiento y tareas. | Permanece propiedad del consumidor. |

El actual `thalyx-abi` es un protocolo de módulos sobre canales y descriptores Linux. Su nombre no lo convierte en el ABI de este kernel. Se conserva su semántica donde convenga mediante un transporte distinto.

## Contrato binario V0

Los valores tienen tamaño fijo, little-endian en la plataforma inicial, padding explícito y campos reservados a cero. No cruzan la frontera referencias Rust, enums sin representación fijada, trait objects, `usize` ni excepciones de lenguaje. Los tamaños, alignment y overflow se comprueban antes de usar datos.

Se propone una entrada `invoke` con operación dirigida a handle. En x86_64: RAX identifica la entrada, RDI el handle, RSI la operación, RDX el puntero a descriptor, R10 su longitud, R8 flags y R9 deadline monotónico; RAX devuelve estado y RDX un valor auxiliar definido por operación. RCX/R11 se consideran destruidos por la instrucción syscall. Los registros restantes siguen el contrato publicado del stub, nunca el de una función Rust interna.

Una operación de consulta de versión usa un identificador reservado, sin autoridad sobre objetos. El descriptor común ocupa 32 bytes: major u16, minor u16, opcode u32, flags u32, total_len u32, cookie u64 y reservado u64. La cookie del cliente sirve para correlación, no autorización ni dedupe durable.

V0 admite descriptores de hasta 4 KiB y payload inline de IPC de hasta 256 bytes. El kernel copia una vez los metadatos para validarlos; descriptores mutables no se reinterpretan durante la ejecución. La tabla numérica completa de opcodes se fija al implementar K2 mediante un esquema único que genera bindings C/Rust y fixtures. No se anuncia compatibilidad con números todavía no asignados.

## Familias de operaciones requeridas

| Familia | Contrato mínimo |
|---|---|
| Consulta | Versión mayor/menor, features y límites efectivos. |
| Capacidades | Derivar, transferir, cerrar, vincular faceta bajo derecho BIND, barrera de grant y consulta de drenaje. |
| Ejecución | Crear/activar dominio, hilos, canal de fallo y terminación. |
| Ámbitos | Crear, reservar/configurar límites, consultar, barrera, drenar, retirar. |
| Memoria | Crear, mapear, desmapear, copiar, sellar y consultar estado. |
| IPC | Admitir, recibir, responder, esperar, ticket, admisión de efecto y resolución. |
| Eventos | Señales, timers y lectura de control. |
| Dispositivo | Asignar recursos autorizados, interrupciones y dominio DMA. |

Los buffers de salida se validan y fijan durante la copia/instalación que debe ser atómica, no durante una espera de duración arbitraria. La recepción conserva el mensaje si no puede preparar esa entrega. Las respuestas pendientes pueden consultarse mediante su invocación; un fallo tardío copiando una respuesta no deshace un efecto del servicio.

El binding no transforma automáticamente «no recibí resultado» en «no ocurrió». Los errores distinguen fase: rechazo antes de admisión, admisión pendiente, fallo de entrega o resultado del servicio. El identificador de invocación permanece recuperable mediante el objeto de llamada mientras su ámbito lo retenga.

## Errores y evolución

Errores base: handle inválido, tipo incorrecto, derechos insuficientes, vencido, ámbito cerrado, límite agotado, cola llena, dirección inválida, versión incompatible, peer muerto, cancelación y operación pendiente. Servicios agregan conflictos de versión, resultado desconocido y resultado expirado sin reutilizar ambiguamente un código kernel.

Una versión mayor incompatible se rechaza antes de efectos. Campos desconocidos no se ignoran si podrían debilitar protección. Extensiones menores requieren negociación de feature; el receptor devuelve capacidades soportadas. En V0 la implementación puede romper ABI entre revisiones identificadas, pero nunca aceptar silenciosamente un descriptor con otro significado.

El formato de disco se versiona aparte. Migrar datos no prolonga handles, leases de reloj ni tickets de una época anterior. Un adaptador de compatibilidad debe declarar qué garantías puede sostener y rechazar el perfil estricto cuando no pueda.

## Compatibilidad decidida

No se implementa el ABI de syscalls Linux como API central. La primera compatibilidad es de **fuente**, con una biblioteca de plataforma y servicios que emulan el subconjunto de archivos, tiempo, threads y procesos que requieran las herramientas seleccionadas.

Un ELF enlazado estáticamente con musl para Linux sigue dependiendo de Linux. Portar Rust std, C/C++, QuickJS, SQLite y herramientas requiere trabajo explícito. Una VM Linux futura puede ejecutar legado, pero no constituye por sí sola Thalyx funcionando nativamente sobre el kernel nuevo.
