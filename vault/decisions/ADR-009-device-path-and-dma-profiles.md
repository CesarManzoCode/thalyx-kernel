---
id: ADR-009
kind: decision
status: accepted
---
# Camino de dispositivo mínimo y perfiles de DMA que no se aproximan

**Problema.** K3 necesita un dispositivo real para que «driver en usuario» deje de ser una afirmación sobre el diseño. Un camino de dispositivo puede crecer en dos direcciones a la vez —más transportes, más rutas de interrupción, más clases de hardware— y cada una añade estado del kernel que después hay que revocar correctamente. Y el aislamiento que ese camino puede prometer depende de hardware que la plataforma de desarrollo no tiene.

**Evidencia.** La ejecución K3 sobre QEMU q35 con una función `virtio-blk-pci` moderna, con y sin `intel-iommu` descrito: [K3](../evidence/k3-smp-devices.md). S17 delimita DMA; [OQ-05](../roadmap/open-questions.md) registra que el inventario de hardware físico y de grupos de aislamiento no está hecho. INV-19 exige rechazar un perfil que no se pueda sostener en lugar de degradarlo en silencio.

**Elección.**

*Un solo camino de interrupción: MSI-X, escrita por el kernel.* No hay IOAPIC ni INTx. Una interrupción por mensaje es una escritura a memoria cuya dirección y cuyo dato eligen a quién se interrumpe y con qué vector; por eso la tabla la escribe el kernel y no el driver, y por eso una estructura del transporte que comparta página con esa tabla se rechaza en el descubrimiento. INTx añadiría compartición de línea entre funciones —revocación que ya no es por dispositivo— sin demostrar nada que MSI-X no demuestre.

*Un solo transporte: virtio moderno sobre PCI, con las estructuras localizadas por capacidad.* Sin modo legado. Lo que se está probando es que un driver de usuario pueda conducir hardware con ventanas de registros acotadas, interrupciones enlazadas a señales y buffers concedidos explícitamente; un segundo transporte repetiría el ejercicio sin ampliar la afirmación.

*Dos perfiles de DMA, y el fuerte se rechaza en lugar de aproximarse.* `WEAK_TRUSTED_DRIVER` es lo que una plataforma sin unidad de remapeo programada puede sostener, y el kernel lo declara con las palabras que le corresponden en cada registro que lo menciona: nada lo impone, el driver es de confianza. `STRONG_IOMMU` se pide explícitamente y se rechaza con `UNSUPPORTED_PROFILE`, un estado propio y no un error genérico, porque un rechazo que llega como otra cosa invita a reintentar hasta caer en una degradación silenciosa. Una unidad DMAR **descrita** no basta: mientras no esté programada el perfil sigue siendo el débil, y el motivo lo dice.

*La autoridad de recuperación no se delega con el dispositivo.* Maestro de bus y reset se quedan con quien asigna, no con quien conduce. El driver recibe mapear, enlazar interrupciones y conceder DMA; los dos primeros derechos no sirven de nada si el dispositivo no puede emitir transacciones, y quien decide que puede es quien podrá decidir que deje de poder.

**Alternativas descartadas.** Programar la unidad de remapeo en K3: es trabajo de tabla de contextos, invalidación de caché de IOTLB y grupos de aislamiento cuyo inventario no existe todavía, y hacerlo a medias produciría exactamente la afirmación que INV-19 prohíbe. Un perfil intermedio «débil pero con bounce buffers»: mueve la copia sin mover la frontera, y describirla como aislamiento sería falso. Soportar INTx para hardware más antiguo, y virtio-net junto a virtio-blk: ninguno amplía lo que K3 afirma. Dar reset al driver «para que se recupere solo»: un driver que puede resetear su dispositivo puede evitar que se lo quiten.

**Consecuencias.** El camino de dispositivo de K3 sirve para funciones virtio modernas con MSI-X y para nada más; hardware sin MSI-X queda fuera hasta que haya una razón medida para incluirlo. Ningún driver es no confiable en esta plataforma, y ninguna nota de evidencia puede decir lo contrario. Cuando exista IOMMU programada, `STRONG_IOMMU` deja de ser un rechazo y pasa a ser un perfil, sin que cambie lo que el débil significa.

**Revisión.** Programar la unidad de remapeo cuando exista el inventario de [OQ-05](../roadmap/open-questions.md) y hardware físico donde medir grupos de aislamiento reales; hasta entonces el perfil fuerte se rechaza. Añadir un segundo transporte o una segunda ruta de interrupción cuando un consumidor concreto lo necesite, no para completar una tabla.

**Referencias.** [Hardware](../architecture/hardware.md), [memoria](../architecture/memory.md), [concurrencia](../architecture/concurrency.md), [K3](../evidence/k3-smp-devices.md).
