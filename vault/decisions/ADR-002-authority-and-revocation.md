---
id: ADR-002
kind: decision
status: accepted
---
# Capacidades y cierre explícito

**Problema.** Revocar una identidad de proceso o una apertura no define qué ocurre con copias, mapas, servicios e I/O pendientes. Un check separado de la admisión deja una carrera.

**Evidencia.** E05–E07 muestran límites de la composición actual. S01/S07 aportan objetos y derechos; S17/S29 delimitan accesos que sobreviven a handles.

**Elección.** Handles locales generacionales, grants con restricciones heredadas y admisión linealizable. Copias conservan revocación; derivaciones solo atenúan. `fence`, quiescencia y retirada son resultados distintos. Publicaciones requieren una admisión de efecto de servidor todavía autorizada.

**Alternativas descartadas.** Tokens de texto sin verificación local; identidad UID/cgroup como permiso universal; matar al cliente como prueba de drenaje; expiración que pretende deshacer efectos históricos. Ninguna sostiene el contrato requerido en todos sus caminos.

**Consecuencias.** Hay metadatos, referencias inversas de mapas y obligaciones retenidas. El cierre puede tardar y debe explicarlo. Un servidor con capacidades propias sigue siendo confiable para no actuar como deputy incorrecto. No existe revocación de información ya copiada.

**Objetos de servicio.** Las capacidades de endpoint llevan facetas inmutables, creadas con autoridad administrativa separada. El servidor las vincula a objeto y operaciones. Una derivación ordinaria no cambia esa faceta; proporcionar un digest o ID no concede acceso a otro objeto del mismo servidor.

**Revisión.** Optimizar búsquedas/epochs solo si preservan atenuación y las carreras definidas. Revisar granularidad de grants con medidas de profundidad y coste; no sustituir una prueba de vida por una caché sin invalidación demostrada.

**Referencias.** [Autoridad](../architecture/authority.md), [concurrencia](../architecture/concurrency.md), [fuentes](../research/sources.md).
