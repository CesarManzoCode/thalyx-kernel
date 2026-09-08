---
id: ARC-011
kind: contract
status: designed
---
# Plataforma, lenguaje y arranque

## Elección inicial

Se elige x86_64, QEMU q35 y UEFI mediante OVMF. Reduce distancia respecto del consumidor actual y permite depuración, discos desechables y fallos repetibles. Un núcleo virtual sirve para K1; SMP es obligatorio antes de afirmar que el sistema sostiene comparación real de un motor residente y herramientas concurrentes.

El primer conjunto de dispositivos es consola serie para diagnóstico, timer/APIC, PCI y virtio-blk; después virtio-net. El framebuffer no es requisito del primer arranque. GPU, USB general, Wi-Fi, suspensión, hotplug, NUMA y BIOS quedan fuera de la primera vertical.

QEMU TCG sirve para corrección y fallos. KVM y hardware físico son entornos diferentes y se registran por separado. Un driver de usuario sin IOMMU sigue perteneciendo al TCB de memoria frente a DMA.

## Lenguaje y límites de confianza

El kernel usa Rust `no_std`, target `x86_64-unknown-none`, y ensamblador mínimo para entrada, interrupciones, cambio de contexto y transiciones de privilegio. El código de dispositivos puede usar `unsafe` en procesos separados. No se incorpora Linux ni otro kernel como implementación base.

Rust reduce clases de errores en código seguro; no demuestra corrección del allocator, MMU, atomics, interrupciones, código de arranque o protocolos. Cada frontera `unsafe` documenta precondiciones, alias, propiedad, orden y manejo de fallos. Se prohíbe exponer una API segura que permita violarlas.

El loader UEFI es un binario separado para `x86_64-unknown-uefi`, usando ABI `efiapi`. El kernel no hereda por accidente su target, red zone, runtime o convención de llamadas. Se fija toolchain y dependencias al comenzar K1; el mínimo de Rust de Thalyx no determina automáticamente el del kernel nuevo.

## Secuencia de bootstrap

1. Firmware carga el loader EFI desde una imagen de prueba.
2. Loader valida tamaños, rangos y formato del ELF kernel y del paquete inicial; carga segmentos sin solapamientos.
3. Obtiene mapa de memoria y datos de arranque, reserva estructuras, vuelve a obtener la clave vigente y ejecuta `ExitBootServices`, reintentando conforme al protocolo UEFI si la clave cambió.
4. Transfiere control con una estructura versionada de rangos físicos, ACPI y módulos, sin punteros a memoria cuya vida no esté reservada.
5. Kernel instala sus tablas, GDT/IDT/TSS, allocator y stack de excepción; conserva regiones de firmware/ACPI según su tipo.
6. Configura reloj, interrupciones y mecanismos de protección; crea primer dominio y manifiesto de capacidades.
7. Entra en ring 3 mediante una transición validada. Una excepción de ese dominio debe llegar al supervisor o diagnóstico previsto.

La verificación de firmas de arranque y un anclaje contra rollback requieren claves y política. En desarrollo se admite imagen local explícitamente confiable; no se anuncia verified boot sin cadena verificadora completa.

## Perfil CPU V0

Modo de 64 bits, paginación de cuatro niveles, NX y protección de escritura del supervisor. Memoria de usuario en rango canónico inferior, kernel en rango superior, páginas cero/guard sin mapear. V0 no soporta procesos de 32 bits ni espacios de direcciones de cinco niveles.

La entrada de syscall usa stack de kernel por hilo y valida direcciones/flags. El retorno inicial privilegia un camino conservador mediante `iretq` validado; optimizar `sysret` exige cubrir estados no canónicos y flags peligrosos. Interrupciones y fallos dobles tienen stacks de emergencia.

Se comprueban SMEP/SMAP y otras capacidades antes de afirmar el perfil que las exige; falta de soporte produce perfil inferior explícito o rechazo. No se permite que una configuración compilada asuma silenciosamente una característica ausente.

El kernel evita SIMD. Usuario V0 admite x87/SSE2 con guardado/restauración **eager** de estado por hilo, inicializado y sin fuga entre dominios; AVX y extensiones posteriores se habilitan únicamente después de implementar y probar su estado completo. Un target Linux optimizado para la CPU del host no se ejecuta sin revisar estos supuestos.

## Reloj y entropía

El reloj de autoridad debe ser monotónico entre núcleos. Se valida la fuente y se convierte a nanosegundos con control de overflow; un TSC no sincronizado no se usa como reloj global por conveniencia. Suspend/resume no forma parte de V0, por lo que no se hereda una promesa de vencimiento durante suspensión.

La época de arranque debe ser fresca; generación de claves requiere además entropía adecuada. Se documenta la fuente disponible —firmware, hardware y/o dispositivo virtual— y se falla el perfil criptográfico cuando no existe una fuente aceptada. Un valor de prueba determinista se marca como tal y no produce evidencia de unicidad global.

## Dispositivos y plataforma física

Virtio negocia features y valida anillos, longitudes y completions. Se exigen barreras correctas y soporte de flush para declarar el contrato de almacenamiento durable. Un reset invalida la sesión del driver; los clientes no reutilizan descriptores antiguos.

El hardware físico de primera aceptación debe inventariar CPU, firmware, RAM, grupos IOMMU y dispositivo de almacenamiento concretos. La información histórica de equipos usados con Thalyx orienta, pero no certifica la máquina donde correrá Thalyx-Kernel. K1 puede comenzar sin ese inventario; la validación DMA fuerte no.

Las fuentes consultadas para Rust, UEFI, virtio y manuales de CPU están en [la bibliografía](../research/sources.md). El índice del manual Intel se usó para identificar documentación; no se afirma haber auditado todos sus volúmenes.
