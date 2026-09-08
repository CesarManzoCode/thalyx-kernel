---
id: CON-001
kind: constitution
status: accepted
---
# Constitución técnica

## Qué debe existir

Thalyx-Kernel debe permitir construir un sistema donde un agente reciba autoridad delimitada para realizar trabajo, consuma recursos atribuibles, produzca resultados sobre entradas identificables y publique efectos bajo reglas verificables. El agente no adquiere autoridad por producir lenguaje convincente, ejecutar código o estar dentro de un proceso privilegiado.

Un agente amplifica un problema antiguo: programas que actúan por delegación, encadenan servicios y fallan. Lo específico de Thalyx es la combinación de trabajo especulativo, herramientas heterogéneas, permisos temporales, conocimiento derivado y una separación deliberada entre conversación humana y ejecución. Ninguno de esos requisitos justifica por sí solo un modelo dentro del kernel.

La unidad semántica es el trabajo que Thalyx comprende. La unidad de protección es el dominio. La unidad de consumo y cancelación es el ámbito. La unidad de publicación es una versión de estado. Confundirlas produce autoridad ambiental, recursos sin dueño o promesas falsas de rollback.

## Por qué un proyecto separado

Thalyx ya puede ofrecer gran parte de su semántica sobre Linux. Un kernel nuevo no es una condición lógica para que existan agentes, transacciones o capacidades. Es una investigación de ingeniería: construir una base cuyo contrato haga explícitas las relaciones que Thalyx hoy compone entre cgroups, descriptores, namespaces, LSM, procesos, snapshots y brokers.

La apuesta es que una frontera menor y más directa permita **razonar y comprobar mejor** la autoridad, la vida del trabajo y el estado observado, y que pueda tener costes aceptables. Ni una superioridad de rendimiento ni una reducción total del TCB están demostradas. Rehacer controladores, runtime, herramientas y recuperación tiene un coste enorme y debe permanecer visible.

Linux seguirá siendo una ruta real de Thalyx. Se admite mejorar esa ruta, incluso con mecanismos que eliminen una ventaja supuesta del kernel nuevo. La comparación debe sobrevivir al mejor argumento del rival.

## Propiedades fundacionales

| Propiedad | Compromiso arquitectónico |
|---|---|
| Autoridad explícita | Referencias no falsificables, derechos atenuables, delegación trazable y validación en la admisión. |
| Aislamiento de código arbitrario | Espacios de direcciones separados; control de páginas, interrupciones y DMA dentro de fronteras declaradas. |
| Trabajo acotado | Ámbitos jerárquicos con límites de recursos y estados de cierre observables. |
| Estado identificable | Entradas inmutables o incertidumbre explícita; ninguna caché convierte observación incompleta en hecho. |
| Publicación defendible | Compare-and-swap sobre una raíz versionada, autorización y evidencia de publicación en una unidad durable. |
| Fallos comprensibles | Cancelado, drenado, publicado, durable y resultado desconocido son estados distintos. |
| Evidencia limitada pero honesta | Origen y orden observados con cobertura y pérdidas explícitas; ninguna pretensión de inferir intención. |
| Frontera reutilizable | El kernel no contiene conceptos de prompts, modelos, módulos de Thalyx o grafos semánticos. |
| Recuperación independiente | Recursos y autoridad reservados para detener trabajo y recuperar el sistema sin cooperación del agente. |

Cada compromiso se desarrolla en [los invariantes](validation/invariants.md). Los límites no son notas al margen: forman parte de la propiedad.

## Qué entra en el kernel

Un mecanismo entra si exige arbitrar un recurso físico o una frontera de protección frente a código no confiable, o si su corrección no puede delegarse sin devolver autoridad irrestricta al adversario. Eso incluye mapas de memoria, cambios de contexto, tablas de capacidades, admisión de IPC, cobro de trabajo ejecutado, interrupciones y aislamiento de dispositivos.

Una política permanece fuera cuando un servicio protegido puede imponerla: nombres, permisos humanos, firmas de paquetes, almacenamiento versionado, transacciones de aplicación, red, modelos, provenance semántica, selección de herramientas y compatibilidad POSIX. El kernel mantiene los vínculos necesarios para que esos servicios no tengan que adivinar quién solicita una operación.

«Microkernel» describe esa frontera; no es una fuente de autoridad intelectual. Su coste en IPC, copias, planificación y recuperación se medirá. No se promete que mover un controlador a usuario elimine automáticamente su acceso DMA o su papel en la integridad de los datos.

## Restricciones deliberadas

La primera plataforma es x86_64 en QEMU con UEFI, seguida por SMP y hardware físico acotado. El kernel se escribe principalmente en Rust `no_std`, con ensamblador y `unsafe` auditables. No se exige un compilador de lenguaje seguro para ejecutar consumidores no confiables: el hardware proporciona la frontera.

No se adoptan inicialmente POSIX como ABI de kernel, compatibilidad binaria Linux, persistencia transparente de procesos, coherencia distribuida en ring 0, planificación aprendida, GPU como requisito de arranque ni replay determinista de programas nativos generales. Estas exclusiones tienen [decisiones registradas](decisions/README.md), no prohibiciones eternas.

No hay un criterio de «el proyecto debe ganar un benchmark o dejar de existir». Sí hay obligaciones de corrección antes de atribuir una garantía y pruebas comparables antes de afirmar una mejora.

## Qué significa haber terminado la fundación

Debe poder iniciarse implementación sin volver a decidir qué significa revocar, quién paga una petición, qué hace durable una publicación o dónde acaba la autoridad del kernel. Los detalles que requieren medidas o hardware se mantienen abiertos con una decisión provisional utilizable. Este vault cumple esa función; no sustituye una implementación, una revisión adversaria externa ni una demostración formal.
