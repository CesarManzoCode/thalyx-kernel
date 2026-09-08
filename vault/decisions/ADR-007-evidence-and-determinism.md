---
id: ADR-007
kind: decision
status: accepted
---
# Evidencia limitada y determinismo explícito

**Problema.** Una traza parcial, un timestamp o una semilla no prueban historia completa, causalidad o reproducción exacta. Las afirmaciones falsas son especialmente dañinas para un sistema que reutiliza conocimiento.

**Evidencia.** E13–E17; S09/S22/S23 distinguen restricciones de determinismo, orden causal y captura de provenance. S03 enseña a especificar supuestos.

**Elección.** Recibos kernel locales, publicación durable y diagnóstico como planos separados. Perfil auditado reserva antes de admisión; pérdidas y cobertura explícitas. Reejecución con entradas identificadas y runtimes deterministas opcionales. Distribución y consenso en usuario.

**Alternativas descartadas.** Ledger global de toda instrucción; provenance como permiso; exactly-once remoto derivado de un journal; replay general nativo obligatorio; autenticación remota mediante handles locales.

**Consecuencias.** Puede haber resultados desconocidos y trazas incompletas sin que el sistema los oculte. La auditoría durable cuesta almacenamiento y pertenece a un TCB definido. No se garantiza ausencia de canales laterales.

**Revisión.** Ampliar cobertura o determinismo para una carga definida con coste medido. No cambiar el significado de «probado» para acomodar una limitación técnica.

**Referencias.** [Observabilidad](../architecture/observability.md), [distribución](../architecture/distributed-determinism.md), [fuentes](../research/sources.md).
