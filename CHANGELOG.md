# Historial del proyecto

## 0.3.0 — 2026-09-08

K2 — Objetos, autoridad y trabajo. Primer sistema de capacidades ejecutándose.

- Interfaz V0 generada desde un esquema único: bindings Rust y C, fixtures y comprobador que verifica que lo generado coincide byte a byte con el esquema. Cinco entradas de kernel, incluida la lectura del reloj monotónico sin la cual los plazos de la interfaz no podían expresarse.
- Tabla generacional de objetos y capacidades, ámbitos con límites y ventana de CPU, objetos de memoria con sellado, IPC copiado con invocaciones que llevan origen y cargo, señales, timers, log de control con celdas reservadas y copia de usuario acotada.
- Un único punto de admisión: estructura, autoridad y efecto en ese orden, con autoridad y efecto bajo el mismo cerrojo que toma la barrera. Las 60 operaciones del esquema se despachan desde ahí.
- Arranque K2 seleccionado por el paquete: un módulo `SUPERVISOR` construye la raíz, el log y un solo dominio con manifiesto explícito de capacidades; su ausencia mantiene los dominios de K1.
- Primera vertical: supervisor, servidor y cliente sin permisos ambientales. El cliente estrecha autoridad sobre su propio buffer y la delega; el servidor admite un efecto y retiene la obligación; el supervisor cierra el ámbito del cliente mientras eso ocurre; la autoridad delegada muere con la barrera y la obligación sobrevive a ella hasta ser resuelta.
- Veintisiete controles negativos ejecutados dentro de la vertical, incluidas once peticiones malformadas sin efecto parcial y los límites de tabla, cola y log con el cierre todavía disponible.
- Trabajo atribuible: un servidor se vincula al ticket y adopta el ámbito efectivo de la invocación; después de la barrera la vinculación pasa a recuperación y se carga a la reserva de cierre del servicio, no a la del cliente cerrado.
- Comprobador de la puerta K2: 18 criterios decididos por separado, la mayoría desde los registros del kernel, con la regresión de K1 como criterio propio y una autocomprobación de 20 ejecuciones dañadas.
- Registro de lo ejecutado, su alcance y sus límites en el vault, incluidos los diez defectos que solo aparecieron al ejecutar.

EXP-01 queda ejecutado; EXP-02, EXP-03, EXP-04 y EXP-06 quedan ejecutados en su alcance K2, que es uniprocesador, sin dispositivos y sin estado durable. No se incorporan SMP, drivers, DMA, estado durable ni resultados de rendimiento.

## 0.2.0 — 2026-09-08

K1 — Arranque protegido. Primer código ejecutable del proyecto.

- Workspace Rust con toolchain fijada, dos targets bare-metal y construcción reproducible.
- Loader UEFI con validación acotada de ELF y del paquete de arranque, tablas iniciales y handoff.
- Kernel con posesión de marcos físicos y tablas propias, GDT/IDT/TSS con pilas de emergencia, reloj medido, timer periódico del LAPIC, dominios en espacios separados, planificador con preempción, entrada de syscall y estado FP por dominio.
- ABI de llamadas y runtime de usuario mínimo; cuatro dominios, dos de ellos con accesos ilegales deliberados.
- Imagen arrancable en QEMU con evidencia reproducible byte a byte y manifiesto de digests.
- Comprobador de la puerta K1: 13 criterios decididos por separado desde los registros del kernel, validado con controles negativos.
- Registro de lo ejecutado, su alcance y sus límites en el vault.

El plano de diagnóstico de K1 es temporal y no es el plano de recibos. No se incorporan capacidades, autoridad, IPC, SMP, drivers, estado durable ni resultados de rendimiento.

## 0.1.0 — 2026-09-08

Primera constitución técnica de Thalyx-Kernel, construida desde un repositorio vacío.

- Reconstrucción de Thalyx a partir de código, vault e historial fijados a una revisión.
- Arquitectura derivada: capacidades, ámbitos de trabajo, IPC con atribución y publicación versionada en espacio de usuario.
- Contratos de autoridad, memoria, recursos, fallos, persistencia, hardware y ABI.
- Registro de alternativas, fuentes primarias, invariantes y obligaciones de validación.
- Separación de las rutas Thalyx/Linux y Thalyx/Thalyx-Kernel.
- Secuencia de implementación desde arranque hasta un consumidor real y comparación equivalente.
- Modelos finitos de investigación y revisión de coherencia del vault.

No se incorpora código de kernel, una imagen arrancable ni resultados de rendimiento.
