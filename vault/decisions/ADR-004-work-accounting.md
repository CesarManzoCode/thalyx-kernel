---
id: ADR-004
kind: decision
status: accepted
---
# Recursos por ámbito

**Problema.** Un motor y un servidor de archivos atienden muchos clientes. Cobrar solo por proceso oculta quién causó trabajo, pero repartir todo coste compartido exactamente tampoco es posible.

**Evidencia.** E09/E18; S05 distingue recurso y proceso; S02 separa presupuesto y thread; S14/S18 muestran mecanismos reales y diferencias entre cuotas y reservas.

**Elección.** Ámbitos jerárquicos, contexto efectivo prestado en IPC, tickets asíncronos y patrocinador único de páginas. CPU V0 con ventanas fijas alineadas, selección justa entre ámbitos y límites agregados de paralelismo. Mantenimiento/recuperación con cuentas separadas y origen conservado.

**Alternativas descartadas.** Un presupuesto nuevo por worker; cobrar RSS sumado como memoria física; borrar cargos al morir el cliente; planificador aprendido como mecanismo de seguridad; afirmar hard real-time con una simple cuota.

**Consecuencias.** Metadatos en rutas de IPC y sincronización de presupuesto SMP. Ventanas fijas permiten ráfagas en fronteras. CPU indirecta y caché compartida requieren una política visible del servicio. Hay reserva no utilizable por agentes para recuperación.

**Revisión.** Medir latencia, deuda de sobrepaso y contención. Un servidor esporádico con refills acotados reemplazará ventanas fijas si una necesidad temporal concreta lo justifica. Los números iniciales no se conservan por inercia.

**Referencias.** [Recursos](../architecture/resources.md), [IPC](../architecture/ipc.md), [fuentes](../research/sources.md).
