---
id: GOV-001
kind: process
status: accepted
---
# Gobierno del conocimiento

## Clasificación

Cada nota tiene un identificador estable, tipo y estado. Los tipos separan constitución, contrato, decisión, investigación, evidencia, plan, revisión y navegación. `accepted` significa decisión vigente; `designed` significa contrato todavía no implementado; `observed` exige procedencia; `planned` no cuenta como progreso ejecutado.

Los contratos evitan futuros ambiguos: «debe» fija una obligación; «podría» exige alternativa o pregunta abierta. Una hipótesis tiene resultado que la refuta. Una afirmación de implementación identifica commit, plataforma, comando y resultado. Un resultado heredado de Thalyx se marca como reportado históricamente y nunca como ejecutado aquí.

## Cambios

Una decisión arquitectónica incluye problema, evidencia, alternativas, elección, consecuencias y condición de revisión. Cambiarla requiere actualizar el contrato dependiente y la tabla de invariantes en el mismo trabajo. Si cambia la semántica, se añade una decisión que sustituye la anterior; no se altera retroactivamente el razonamiento histórico.

La versión del vault es independiente de la versión ABI y de la implementación. Una etiqueta de Git fija una constitución revisable. No se promete compatibilidad de un ABI todavía no implementado. Las futuras rupturas del ABI tienen identificación explícita y mecanismos de rechazo.

## Estado que cabe leer

[Estado actual](roadmap/current-state.md) conserva únicamente situación vigente, evidencia y siguiente trabajo. [El changelog](../CHANGELOG.md) registra cambios consolidados. Los informes experimentales se guardan como artefactos separados con entorno y hashes; no se acumulan cientos de páginas de intentos dentro del punto actual.

Las fuentes de código quedan fijadas por commit y ruta en [el manifiesto](evidence/source-manifest.json). Las páginas externas incluyen fecha de consulta y alcance; una documentación viva no se convierte en una instantánea reproducible por tener un enlace. No se redistribuyen papers completos.

## Criterio de revisión

Una revisión recorre cada propiedad hacia su mecanismo y su experimento, y cada mecanismo hacia la propiedad que justifica su coste. Revisa además cierre bajo fallos, agotamiento, concurrencia, reinicio, autoridad del broker y equivalencia de perfiles. «Inspirado en un sistema verificado» no transfiere pruebas.

Las incertidumbres no frenan por defecto el trabajo: se adopta una opción provisional conservadora y una prueba para sustituirla. Solo una ausencia que impida definir un contrato correcto se declara bloqueo real. No se pide al usuario que resuelva elecciones técnicas investigables.
