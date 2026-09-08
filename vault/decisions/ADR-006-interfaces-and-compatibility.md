---
id: ADR-006
kind: decision
status: accepted
---
# ABI propio y compatibilidad en usuario

**Problema.** Conservar automáticamente Unix como interfaz central puede impedir derivar autoridad/vida desde el consumidor; ignorar sus dependencias haría inviable el port real.

**Evidencia.** E01/E03/E16 y el fallo histórico de spawn musl. S06 demuestra que la compatibilidad puede coexistir con capacidades, pero no sin un contrato específico.

**Elección.** ABI binario acotado de objetos, separado de protocolos de plataforma y API semántica. Compatibilidad inicial de fuente mediante runtime/biblioteca de usuario y servicios. Rechazo explícito de perfiles o versiones insuficientes.

**Alternativas descartadas.** Binarios Linux supuestamente portables por ser estáticos; aceptar una parte indeterminada de syscalls Linux y llamarla compatibilidad; insertar CBOR/Rust layouts en el kernel; convertir Thalyx-ABI de módulos en syscall ABI.

**Consecuencias.** El port de std/libc, herramientas y motor es un esfuerzo importante y explícito. Una VM de legado no cuenta como integración nativa. Los números de opcodes se fijan al implementar bindings, sin prometer estabilidad anticipada.

**Revisión.** Si un subconjunto POSIX reduce coste sin introducir autoridad ambiental en el kernel, ampliarlo en el servicio. Compatibilidad binaria puede evaluarse como producto adicional después de la vertical nativa.

**Referencias.** [ABI](../architecture/abi.md), [portabilidad](../integration/thalyx.md).
