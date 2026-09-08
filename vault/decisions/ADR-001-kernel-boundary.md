---
id: ADR-001
kind: decision
status: accepted
---
# Microkernel con ámbitos de trabajo

**Problema.** Thalyx compone protección, autoridad y estado mediante varios mecanismos. Una tarea puede continuar en un servidor después de terminar su proceso emisor. Una frontera basada únicamente en procesos no expresa esa vida.

**Evidencia.** E02/E04/E08/E18 describen mediación, lanzamiento y residencia. S04/S05/S13 permiten separar protección de política y consumo de proceso. S06 impide deducir una escuela a partir de capacidades; S10 aporta una alternativa de lenguaje.

**Elección.** Microkernel con dominios, capacidades, ámbitos, memoria, IPC y recursos. Drivers y servicios transaccionales se ejecutan en usuario. El ámbito es identidad operativa de trabajo, no tarea semántica.

**Alternativas descartadas.** Monolito nuevo: integra costes, pero amplía el radio de fallos sin necesidad demostrada. Un solo espacio Rust: no protege frente a todo el código nativo del consumidor. Exokernel completo: deja demasiada semántica de vida/cargo a cada libOS. Kernel existente: fuente de evidencia, incompatible con construir este proyecto desde cero.

**Consecuencias.** Se paga IPC y recuperación de servidores. El TCB total no se reduce automáticamente: estado y autorización siguen siendo confiables para sus propiedades. El kernel no interpreta prompts, grafos o paquetes de Thalyx.

**Revisión.** Medir una vertical real. Una ruta caliente podrá optimizarse o incorporar un mecanismo mínimo nuevo si se demuestra que su ubicación actual impide la propiedad o impone un coste dominante; no se traslada una política completa por intuición.

**Referencias.** [Fuentes](../research/sources.md), [alternativas](../research/alternatives.md), [contrato](../architecture/overview.md).
