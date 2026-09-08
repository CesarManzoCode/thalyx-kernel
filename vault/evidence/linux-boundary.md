---
id: EVD-003
kind: analysis
status: accepted
---
# Qué limita Linux y qué limita la composición actual

No se justifica un kernel nuevo inventando incapacidades de Linux. Esta matriz distingue hechos observados de costes arquitectónicos e hipótesis.

| Necesidad | Situación sobre Linux | Naturaleza del límite | Respuesta elegida |
|---|---|---|---|
| Autoridad de objetos sin nombres ambientales | Descriptores, `openat2`, seccomp, LSM y Landlock permiten construir confinamiento. | Composición y cobertura del perfil; Linux no expone una única semántica de delegación y revocación para todos esos objetos. | Tabla de capacidades y contrato común de admisión; nombres en servicios. |
| Vencimiento de cada grant | La política BPF inspeccionada agrega permisos por cgroup; brokers y recursos abiertos requieren tratamiento propio. | Limitación de esta implementación; no prueba de imposibilidad en Linux. | Restricciones por delegación y barrera explícita; servicios mantienen responsabilidad por operaciones ya admitidas. |
| Coste de una petición que cruza servidores | Cgroups acotan grupos de procesos. Un servicio puede implementar cuotas por petición y distribuir trabajo a workers. | Desajuste entre agrupación de procesos y principal de trabajo; mover procesos no reasigna automáticamente todos los cargos. | Ámbito y ticket atraviesan IPC; memoria compartida tiene patrocinador explícito. |
| Identidad exacta de entradas | Snapshots administrados y servicios con escritor exclusivo pueden ofrecerla. Un árbol mutable recorrido por hashes no es una foto atómica. | Exclusividad y protocolo de aplicación; no ausencia de snapshots en Linux. | Perfil de estado administrado con versiones inmutables en ambas plataformas. |
| Publicación y recuperación | Renames, fsync, Btrfs, bases de datos y journals proporcionan componentes suficientes bajo sus contratos. | Composición de visibilidad, dependencias y durabilidad. | Servicio único de raíz/publicación; mismo contrato implementable sobre Linux. |
| Aislamiento de drivers | VFIO/IOMMU, procesos y virtualización permiten separar casos concretos. | Cobertura de hardware, grupos IOMMU y complejidad del TCB; no aislamiento automático de cualquier dispositivo. | Drivers de usuario con asignación controlada y perfil que rechaza DMA inseguro. |
| Observabilidad | Tracepoints, BPF, audit y provenance pueden registrar eventos del sistema. | Cobertura, pérdidas y significado de eventos; un hook no demuestra intención. | Separar recibos de control, historial durable y trazas diagnósticas. |
| Compatibilidad y velocidad de desarrollo | Linux ya soporta toolchains, drivers, memoria virtual, SIMD y aplicaciones reales. | Ventaja sustancial del sistema existente. | Mantenerlo y presupuestar explícitamente el trabajo de portabilidad nuevo. |

## El argumento positivo

Thalyx-Kernel compra control sobre el contrato de protección: puede hacer que la admisión de una petición valide simultáneamente capacidad, vida del ámbito, límites y espacio para evidencia. Puede definir desde el principio qué significa cerrar trabajo delegado y cómo conservar cargos cuando muere un cliente.

Eso no vuelve simples los servidores ni elimina carreras en persistencia. Cambia qué conexiones puede comprobar el kernel y cuáles debe implementar Thalyx. La hipótesis de valor es una reducción de **complejidad de composición y ambigüedad**, acompañada de aislamiento y costes medibles; no «Linux no sirve para IA».

## Comparaciones prohibidas por falta de equivalencia

- Contraponer un árbol Linux modificable por el host a un almacén nativo exclusivamente administrado y atribuir toda diferencia al kernel.
- Llamar «revocación inmediata» a matar un proceso sin comprobar servidores, sockets y DMA pendientes.
- Comparar Thalyx con un agente usando herramientas shell y presentar el resultado como Linux frente a Thalyx-Kernel.
- Restar del TCB nativo todos los servicios de usuario y contar en Linux componentes que también operan en usuario.
- Confundir una VM emulada con rendimiento físico; ni TCG ni diferente aceleración CPU son controles válidos.

Las fuentes normativas de los mecanismos Linux se recogen en [S14–S20 y S28–S30](../research/sources.md). El [protocolo comparativo](../integration/linux-comparison.md) permite que Linux adopte las mismas mejoras de servicio.
