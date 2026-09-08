# Thalyx-Kernel

Thalyx-Kernel es un kernel nuevo para sistemas que ejecutan trabajo delegado a agentes bajo autoridad, recursos y efectos explícitamente delimitados. Thalyx es su consumidor de referencia. El proyecto es independiente de Thalyx y conserva Linux como plataforma de producción, investigación y comparación de ese consumidor.

La arquitectura fundacional elegida es un **microkernel de capacidades con ámbitos de trabajo explícitos**. Separa protección, ejecución, autoridad y consumo de recursos; permite que un trabajo atraviese servicios sin perder su identidad operativa. Un servicio de estado en espacio de usuario proporciona versiones inmutables, publicación condicional y recuperación. La interpretación de intenciones, la política humana, los modelos y el conocimiento pertenecen al sistema consumidor.

**Estado: constitución técnica 0.1.0; diseño aceptado para iniciar implementación. No existe todavía un kernel ejecutable.** Los modelos de investigación incluidos no son una implementación del kernel ni una prueba general de sus propiedades.

- [Leer el vault fundacional](vault/README.md).
- [Entender las decisiones y sus límites](vault/constitution.md).
- [Empezar a implementar](vault/roadmap/phases.md).
- [Consultar el estado y la evidencia real](vault/roadmap/current-state.md).

El análisis de Thalyx está fijado al commit `0492f72e487e2463b0d7b938365a8b3383364cb9`, consultado el 8 de septiembre de 2026. Las afirmaciones históricas, las decisiones nuevas y lo pendiente de demostrar se mantienen separados.

El vault usa Markdown y enlaces relativos compatibles con GitHub y Obsidian. No requiere extensiones, servicios externos ni una configuración privada del editor.
