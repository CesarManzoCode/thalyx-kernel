---
id: ARC-006
kind: contract
status: designed
---
# Memoria, alias y versiones

## Modelo inicial

El kernel arbitra páginas físicas y espacios de direcciones. `MemoryObject` es una secuencia de páginas con longitud y derechos máximos. Mapear requiere capacidad sobre el objeto y autoridad sobre el dominio destino. Las páginas nuevas se entregan a cero; se elimina información residual al pasar entre propietarios.

V0 usa páginas de 4 KiB, asignación explícita y backing residente. No hay swap, deduplicación automática, overcommit ni pager externo general. Al fallar una reserva se devuelve un error antes de prometer memoria. Esto simplifica contabilidad, aislamiento y primeros fallos. Huge pages, NUMA y paginación a demanda necesitan mediciones y un contrato de recuperación posterior.

La memoria ejecutable cumple W^X por mapa y por política de creación de código: una página ejecutada no conserva un alias escribible accesible al mismo adversario. JIT futuro utiliza transición controlada de escritura a ejecución con invalidación adecuada; no se abre RWX porque un runtime lo solicite. Stacks tienen guard pages y límites.

Los mapas V0 admiten R, RW y RX; no se anuncia memoria write-only o execute-only mediante paginación x86 ordinaria. Mapear RW/RX requiere READ además del derecho correspondiente. Las operaciones mediadas y los permisos de servicio pueden ser más finos que un mapa de hardware.

## Sellar no es marcar un bit

`seal` promete que nadie dentro del perímetro de protección puede modificar esos bytes mientras el objeto sellado exista. Requiere:

1. Impedir nuevas derivaciones/mapas de escritura y nuevas asignaciones DMA.
2. Retirar todos los alias escribibles existentes, incluidos mapas en otros dominios.
3. Completar invalidaciones de TLB y parar accesos de dispositivo relevantes.
4. Publicar el estado sellado solo después de esas confirmaciones.

Se mantiene un índice inverso de mapas y un estado `Mutable → Sealing → Sealed`. Un fallo deja el objeto sin la promesa de sellado; los bytes no se entregan como inmutables.

La primera implementación puede copiar a un objeto privado, retirar su único mapa de escritura y sellarlo. La copia reduce el problema de alias y permite medir antes de optimizar. «Zero-copy» no justifica un lifetime indefinido ni una mutación oculta.

Sellar memoria no hace durables los bytes y no los convierte en una versión completa de un filesystem. Esa distinción evita confundir una propiedad MMU con una transacción de almacenamiento.

## Estado administrado

Los objetos publicados por el servicio de estado son inmutables. Un workspace mutable es privado; produce una nueva versión mediante copia o copy-on-write administrado. El perfil estricto no expone mapas compartidos de escritura sobre contenido publicado ni accesos alternativos de escritura al dispositivo.

Las entradas de una validación incluyen raíz, toolchain, configuración y dependencias externas declaradas. Si una herramienta modifica Cargo.lock, esa salida pertenece a una versión diferente que debe validarse o ser identificada como tal. Una caché no recicla el resultado de una versión bajo otra generación por coincidir el nombre.

El hash de contenido puede reutilizar objetos iguales. La generación de publicación evita ABA cuando una raíz vuelve a un contenido anterior. Un mtime o una notificación de escritura no desempeñan ninguno de esos dos papeles.

## Propiedad y contabilidad

Cada página física tiene un patrocinador. Los mapas adicionales pagan entradas de tablas y metadatos, no vuelven a cobrar la misma página como si se hubiese asignado otra. Compartir pesos de un motor hace visible un coste residente; no permite que desaparezca de la cuenta del sistema.

Una página privada creada por copy-on-write se cobra al ámbito que provoca la copia, antes de instalarla. Si no tiene presupuesto, se falla la operación o se entrega una excepción controlada. Un lector que conserva un objeto impide su reclamación y consume una reserva de retención definida por el servicio.

Transferir patrocinio exige aceptación del nuevo pagador y capacidad suficiente. Al morir un propietario, memoria retenida continúa cargada al subárbol/ancestro responsable hasta transferencia o liberación. No se ofrece memoria «gratis» durante limpieza.

## Dispositivos y DMA

El IOMMU limita a qué páginas accede un dispositivo cuando la topología soporta aislamiento. La unidad mínima de asignación puede ser un grupo de dispositivos, no una función PCI arbitraria. MMIO por sí solo no confina bus mastering.

Las páginas DMA se fijan y cobran. Revocar acceso requiere detener colas, completar o resetear operaciones, invalidar mapas IOMMU y confirmar el protocolo del dispositivo. No se devuelve RAM al allocator mientras siga existiendo posibilidad de DMA pendiente. Ante incertidumbre se conserva en cuarentena y se informa el coste.

El perfil de desarrollo QEMU sin aislamiento IOMMU considera confiables los drivers DMA y no afirma aislamiento frente a ellos. El perfil nativo fuerte exige IOMMU y grupos válidos; si faltan, rechaza asignación no confiable. No se degrada automáticamente a DMA irrestricto.

## SMP y orden de memoria

En SMP, cambiar una tabla no revoca una traducción que otro núcleo conserva. Los mapas retirados quedan pendientes hasta el acuse de todos los núcleos relevantes. La muerte de un hilo no sustituye un shootdown.

Las estructuras compartidas usan locks o atómicos con relaciones de orden justificadas. x86 TSO no permite data races en Rust ni sustituye barreras de MMIO/DMA. [Concurrencia](concurrency.md) fija los puntos de publicación y reclamación; las optimizaciones deben conservarlos.
