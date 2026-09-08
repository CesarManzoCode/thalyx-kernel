---
id: EVD-002
kind: evidence
status: observed
---
# Historia como evidencia de diseño

Se inspeccionó el historial Git completo alcanzable desde la revisión de corte: 407 commits. El vault inicial se incorporó el 1 de agosto de 2026 con 43 notas; el árbol de corte contiene 96 archivos bajo `vault/`. Esto mide material disponible, no exhaustividad de validación.

## Decisiones y regresiones que importan

| Revisión | Cambio observado | Lección que conserva Thalyx-Kernel |
|---|---|---|
| [f111306](https://github.com/CesarManzoCode/thalyx/commit/f11130662a22a2a811d5f537649e149fa66c2842) | Vault fundacional organizado alrededor de filosofía, arquitectura, primitivas y flujo canónico. | La constitución debe preceder al código, pero el estado vivo debe poder corregir interpretaciones iniciales. |
| [7acde177](https://github.com/CesarManzoCode/thalyx/commit/7acde1777ee3cb82dc54cc2b2a16a714cfdc547c) | Resolución de contradicciones entre notas fundacionales. | La unidad de revisión es el conjunto; una nota correcta aisladamente puede romper otra. |
| [7200f69](https://github.com/CesarManzoCode/thalyx/commit/7200f691f08ef9474eb84692b334e84220c94ae9) | Registrar intención antes de commit. | Si el proceso cae entre efecto y registro, la recuperación necesita una fuente de verdad que resuelva la ventana. |
| [8ae190d](https://github.com/CesarManzoCode/thalyx/commit/8ae190da1cfaa52d7f9f9024457d273d4e2f7eec) | Exclusión mutua y permisos ligados a la versión exacta activada. | No publicar estado y autoridad mediante dos decisiones independientes. |
| [087b2f0](https://github.com/CesarManzoCode/thalyx/commit/087b2f0ebf8fc29db67d1aeeed189ba0bf93b011) | Corregir orden de preparación del sandbox y aplicación de restricciones. | La transición a ejecutable debe ser un punto único después de establecer la protección. |
| [bab9408](https://github.com/CesarManzoCode/thalyx/commit/bab9408a3bda8974909badd9d12e865abf078e1e) | Compartir la exclusión del store durante abandono de intento. | Las transacciones compiten con instalación, restauración y operaciones de mantenimiento. |
| [def0bee](https://github.com/CesarManzoCode/thalyx/commit/def0bee687ed6df410cf001f681965270e97877a) | Fortalecer witness y rechazo de restauración contra estado cambiado. | Timestamps y tamaño no son identidad suficiente para una operación destructiva. |
| [adc9ed8](https://github.com/CesarManzoCode/thalyx/commit/adc9ed80a0384241a93c89cfa9cd8872fe9c5f84) | Corregir pérdida de retención del motor residente. | Una prueba funcional debe medir la propiedad de ciclo de vida que afirma, como el número de cargas. |
| [17c042a](https://github.com/CesarManzoCode/thalyx/commit/17c042a643b728eef59d549139aa0ecb8be2f2bd) | Introducir programas acotados entre inferencias. | El consumidor real evoluciona hacia cómputo y herramientas compuestas, no una lista inmóvil de operaciones. |
| [08172a3](https://github.com/CesarManzoCode/thalyx/commit/08172a3f9e115b40280b4b53a902ac173eaee1fa) | Confinamiento del proveedor semántico. | Un compilador invocado por un servicio también es código con autoridad y consumo. |
| [cf58fbe](https://github.com/CesarManzoCode/thalyx/commit/cf58fbec4ccd851444b5a808bf5b13fac4a0850d) | Permitir la operación usada por musl para crear hijos. | «Usamos una abstracción propia» no borra las necesidades reales de libc, compilador o toolchain. |
| [29cae8a](https://github.com/CesarManzoCode/thalyx/commit/29cae8a122e913464ea88d0afabb72f86e4ad28a) | Preservar resolución cuando se agota presupuesto de enriquecimiento. | El fracaso de una subconsulta no cambia retroactivamente evidencia válida. |
| [a563999](https://github.com/CesarManzoCode/thalyx/commit/a563999839470f182ecc8378c1d8287a82538236) | Archivar validación bajo el árbol dejado por Cargo. | Una herramienta puede modificar sus propias entradas; «validé este estado» necesita identidad estable, no el nombre del directorio. |

Los hashes completos de las revisiones abreviadas se incluyen en [el manifiesto](source-manifest.json). Esta tabla resume cambios de código e intención registrada; no atribuye una causa universal ni un resultado de pruebas que no se ejecutó aquí.

## Qué mejora este vault

El vault de Thalyx preserva razonamiento útil y mucha experiencia real. También mezcla en notas muy extensas situación actual, resultados de máquina, intentos y correcciones. Algunas formulaciones tempranas dejan de describir herramientas presentes al final del historial.

Aquí se separan cinco planos: evidencia fijada, contrato normativo, decisión histórica, estado corto y experimentos reproducibles. Los enlaces entre ellos son obligatorios. No se heredan como hechos las afirmaciones comparativas absolutas del vault anterior ni como decisiones vinculantes sus elecciones de Linux, proceso base o interfaz.

La historia tampoco dicta repetir cada parche como un objeto del kernel. Del problema de una caché inválida se deriva una necesidad de entradas identificables; del problema de un permiso mal ligado se deriva publicación coherente de política. El mecanismo se vuelve a elegir desde esas propiedades.
