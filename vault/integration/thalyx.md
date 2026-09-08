---
id: INT-001
kind: contract
status: designed
---
# Frontera con Thalyx y ruta de portabilidad

## Responsabilidades

Thalyx conserva intención, planes, interfaz humana, contratos de módulos, autorización semántica, conocimiento, políticas de validación y la decisión de publicar. Thalyx-Kernel proporciona aislamiento, autoridad de objetos, vida del trabajo y recursos. Los servicios de plataforma proporcionan estado y operaciones de sistema sin codificar vocabulario de Thalyx.

El proyecto nuevo no impone a otros consumidores el modelo de módulos, una base de datos, un runtime JS o la superficie MCP de Thalyx. Otro consumidor puede usar los mismos ámbitos, capacidades y versiones con sus propios objetos semánticos. No se añade una API genérica sin un segundo uso concreto; se mantiene una frontera libre de nombres del primer consumidor.

## Adaptación conceptual

| Thalyx actual | Frontera propuesta | Qué debe preservarse |
|---|---|---|
| Módulo ejecutado en sandbox/cgroup | Dominio y ámbito, bundle de capacidades iniciales. | Ninguna instrucción no confiable antes de activar toda la protección. |
| Permiso persistente/JIT | Regla en autorización y grant temporal derivado. | Objeto, versión, propósito autorizado y vencimiento independientes. |
| Intento sobre workspace | Workspace privado, ámbito de trabajo y candidato congelado. | Abandonar no borra evidencia ni pisa estado ajeno publicado. |
| Witness de validación | Versión inmutable + toolchain/configuración + cobertura. | Datos incompletos no justifican reutilización. |
| Commit de módulo/current | Publicación CAS de contenido y política en una raíz. | No mezclar versión ejecutada con permisos de otra. |
| Journal | Historial semántico durable y recibos de control vinculados. | Intención, resultado e incertidumbre distinguibles. |
| Grafo y memoria | Servicios derivados de versiones, con política de frescura. | El índice no suplanta la fuente ni elimina «desconocido». |
| Motor residente | Dominio de servicio, pesos patrocinados y tickets por inferencia. | Cargar una vez cuando se afirma residencia; aislar buffers y límites. |
| `hacer`/QuickJS | Runtime de usuario que recibe capacidades por hostcall. | Límites, aserciones persistentes y composición de herramientas. |
| FD 3/CBOR de módulos | Transporte sobre endpoints y protocolo de usuario. | Mensajes acotados, derechos explícitos y errores comprensibles. |

## Portabilidad real, no un trait imaginario

`thalyx-syscall` concentra llamadas inseguras propias, pero no todas las dependencias de Linux: `std::fs`, `std::process`, sockets, libc/musl, SQLite, QuickJS, toolchain y bibliotecas del motor también tienen contratos de sistema. Un binario estático Linux no es un binario nativo de Thalyx-Kernel.

El inventario inicial de portabilidad debe clasificar cada dependencia por API usada, syscall efectiva, expectativa POSIX, estado TLS/FP, creación de hijos, memoria mapeada y tratamiento de señales. Solo después se extraen interfaces; no se esconde todo bajo un `Platform::do_anything`.

Interfaces de plataforma propuestas: `WorkControl`, `ObjectAuthority`, `VersionedState`, `ProgramLaunch`, `MessageTransport`, `MonotonicClock` y `EvidenceSink`. Cada una declara un perfil de garantías y errores. Un backend Linux puede implementar el mismo servicio versionado; un backend de archivos ordinarios declara cobertura inferior.

## Orden del port

1. Extraer tipos semánticos y serialización que no necesitan sistema; conservar pruebas de comportamiento sobre Linux.
2. Implementar cliente de protocolos nativos y supervisor mínimo. Ejecutar una vertical de capacidades/estado con fixtures; llamarla fixture, no Thalyx completo.
3. Portar runtime mínimo Rust/C de usuario: allocation, TLS, tiempo, threads, archivos administrados y transporte. Fijar un target propio y su contrato, sin fingir que `unknown-linux-musl` sirve.
4. Portar la superficie de Thalyx y QuickJS con hostcalls mediadas. La interfaz humana de recuperación puede comenzar en consola; no se comparte con salida de módulos.
5. Portar una toolchain real y el motor CPU residente. Revisar spawn, mmap, SIMD, biblioteca matemática, filesystem y dependencias de construcción; medir costes residentes y por trabajo.
6. Ejecutar `contexto → hacer → validar → publicar/abandonar → evidencia` con Thalyx real, fallos y reinicio. Documentar qué partes permanecen externas o todavía no soportadas.

Un agente externo por MCP puede conducir la vertical antes de que corra inferencia local. Es una integración útil, pero no prueba el motor residente nativo. Del mismo modo, un compilador ejecutado en el host no prueba confinamiento ni resource accounting del compilador dentro del kernel.

## Perfiles

`managed-local-v1` exige raíces inmutables, publicación durable CAS, grants y cierre definidos, recursos explícitos y ausencia de escritores externos al almacén. `mutable-files` puede usar paths y observaciones parciales; no satisface automáticamente esas garantías. `dma-isolated` agrega las condiciones de hardware. `audited-control` agrega captura con reserva de eventos cubiertos.

Las features son requisitos combinables, no un número que permita suponer que todo nivel superior incluye cualquier propiedad. El consumidor consulta y exige las que necesita. La respuesta nunca convierte un fallback en éxito de un perfil más fuerte.

## Cambio en Thalyx

Este trabajo no modifica el repositorio de Thalyx. Las futuras extracciones y backends se harán allí conservando su ruta Linux, y se fijarán revisiones de ambos repositorios en cada experimento. El diseño de Thalyx-Kernel no adquiere autoridad para reescribir incidentalmente decisiones de interfaz humana de Thalyx.
