---
id: PLAN-001
kind: plan
status: planned
---
# Desde cero hasta Thalyx real

## Estado inicial y disciplina

K0 entrega esta constitución. Todas las fases K1–K6 están pendientes. No se fijan calendarios ficticios sin un equipo, presupuesto y datos de implementación. Cada fase produce código utilizable y evidencia de su contrato; no exige resolver toda la investigación futura.

El orden protege dependencias: primero memoria/ejecución, después autoridad/trabajo, después concurrencia/dispositivos, después persistencia y consumidor. Trabajo de inventario/portabilidad puede avanzar mientras se desarrolla el kernel; no se exige esperar a K5 para descubrir necesidades de libc.

## K0 — Fundación

Entregables: vault, evidencia fijada de Thalyx, fuentes, decisiones, modelos limitados y auditoría. Resultado: arquitectura inicial defendible y siguiente trabajo concreto. No hay código de kernel.

## K1 — Arranque protegido

Crear workspace Rust con `boot/uefi`, `kernel`, `abi`, `user/init` y herramientas de imagen. Fijar toolchain, dependencias, comandos y build reproducible. Mantener código de arquitectura separado de objetos comunes sin inventar interfaces para hardware inexistente.

Implementar loader, ELF/bootinfo acotados, mapa físico, allocator, tablas, GDT/IDT/TSS, stacks de emergencia, timer, entrada/salida de usuario y contexto FP inicial. Ejecutar dos programas pequeños; uno intenta acceso ilegal y otro sigue progresando.

Evidencia: imagen arrancable en QEMU, logs con puntos de arranque, fallo de usuario contenido y preempción real. Un print desde ring 0 no completa K1. Los canales de supervisor completos pertenecen a K2; el mecanismo temporal de diagnóstico de K1 se marca como tal.

## K2 — Objetos, autoridad y trabajo

Implementar esquema ABI/bindings C y Rust, tabla generacional, dominios, grants y ámbitos. Reservas de memoria/metadatos, CPU UP, IPC copiado, tickets, señales, timers, barrera y drenaje. Añadir servicio supervisor con capacidades de arranque explícitas.

Primera vertical: cliente sin permisos ambientales solicita una operación a un servidor, deriva autoridad a otro dominio, consume presupuesto y se cancela mientras el servidor retiene trabajo. Comprobar qué termina y qué sigue pendiente. Implementar copia/sellado conservador antes de optimizar datos.

Evidencia: EXP-01–04/06 en alcance UP, trazas pequeñas con origen auténtico, errores de quota y resultados del cierre. No declarar SMP ni DMA.

## K3 — SMP y dispositivos

Implementar arranque de APs, planificación con presupuesto agregado, sincronización, TLB shootdowns y reclamación diferida. Probar límites con varios núcleos y servidores prestados. Controlar FP/SIMD y reloj entre núcleos.

Separar driver virtio-blk, IRQ y buffers; comenzar con perfil QEMU que declare sus dependencias. Añadir IOMMU y grupos de aislamiento para el perfil fuerte y plataforma física seleccionada. Driver reset/quiescencia tiene pruebas de DMA y RAM retenida.

Evidencia: EXP-02–06 concurrentes, no reutilización tras revocación incompleta y contabilidad conservada. El perfil sin IOMMU sigue siendo útil para desarrollo, pero no satisface aislamiento de drivers no confiables.

**Ejecutado**, salvo la unidad de remapeo. K3 implementa el arranque de APs, la planificación agregada, la invalidación entre núcleos con acuse, la reclamación diferida y el camino de dispositivo entero con reset y quiescencia; **no** programa la IOMMU y por tanto no habilita el perfil fuerte, que se rechaza con `UNSUPPORTED_PROFILE` en lugar de aproximarse. Esa parte necesita el inventario de hardware físico de [OQ-05](open-questions.md) y queda registrada en [ADR-009](../decisions/ADR-009-device-path-and-dma-profiles.md). [Qué se ejecutó exactamente](../evidence/k3-smp-devices.md).

## K4 — Estado administrado durable

Implementar primero el servicio RAM con versiones, workspace, validación de entradas y CAS. Después log, PREPARE/COMMIT/ABORT, flush, resultado por petición, high-water marks, cuotas y compactación en dos arenas. Diseñar fixtures de formato antes de escribir datos que se pretendan conservar.

Construir driver de fallos que corta/reordena escrituras, pierde completions y devuelve errores. Integrar admisión de efecto con barrera y reserva de cierre. Separar recibos de control de evidencia durable. Demostrar un efecto remoto incierto con broker de prueba.

Evidencia: EXP-07–09 con reinicios en cada frontera, no solo tests del parser. Un almacén RAM o rename en el host no cumple la fase durable.

**Ejecutado.** El formato se fijó antes de escribir nada, el servicio publica versiones inmutables con CAS sobre un medio real, y la matriz corta la ejecución en cada punto de escritura y arranca de nuevo sobre lo que el corte dejó. La puerta decide desde los registros del kernel y desde los bytes del medio, decodificados por el módulo que genera el esquema. Lo que **no** demuestra es durabilidad frente a un corte de energía: la supresión la hace el driver del invitado, no el emulador, y el perfil declara esa dependencia igual que el de DMA declara la suya. [Qué se ejecutó exactamente](../evidence/k4-durable-state.md).

## K5 — Port de Thalyx y herramientas

Inventariar dependencias y extraer interfaces en Thalyx manteniendo Linux. Portar runtime de usuario, transporte, filesystem administrado de compatibilidad y launch. Thalyx usa su API semántica sobre el backend nativo.

K5a demuestra una superficie con fixtures y agente externo; se etiqueta como integración parcial. K5b ejecuta programa acotado y herramienta real dentro del kernel. K5c incorpora motor CPU residente y la toolchain requerida por la carga de referencia, con todas sus dependencias de usuario documentadas.

La vertical completa es: obtener contexto de una versión, ejecutar cambios privados, validar con herramienta real, publicar o abandonar, consultar evidencia y sobrevivir a una caída en publicación. Incluir dos tareas rivales y cierre durante servicio residente.

Evidencia: EXP-10/11, misma semántica sobre Linux y nativo, costes de motor/petición separados, ningún comando del host confundido con ejecución nativa. El esfuerzo de std, libc, C++ y toolchain es parte central de K5.

**Ejecutado**, en su alcance. Las tres partes de la ruta corren sobre el mismo kernel: K5a como superficie sobre el estado administrado de K4 con la validación etiquetada como afirmación propia; K5b con QuickJS real ejecutando un programa que sale de la versión publicada y una herramienta nativa real decidiendo la publicación desde un dominio propio; K5c con llama.cpp entero como motor residente —la etiqueta que Thalyx fija, el `serve_one` de su propio motor, su propio modelo de prueba— sobre una libc escrita aquí y la biblioteca estándar de C++ del anfitrión enlazada, con residencia y petición cobradas por el kernel a ámbitos distintos, y las completaciones byte a byte las del motor de Thalyx en Linux. La vertical completa con dos rivales, cierre durante el servicio residente y cortes en publicación es la matriz de EXP-10; el perfil declarado, el rechazo del insuficiente y las diferencias con Linux son la parte K5 de EXP-11. Lo que **no** hay es `cargo` ni comprobación de tipos dentro del kernel, un modelo de trabajo, más de un hilo de cómputo, ni una ejecución de Thalyx sobre Linux aquí: el perfil de Linux es una lectura. [Qué se ejecutó exactamente](../evidence/k5-thalyx-port.md).

## K6 — Comparación, endurecimiento y primera referencia

Ejecutar protocolos emparejados, medir contención, IPC, memoria, colas, recuperación y cargas reales. Revisar los parámetros V0; mejorar solo los cuellos identificados. Completar hardware físico acotado, diagnóstico de fallo y documentación de operación.

Una primera referencia funcional requiere el recorrido K5 completo, SMP, estado durable probado, autoridad/cierre comprobados, perfiles declarados y la ruta Linux conservada. No requiere soportar cualquier dispositivo ni ganar todos los benchmarks.

**Ejecutado en su alcance**, que es KVM sobre la máquina de desarrollo. K6 corre diecisiete protocolos emparejados de una sola fuente sobre este kernel y sobre Linux como invitados de la misma máquina, en campañas de rondas con plan sorteado, y decide cada comparación con la equivalencia que el esquema declaró antes de medir; midió contención, IPC, memoria, colas, presupuesto, cierre y la carga real del motor; encontró ocho cuellos midiendo y arregló cada uno con su cifra de antes y después; revisó cinco parámetros V0 con medida y cambió cuatro ([ADR-010](../decisions/ADR-010-k6-parameters-and-wake-policy.md)); inventarió la máquina física bajo KVM sin arrancar el kernel sobre ella; y dejó una puerta de 19 criterios más las cinco regresiones, con las puertas K1–K5 verdes en TCG y en KVM. Lo que no hizo: mover el cerrojo único de la máquina, que es lo que impide que el IPC escale con los pares y está medido; arrancar en hardware físico; y el diagnóstico de fallo y la documentación de operación más allá de lo que cada puerta y cada nota registran. [Qué se midió exactamente](../evidence/k6-comparison-hardening.md).

## Primer paquete de implementación, listo para comenzar

Crear la rama de K1 y fijar un par de targets separado para loader/kernel. Construir imagen UEFI desechable con kernel ELF y dos binarios de usuario mínimos. Implementar una serie corta: bootinfo validado → memoria propia → traps/timer → ring 3 → fallo contenido y preempción. Registrar cada evidencia con commit e imagen.

Antes del primer unsafe, escribir su contrato; antes del primer handle externo, fijar layout ABI; antes del primer almacenamiento durable, fijar protocolo/fixtures. No se necesita rediseñar el sistema completo para iniciar ese paquete.

## Estructura de código prevista

```text
boot/uefi/          loader y contrato de arranque
kernel/            núcleo común y arch/x86_64
abi/               esquema binario y bindings generados
user/init/         supervisor inicial
user/services/     autorización, estado y brokers
user/drivers/      dispositivos aislados por perfil
runtime/           biblioteca de plataforma y compatibilidad
tests/system/      pruebas de imagen, fallos y consumidor
tools/             construcción, imagen y captura de evidencia
research/          modelos y experimentos exploratorios
vault/             contratos y conocimiento versionado
```

Esta estructura es una previsión; hoy solo existen vault, herramientas de revisión y modelos de investigación.
