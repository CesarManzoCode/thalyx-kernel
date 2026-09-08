---
id: INT-002
kind: plan
status: planned
---
# Linux y comparación equivalente

## Dos rutas permanentes

Thalyx/Linux sigue siendo un sistema mantenido. Thalyx/Thalyx-Kernel se construye como backend independiente. No se elimina Linux al conseguir el primer arranque ni al superar una prueba. Mejorar un servicio compartido puede beneficiar a ambos y cuenta como aprendizaje válido.

La comparación final necesita una revisión identificada de Thalyx, dos backends y perfiles equivalentes. Cuando difieran los mecanismos de estado o aislamiento se registra esa diferencia y se comparan por separado las garantías.

## Tres preguntas experimentales

| Pregunta | Comparación válida | Qué no concluye |
|---|---|---|
| ¿Cuánto cuestan las primitivas? | IPC, mapas, grants, barreras y cuotas con cargas y garantías descritas. | Que un sistema completo sea más rápido. |
| ¿Qué cambia para Thalyx? | Misma superficie, tareas, modelo, herramientas, versiones de datos y política sobre ambos backends. | Que una diferencia de modelo o toolchain sea mérito del kernel. |
| ¿Qué aporta el perfil administrado? | Mismo protocolo de estado implementado también sobre Linux frente a la ruta mutable, separado del efecto kernel. | Que Linux carezca de snapshots o transacciones. |

El benchmark actual de Thalyx frente a herramientas generales de un agente investiga otra pregunta. Sus resultados no son evidencia de rendimiento de un kernel todavía inexistente.

## Diseño de medición

Fijar CPU, núcleos, RAM, límites, firmware, almacenamiento, aceleración VM y estado térmico cuando corresponda. Publicar imágenes, hashes, compiladores, flags, modelo/pesos, inputs y comandos. Un experimento de TCG no se usa para cuantificar un coste de context switch físico.

Ejecutar ensayos emparejados con orden aleatorizado, separando arranque frío, carga de pesos y ejecución caliente. Usar suficientes repeticiones para intervalos de confianza del efecto observado y conservar muestras crudas; el tamaño se decide mediante variación piloto, no un número ceremonial.

Medir latencia mediana y colas p95/p99, throughput, tiempo de CPU y pared, memoria física patrocinada, coste residente, bytes copiados, I/O, ocupación de colas, pérdidas y tiempo de drenaje. Los percentiles requieren tamaño muestral que los sostenga; no se publica p99 significativo de diez pruebas.

Incluir cargas sin agente: workers maliciosos que saturan CPU, mensajes, handles y memoria; lectores lentos; servidor muerto; revocación concurrente; fallos de flush y clientes que reintentan. La corrección no se infiere de una media de velocidad.

## Preguntas falsables

- Si ámbitos nativos no mejoran atribución o añaden costes altos a peticiones cortas, revisar metadatos y granularidad; no eliminar evidencia del resultado.
- Si IPC domina el tiempo de herramientas reales, medir batching, copias y servicio residente antes de mover política a kernel.
- Si el servicio de estado Linux sostiene exactamente el mismo contrato con costes comparables, atribuir el beneficio al protocolo, no al nuevo kernel.
- Si mantenimiento y recuperación consumen mucho más que el coste ordinario reportado, corregir la comparación incluyendo esas cuentas.
- Si la latencia de cierre depende de drivers no drenables, limitar el perfil y resolver el dispositivo; no seguir diciendo revocación inmediata.

Las pruebas de aceptación por fase comprueban que una garantía existe. Las comparaciones posteriores deciden qué mecanismo mejorar; no son una votación sobre si el proyecto tiene derecho a continuar.
