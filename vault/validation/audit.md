---
id: VAL-003
kind: review
status: observed
---
# Auditoría de coherencia de la fundación

Fecha: 2026-09-08. Alcance: revisión de esta arquitectura como conjunto, sus referencias a Thalyx, contratos y modelos limitados. Es una revisión realizada durante la construcción del vault; no una auditoría externa independiente ni verificación de código kernel.

## Hallazgos resueltos

| Cruce examinado | Problema encontrado o afirmación que debía impedirse | Resolución incorporada |
|---|---|---|
| Capacidad ↔ objeto de servicio | Permiso de enviar a un servidor era insuficiente para explicar autoridad sobre una raíz específica. | Faceta inmutable de endpoint, derecho BIND separado y tabla de objeto/verbo/política en el servicio; INV-23. |
| Derechos abstractos ↔ MMU x86 | Separar READ/WRITE podía sugerir mapas write-only que V0 no puede imponer. | Mapas R/RW/RX; WRITE/EXECUTE mapeados exigen READ o se rechazan. |
| Grant ↔ publicación diferida | Una comprobación temprana podía sobrevivir a fence y comenzar un efecto nuevo. | Admisión de efecto sobre el mismo linaje y bajo sincronización; modelo rival produce contraejemplo. |
| Barrera ↔ cierre durable | Prohibir toda llamada del ámbito cerrado podía impedir finalizar una publicación ya aceptada. | Cliente cercado; ticket retenido; servicio completa/aborta usando reserva de cierre propia con origen y coste explícitos. |
| Cancelación ↔ efectos | «Revocado» podía interpretarse como ausencia de cualquier efecto posterior. | Barrera, quiescencia y retirada separados; efectos previos pueden completar; el modelo muestra la ventana. |
| Expiración ↔ mapas/CPU | Un deadline podía confundirse con interrupción exacta de todos los loads/stores. | Rechazo de nuevas admisiones y drenaje de acceso directo son hitos diferentes. |
| Estado ↔ validación | Digest/witness podía confundirse con raíz inmutable o con generación. | Tipos canónicos, versión, cobertura y generación CAS diferentes; caso ABA explícito. |
| Estado ↔ permisos ↔ evidencia | Tres registros separados podían producir combinación incoherente después de caída. | Misma raíz/commit para política y recibo; índices derivados no son otra fuente de verdad. |
| Retry ↔ retención | Expulsar un resultado podía convertir una petición vieja en nueva. | High-water mark durable, secuencia por principal y RESULT_EXPIRED sin reejecución. |
| Sellado ↔ alias ↔ DMA | Un bit de estado podía declarar bytes inmutables mientras quedaban escritores. | Retirada de todos los alias y acuses antes del sello; copia conservadora inicial. |
| Recursos ↔ muerte del emisor | Terminar el proceso podía borrar cargos aún retenidos por servidor/dispositivo. | Patrocinador y ticket sobreviven hasta liberación/transferencia autorizada. |
| CPU ↔ SMP ↔ servicios | Donación o múltiples cores podían gastar el mismo presupuesto dos veces. | Reservas en ancestros, paralelismo agregado y deuda entre ventanas. |
| Auditabilidad ↔ saturación | Fail-closed del log podía bloquear el propio cierre de autoridad. | Canal/celdas de cierre independientes, cobertura explícita y reserva por operación auditada. |
| Microkernel ↔ TCB | Sacar un servicio a usuario podía confundirse con eliminar su papel de confianza. | Matriz por propiedad; drivers sin IOMMU y brokers siguen contándose donde corresponde. |
| ABI ↔ port de Thalyx | Un binario musl estático o un único wrapper podía parecer portable sin más. | Tres fronteras; inventario de runtime/herramientas y port nativo como trabajo explícito. |
| Comparación ↔ Linux | Ventajas del servicio administrado podían atribuirse injustificadamente al kernel. | Mismo perfil y servicio posible sobre Linux; comparación de superficie separada. |

## Resultados ejecutados

Intérprete: Python 3.12.14. El [informe](../../research/models/results.json) incluye hash del programa y resultados reproducibles.

- MODEL-01: **294 estados y 615 transiciones** del espacio declarado; propiedades abstractas satisfechas. La variante check-then-admit encuentra una admisión indebida después de fence. También se obtiene un efecto legítimo posterior a fence cuando su admisión fue anterior.
- MODEL-02: **23 casos de caída** del protocolo correcto. Las variantes sin flush previo de datos y con ACK prematuro producen los contraejemplos esperados.
- ABA: comparar contenido acepta incorrectamente A/0 después de A/0 → B/1 → A/2; comparar generación lo rechaza.

Estos resultados no comprueban locks Rust, MMU, memoria débil, DMA, disco real, formato de checkpoints ni todo el protocolo de dedupe. Los [experimentos de implementación](experiments.md) permanecen pendientes.

## Revisión de cobertura

Se recorrieron las propiedades de [la constitución](../constitution.md) hacia [23 invariantes](invariants.md), mecanismos y experimentos, y se revisaron los mecanismos hacia la necesidad que justifican. Se revisaron nombres, identidad, ownership, cierre, reinicio, cuotas, fuentes de verdad y versiones de interfaz.

Las alternativas rechazadas tienen una razón y condición de revisión. Los parámetros que requieren medida se declaran provisionales. Las dependencias materiales ausentes —hardware, port, flush, entropía— limitan garantías concretas; no se completan con datos inventados.

El comprobador documental verifica metadatos, IDs únicos, enlaces locales, forma de hashes y correspondencia del informe con su script. Los enlaces de código se fijan a revisiones existentes del clon. Esa comprobación no certifica el contenido de una página externa ni reemplaza esta lectura de arquitectura.

Resultado ejecutado: **PASS**, 39 IDs únicos y 116 enlaces locales comprobados. Se resolvieron además las 36 referencias Markdown a código/historia de Thalyx contra objetos Git del clon fijado, sin referencias ausentes ni revisiones abreviadas en los destinos. `git diff --cached --check` terminó sin errores.

## Conclusión de la revisión

No quedan contradicciones conocidas sin resolver entre los contratos vigentes. Quedan obligaciones de implementación y preguntas empíricas identificadas. La siguiente acción es K1: construir y comprobar arranque protegido, manteniendo este vault como contrato revisable.
