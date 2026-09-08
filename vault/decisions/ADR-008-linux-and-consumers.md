---
id: ADR-008
kind: decision
status: accepted
---
# Dos rutas de Thalyx y una frontera reutilizable

**Problema.** Un kernel hecho para un consumidor puede quedar acoplado a su aplicación. A la inversa, intentar servir a cualquier sistema puede diluir las necesidades que lo justifican.

**Evidencia.** El mandato del proyecto establece independencia, Thalyx como referencia y conservación de Linux. EVD-003 muestra mejoras de servicio que son posibles en ambas plataformas.

**Elección.** Mantener Linux y comparar perfiles equivalentes. Kernel con vocabulario de protección/trabajo, protocolos de plataforma con semántica propia y Thalyx como primer cliente real. Un segundo consumidor no está obligado a usar el grafo, runtime o UI de Thalyx.

**Alternativas descartadas.** Retirar Linux al arrancar el kernel; usar benchmarks de superficies diferentes para atribuir rendimiento; objetos kernel Agent/Prompt/Module; añadir compatibilidad genérica sin un requisito concreto.

**Consecuencias.** Hay dos backends que mantener y una disciplina experimental más exigente. Una mejora portable de estado se atribuye al servicio. La independencia de repositorios no impide fijar pares de revisiones para reproducir resultados.

**Revisión.** Un segundo consumidor real puede motivar cambios de protocolo; se exige un uso concreto y preservar invariantes. No se sacrifica una propiedad de Thalyx para una generalidad hipotética.

**Referencias.** [Integración](../integration/thalyx.md), [comparación](../integration/linux-comparison.md), [frontera Linux](../evidence/linux-boundary.md).
