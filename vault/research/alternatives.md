---
id: RES-002
kind: research
status: accepted
---
# Hipótesis rivales y alternativas

## Cómo se compararon

Los criterios son autoridad comprobable, cierre de trabajo delegado, memoria y recursos sin dueño oculto, estado identificable, radio de fallos, coste de implementación y comportamiento real de Thalyx. No se otorgan puntos por novedad, pureza o reputación.

| Alternativa | Mejor argumento a favor | Objeción concreta para este proyecto | Decisión |
|---|---|---|---|
| Conservar solo Linux y mejorar servicios | Máxima reutilización; snapshots, restricciones y brokers pueden sostener gran parte de la semántica. | No permite investigar desde cero un contrato kernel unificado de autoridad/vida/cargo. | Mantener como ruta y rival fuerte; no sustituye el nuevo proyecto autorizado. |
| Microkernel existente como seL4 | Implementación madura y, bajo alcance preciso, evidencia formal excepcional. | No sería un kernel construido desde cero; modificar su contrato tampoco hereda todas sus pruebas. | Fuente y comparador; no base de código. |
| Monolito nuevo con capacidades | Llamadas internas baratas, más sencilla integración de algunos servicios. | Parsers/drivers con fallo de memoria comparten protección global; no hay evidencia que exija ese coste para Thalyx. | Rechazado como frontera inicial; optimizar rutas calientes antes de ampliar TCB. |
| Exokernel/libOS | Máxima libertad de gestión de recursos para cada consumidor. | Si cada libOS redefine vida y cobro, el trabajo entre servicios recupera el problema de composición; revocación física sigue necesitando arbitraje. | Adoptar separación protección/política, conservar contratos comunes de ámbito. |
| Un solo espacio con aislamiento Rust | Comunicación barata y tipos fuertes para referencias. | Herramientas C/C++, JIT y código nativo ajeno no cumplen ese supuesto; agranda el TCB del compilador/runtime. | Rust para reducir errores, MMU para frontera adversaria. |
| Multikernel desde el inicio | Escala explícita y localidad en máquinas heterogéneas grandes. | Replicar autoridad y cuotas exige consenso/coherencia adicional antes de demostrar necesidad en plataforma inicial. | Diseño SMP sencillo primero; revisar con contención medida. |
| POSIX o syscalls Linux como contrato primario | Toolchains y binarios familiares, menor esfuerzo de port inicial si se implementa fielmente. | Enorme semántica heredada y autoridad/nombres ambientales como interfaz central; una imitación parcial también rompe software. | Compatibilidad de fuente en usuario; no prometer binarios Linux. |
| VM Linux como ejecución principal | Acceso inmediato a drivers y toolchains. | Desplaza autoridad/cargo real de las herramientas al kernel huésped y evita probar el contrato nativo buscado. | Posible servicio de legado posterior; no cuenta como vertical nativa completa. |
| Persistencia transparente de todo el kernel | Reinicio con objetos y procesos aparentemente continuos. | Relojes, tickets remotos, hardware y grants expirados no se restauran simplemente; aumenta estado crítico recuperable. | Estado semántico persistente, capacidades efímeras reconstruidas. |
| Transacciones generales en kernel | Un único coordinador podría abarcar muchas operaciones locales. | El kernel no conoce el significado de validación, política ni efectos remotos; incorporarlos rompe su frontera. | Publicación local en servicio; tickets y memoria como soporte mínimo. |
| Identidad mediante watcher/mtime | Barata y útil para invalidar cachés aproximadas. | No da una instantánea exacta ni descarta ABA/escritores alternativos. | Admitida en perfil débil; versiones inmutables para validación estricta. |
| Reiniciar/matar para revocar | Mecanismo simple para detener instrucciones del cliente. | Servidores, mapas, dispositivos y efectos ya aceptados pueden sobrevivir. | Barrera, drenaje y reclamación separados. |
| Planificación predictiva con un modelo | Puede anticipar carga y mejorar experiencia. | No aporta una cota verificable ni una política confiable bajo errores del predictor. | Hints en usuario dentro de techos; mecanismo determinista de recursos. |
| Determinismo nativo universal | Replay más fuerte para diagnóstico y auditoría. | Restricciones y registro costosos para memoria compartida, GPU, herramientas y red. | Reejecución identificada y runtime restringido opcional. |
| CHERI obligatorio al arrancar | Capacidades de memoria más finas que páginas en hardware adecuado. | Disponibilidad/portabilidad y toolchain inicial; no resuelve autorización semántica. | Investigación posterior, sin bloquear x86_64. |

## Objeción más fuerte a la elección

El nuevo microkernel puede terminar desplazando complejidad a servidores y añadiendo IPC, sin reducir el TCB de las propiedades que más le importan a Thalyx. Además, el port de herramientas puede dominar el coste del proyecto.

La respuesta es medible: mantener TCB por propiedad; construir pronto una vertical de petición, versión y publicación; medir cuentas y rutas; portar herramientas reales antes de afirmar utilidad. Si la parte valiosa resulta ser principalmente el servicio de estado, debe reconocerse y mantenerse también sobre Linux. Eso no invalida la exploración del contrato kernel, pero sí impide atribuirle méritos ajenos.

## Qué no es una decisión cerrada por prestigio

SeL4 no impone una copia de su modelo temporal; Capsicum muestra que las capacidades no obligan a una escuela; Exokernel cuestiona política innecesaria; RedLeaf delimita una alternativa de lenguaje; Barrelfish muestra costes reales de coordinación. La arquitectura elegida combina únicamente mecanismos que responden a invariantes de este consumidor.
