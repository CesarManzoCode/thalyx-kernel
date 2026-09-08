---
id: ARC-013
kind: contract
status: designed
---
# Distribución y determinismo

## Frontera local

Las capacidades del kernel son locales a una época de una máquina. No se serializan handles para dar autoridad a otra. Un servicio de red autenticado traduce capacidades locales a una sesión y protocolo remoto con identidad, derechos, audiencia, época y límites propios.

La autoridad remota debe comprobarla el receptor. Una firma transporta una afirmación del emisor; no garantiza que siga autorizada ni que no haya sido revocada. Para revocación inmediata bajo partición se debe sacrificar disponibilidad o consultar una autoridad accesible. Leases solo acotan vida bajo hipótesis explícitas de reloj y renovación.

V0 tiene un servicio de estado local. Replicar su log, elegir líderes o distribuir una tarea son trabajos de usuario. Raft puede ordenar decisiones de un grupo; no ordena efectos de red arbitrarios ni garantiza exactly-once de clientes que reintentan sin dedupe.

## Fallos remotos

Los resultados distinguen rechazo, aceptación, commit conocido, fallo conocido y resultado desconocido. Un timeout puede deberse a pérdida de respuesta después de commit. Los protocolos necesitan identidad de petición, consulta de resultado y retención de dedupe.

Enviar una cancelación no prueba que el remoto la haya aplicado. Una barrera local solo asegura que este lado no admite trabajo nuevo bajo la autoridad cerrada y que drenó lo que controla. Ningún reloj local certifica quiescencia de otro sistema.

La provenance distribuida usa relaciones explícitas de mensajes y épocas. Timestamps sirven para diagnóstico y límites bajo condiciones declaradas; no se convierten en una historia causal total.

## Tres niveles de repetibilidad

| Nivel | Qué puede prometerse | Qué debe registrarse |
|---|---|---|
| Reejecución de trabajo | Mismas entradas y configuración pueden volver a ejecutarse. | Raíz, herramientas, modelo/pesos, parámetros, entorno y servicios externos. |
| Replay de un runtime restringido | Mismo resultado si se capturan/controlan todas las fuentes de no determinismo de ese runtime. | Orden de hostcalls, reloj virtual, RNG, respuestas y reglas de concurrencia. |
| Replay nativo general | No prometido en V0. | Requeriría tratar carreras, señales, time sources, I/O, SIMD/GPU y ejecución externa. |

Incluso una inferencia con semilla fija puede variar por algoritmo, backend numérico, paralelismo o hardware. Un recibo debe distinguir «mismos inputs» de «mismos bits de salida».

El kernel proporciona identidad de invocaciones, timers controlables por perfil, contabilidad y memoria sellada. No introduce una historia global de cada lectura/escritura para hacer determinista código nativo arbitrario. Un consumidor puede usar un runtime determinista acotado como servicio separado.

## Por qué no imponer determinismo universal

Determinación fuerte exige restringir comunicación y memoria compartida o registrar mucho no determinismo. La experiencia de [Determinator](../research/sources.md) muestra una alternativa concreta con costes y restricciones; no prueba que todos los toolchains y motores actuales encajen sin cambios.

Thalyx necesita saber qué validó y por qué confía en ello antes de necesitar reproducir cada instrucción. Se elige primero identidad de entradas y resultados honestos. Una futura modalidad determinista debe declarar sus restricciones y medirse contra esa necesidad, no convertirse en un requisito ceremonial de todo proceso.
