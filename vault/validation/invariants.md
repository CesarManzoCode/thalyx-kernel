---
id: VAL-001
kind: contract
status: designed
---
# Invariantes y base de confianza

**Todas las garantías de esta tabla son obligaciones de implementación.** Ninguna se declara satisfecha por un kernel existente en este repositorio. Los modelos de investigación cubren fragmentos abstractos de INV-02/03/04/12/13; su alcance exacto está en [los modelos](../../research/models/README.md).

## Propiedad → mecanismo → comprobación

| ID | Invariante y límite | Mecanismo requerido | Evidencia futura que lo puede comprobar |
|---|---|---|---|
| INV-01 | Código de un dominio no accede a memoria ajena sin autoridad; excluye canales laterales y firmware hostil. | MMU, validación de mapas, zeroing, privilegios y DMA según perfil. | EXP-01/05: probes adversarios, fallos, reutilización de RAM y dispositivos. |
| INV-02 | Derivar/copiar nunca aumenta derechos ni escapa de restricciones heredadas. | Tabla local generacional, árbol de grants y checks por tipo. | EXP-02: derivación, transferencia, slots reciclados, profundidad y overflow. |
| INV-03 | Después de una barrera no hay nuevas admisiones ordinarias bajo esa autoridad/ámbito. | Punto conjunto de validación y registro; expiración monotónica. | EXP-02/03: races SMP entre admisión, derivación y cierre. |
| INV-04 | Quiescencia nunca se declara con obligaciones locales pendientes dentro de su perímetro. | Tickets, referencias inversas, acuses TLB/DMA y recibos de servidor. | EXP-03/05: servidor caído, timeout, mapa remoto y DMA bloqueado. |
| INV-05 | Un hijo no multiplica recursos del padre. | Reservas en todos los ancestros y control de paralelismo. | EXP-04: fan-out y workers prestados en varios núcleos. |
| INV-06 | Páginas/metadatos retenidos siguen cobrados hasta liberación o transferencia autorizada. | Patrocinador, cuentas de retención y reclamación diferida. | EXP-04/05: muerte del propietario y retención por lector/dispositivo. |
| INV-07 | Un request no se convierte en trabajo gratuito al entrar a un servidor. | Ámbito efectivo/ticket; cuentas de mantenimiento y cierre separadas. | EXP-04/10: CPU directa, indirecta y recuperación reportadas. |
| INV-08 | Un objeto sellado no conserva un escritor autorizado en el perímetro declarado. | Retirar alias, TLB y DMA antes de publicar sello. | EXP-05: alias hostil y escritura simultánea al sellado. |
| INV-09 | Recibir un mensaje no instala un subconjunto de sus capacidades. | Reserva y commit de entrega conjuntos, metadatos copiados. | EXP-06: memoria/tabla/cola insuficiente en cada punto. |
| INV-10 | Estado privado no altera raíz publicada sin publicación autorizada. | Servicio escritor exclusivo, mapas inmutables y autoridad separada. | EXP-07: herramienta maliciosa, paths alternativos y permisos de publicar ausentes. |
| INV-11 | La validación identifica sus entradas y nunca trata cobertura desconocida como completa. | Versión/toolchain/configuración y resultado tipado. | EXP-07/10: inputs modificados, Cargo.lock, dependencias faltantes. |
| INV-12 | CAS usa generación, no solo igualdad de contenido. | Contador/época y serialización de publicación. | EXP-07: A→B→A y escritor rival. |
| INV-13 | Un ACK durable implica que recuperación conserva raíz, política y recibo de ese commit bajo las hipótesis de disco. | PREPARE, flush de dependencias, COMMIT, flush, respuesta. | EXP-08: cortes/reordenamientos y driver de fallos. |
| INV-14 | Un reintento identificado no publica dos veces y un resultado expulsado no se ejecuta como nuevo. | Secuencia por principal, terminal durable y high-water marks. | EXP-08: respuesta perdida, reinicio, compactación y retry antiguo. |
| INV-15 | Cerrar/rollback no afirma deshacer efectos remotos ya aceptados. | Resultados externos separados, outbox e incertidumbre. | EXP-09: envío aceptado con respuesta perdida y cancelación concurrente. |
| INV-16 | Capacidad/lease de una época no resucita después de reinicio. | Handles efímeros, épocas y reautorización de reglas persistentes. | EXP-02/08: replay de IDs y vencimientos antiguos. |
| INV-17 | Una garantía auditada no oculta pérdidas; cancelar sigue siendo posible con logs llenos. | Reservas antes de admisión y canal/celdas de cierre independientes. | EXP-06/09: consumidor detenido, saturación y fallo de log. |
| INV-18 | Solo el kernel fija origen local y linaje operativo; eso no prueba intención. | Encabezado no sobrescribible y permisos de lectura de logs. | EXP-06/09: payload que falsifica caller/task y acceso a logs de otro ámbito. |
| INV-19 | Un perfil insuficiente no se presenta como cumplimiento del estricto. | Negociación de features y errores antes de efectos. | EXP-10/11: Linux mutable, IOMMU ausente y flush no soportado. |
| INV-20 | Trabajo no confiable no consume la reserva de recuperación. | Capacidades y cuotas separadas; cierre de operaciones reservado. | EXP-04/09: saturación simultánea de CPU, memoria, colas y almacenamiento. |
| INV-21 | Un dominio incompleto nunca ejecuta código de usuario. | Activación única después de mapas, autoridad, recursos y fallos. | EXP-01: omitir cada requisito y comprobar rechazo. |
| INV-22 | El contexto FP/SIMD no filtra datos ni se corrompe entre dominios. | Estado por hilo inicializado, guardado/restaurado y features controladas. | EXP-01/05: patrones distintos y cambios de contexto/fallos repetidos. |
| INV-23 | Acceder a un endpoint no concede todos los objetos del servicio ni conocer un digest concede lectura. | Faceta inmutable emitida con BIND; resolución y validación de objeto/verbo en el servicio. | EXP-02/06/07: falsificar faceta, sustituir objeto, derivar otro permiso y reciclar identidad de sesión. |

Los números de latencia, overhead o tamaño aún no son invariantes. Se medirán como hipótesis; convertir una aspiración de rendimiento en garantía exigiría supuestos y mecanismos adicionales.

## Amenazas

Adversarios previstos: módulo nativo que ejecuta instrucciones arbitrarias; agente que solicita operaciones fuera de autoridad; cliente que falsifica IDs, agota recursos o muere; servidor no crítico que cae; entrada de archivo/mensaje hostil; carreras de varios núcleos; disco que interrumpe/reordena dentro del modelo de fallo declarado.

Un servidor que pertenece al TCB de una propiedad puede violarla si es malicioso. Se reducen sus permisos y radio de daño, pero no se afirma que el kernel pruebe el significado de sus escrituras. Drivers con DMA irrestricto pertenecen al TCB de memoria; con IOMMU pueden seguir perteneciendo al de integridad de sus datos.

No se incluyen en V0 defensa completa ante microarquitectura especulativa, fallos físicos arbitrarios de RAM, hardware/firmware malicioso, eliminación retroactiva de información revelada, rollback físico del disco sin anclaje externo o tiempo real duro. No son excusas para ignorar mitigaciones conocidas: cada perfil debe especificar qué activa y qué queda sin cubrir.

## TCB por propiedad

| Propiedad | Componentes confiables adicionales al hardware/firmware y arranque |
|---|---|
| Memoria entre dominios | Kernel, allocator/MMU/traps/FP; drivers DMA en perfil sin IOMMU. |
| Restricciones de capacidades | Kernel y provisión inicial de raíces; autorización para la política que decide conceder. |
| Intención/consentimiento humano | Servicio de autorización, ruta de entrada y renderizado confiable del consumidor. El modelo no es autoridad. |
| Integridad/publicación de estado | Servicio de estado, protocolo, driver/disco según amenazas, reglas de autorización. |
| Validez de una compilación o inferencia | Herramienta/runtime/modelo y entradas pertinentes; el kernel solo asegura límites operativos. |
| Disponibilidad y cierre | Kernel, supervisor, servicios de recuperación y comportamiento de dispositivos asignados. |
| Provenance semántica | Servicio que emite la afirmación y su cobertura; recibo kernel solo autentica origen local. |

El compilador, linker, dependencias y cadena de construcción forman parte de los supuestos de confianza del binario. «Mayoría de Rust seguro» y «fuera de ring 0» no eliminan esas dependencias.
