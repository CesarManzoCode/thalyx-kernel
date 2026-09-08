---
id: ADR-005
kind: decision
status: accepted
---
# x86_64, UEFI y Rust

**Problema.** Construir desde cero exige escoger una plataforma depurable y sostener herramientas nativas reales. Empezar por todos los dispositivos o una GPU hace imposible aislar fallos fundacionales.

**Evidencia.** E01 documenta el entorno de Thalyx. S24–S27 definen targets y arranque; S10 delimita aislamiento de lenguaje; S17 delimita DMA.

**Elección.** QEMU q35/OVMF, x86_64, UP inicial y SMP antes de comparación real. Kernel Rust `no_std`, ensamblador y unsafe pequeños y auditables. Protección MMU para código arbitrario. Drivers de usuario; perfil IOMMU explícito. CPU/SSE2 inicial para el consumidor.

**Alternativas descartadas.** ARM/CHERI como requisito inicial sin necesidad demostrada; BIOS y un catálogo amplio de drivers; safety de lenguaje como única frontera; AVX/GPU antes de controlar todo su estado y memoria.

**Consecuencias.** Se implementan trampas, memoria, FP, SMP y runtimes propios. Rust no elimina TCB ni verifica protocols. QEMU sin IOMMU no demuestra aislamiento DMA.

**Revisión.** Portar a otra arquitectura después de separar código común y arquitectura, cuando exista hardware/carga concreta. Activar SIMD adicional y drivers físicos con pruebas de aislamiento y estado, no por compilar con native CPU.

**Referencias.** [Hardware](../architecture/hardware.md), [memoria](../architecture/memory.md), [fuentes](../research/sources.md).
