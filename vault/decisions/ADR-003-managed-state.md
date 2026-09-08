---
id: ADR-003
kind: decision
status: accepted
---
# Estado administrado y capacidades efímeras

**Problema.** Validaciones sobre árboles cambiantes y publicación separada de permisos producen ambigüedad. Atomicidad visible no garantiza recuperación durable.

**Evidencia.** E10–E15 y las regresiones de identidad de entradas. S19/S20 muestran la importancia del protocolo durable; S21 muestra el problema del reintento.

**Elección.** Servicio único de estado, objetos inmutables, workspace privado y CAS sobre generación. Log con PREPARE, dependencias durables, COMMIT y recibo/política en la misma publicación. Capacidades de kernel no persisten; autorización reconstruye delegaciones en una época nueva.

**Alternativas descartadas.** Watcher como identidad exacta; writable mmap sobre raíces publicadas; rename como prueba suficiente de durabilidad; transacciones semánticas en kernel; restaurar procesos y grants transparentemente después de reinicio.

**Consecuencias.** Se requiere escritor exclusivo, formato de recuperación, cuota de retención y reserva para compactación. La publicación V0 se serializa. Los efectos remotos quedan fuera; timeout puede dejar resultado desconocido. Restaurar una raíz vieja es una publicación nueva.

**Revisión.** Fragmentar raíces o paralelizar commits solo después de especificar atomicidad entre particiones y medir el cuello de botella. Incorporar una base de datos existente puede reemplazar el formato si conserva exactamente CAS, dedupe, política y recibo; no debe crear dos fuentes de verdad.

**Referencias.** [Persistencia](../architecture/persistence.md), [memoria](../architecture/memory.md), [fuentes](../research/sources.md).
