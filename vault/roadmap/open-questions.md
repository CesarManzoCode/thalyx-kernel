---
id: PLAN-002
kind: research
status: planned
---
# Preguntas abiertas con respuesta provisional

No hay una arquitectura escondida pendiente de que el usuario escoja una escuela. Las incertidumbres siguientes necesitan implementación, medida o datos que hoy no existen.

| ID | Pregunta y dato ausente | Decisión utilizable ahora | Cómo se resuelve y cuándo |
|---|---|---|---|
| OQ-01 | Coste de validar cadenas de grants/ámbitos en IPC real. | Profundidades acotadas, checks completos y estructuras sencillas. | EXP-02/12; perfilar antes de introducir caches/epochs, K2–K6. |
| OQ-02 | Latencia requerida de UI, motor y cierre bajo saturación. | Cuotas de ventana fija, recuperación reservada, sin hard real-time. | Medir colas y deuda con Thalyx; evaluar servidor esporádico si la necesidad lo exige. |
| OQ-03 | Granularidad óptima de payloads y coste de copias. | 256 bytes inline, cuatro capacidades, copia/sellado conservador. | Trazas de workload y microbench comparables; ajustar con versión ABI. |
| OQ-04 | Contención SMP en cuentas y metadatos. | Reservas globales correctas antes de sharding. | EXP-04/12 en varios núcleos; revisar partición solo con medidas. |
| OQ-05 | Inventario de hardware físico y aislamiento IOMMU efectivo. | QEMU y perfiles separados; no prometer aislamiento DMA universal. | Inventario CPU/firmware/dispositivos/grupos al integrar hardware, K3. |
| OQ-06 | Coste y contrato mínimo de port de herramientas. | Compatibilidad de fuente; inventario completo desde K1. | Spike de runtime/toolchain y carga real K5; no contar host o VM Linux como native. |
| OQ-07 | Coste de log, retención y compactación para repositorios/modelos reales. | Escritor único, reserva de segunda arena, rechazo antes de agotar cierre. | EXP-08/12; valorar estructuras de índices/segmentos sin cambiar atomicidad. |
| OQ-08 | Biblioteca de disco/hash y formato de bytes exacto. | Hash identificado, longitud/tipo canónicos, registros enmarcados y protocolo fijado. | Elegir dependencia auditada y fixtures antes de K4; no se congela un formato sin parser. |
| OQ-09 | Entropía y cadena verificable de arranque en equipo objetivo. | Imagen de desarrollo confiada explícitamente; perfil criptográfico exige fuente y claves válidas. | Diseñar provisión, test de firmware y anclaje antes de anunciar verified boot. |
| OQ-10 | Aislamiento/planificación de GPU y aceleradores. | CPU primero; ninguna promesa de revocar kernels GPU instantáneamente. | Hardware, firmware, reset/preempción y costes de buffers concretos después de K5. |
| OQ-11 | Utilidad de un runtime determinista para workloads reales. | Entradas identificadas y reejecución; no replay universal. | Experimento acotado con hostcalls controladas; medir beneficio y restricciones. |
| OQ-12 | Qué segundo consumidor probará la frontera. | Kernel sin vocabulario de Thalyx, protocolos explícitos. | Un prototipo mínimo independiente y luego un consumidor real; evitar API especulativa. |
| OQ-13 | Alcance de pruebas formales sostenible por el proyecto. | Modelos pequeños y obligaciones por contrato; cero afirmaciones de verificación general. | Elegir componente de alto riesgo y modelo refinable con código cuando exista K2. |
| OQ-14 | Política de redistribución/licencia y procedencia de aportes. | No copiar código de kernels ni asumir que licencias de Thalyx se heredan al proyecto separado. | Fijar licencia de distribución y política de contribuciones antes de incorporar/publicar código de terceros; no bloquea el diseño ni escribir K1 original. |

## Cuándo una incertidumbre sí bloquea una garantía

Sin conocer el comportamiento de flush no se declara durabilidad del dispositivo. Sin IOMMU válido no se afirma aislamiento de un driver DMA no confiable. Sin port ejecutado no se anuncia Thalyx nativo. Sin fuente de entropía/provisión no se anuncia una cadena criptográfica segura.

Estos límites bloquean una **afirmación o perfil concreto**, no el trabajo autorizado de construir componentes anteriores. Se avanza con el perfil que realmente se puede sostener y se conserva la deuda explícita.
