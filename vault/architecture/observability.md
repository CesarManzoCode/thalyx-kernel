---
id: ARC-009
kind: contract
status: designed
---
# Evidencia, auditabilidad y provenance

## Tres planos

| Plano | Fuente de verdad | Garantía y límite |
|---|---|---|
| Control local | Recibos del kernel de admisión, barrera y vida de objetos/invocaciones. | Origen y orden dentro de una época, con perfil de captura declarado; no durabilidad tras caída. |
| Publicación semántica | Log del servicio de estado con raíz, política y resultado. | Resultado durable bajo sus hipótesis de disco y TCB; no describe cada instrucción ejecutada. |
| Diagnóstico | Trazas, contadores, muestras y logs de servicios. | Rendimiento y depuración; puede perder eventos y debe declararlo. |

El plano de diagnóstico distingue, desde K6, registros de **traza** —uno por operación u objeto— y de **resumen** —qué es el kernel, qué encontró al arrancar, qué falló, qué anotó un programa y qué sumó todo al final—. Un paquete que mide pide solo los resúmenes, con una bandera en su módulo supervisor, y el resumen final dice cuántos registros de traza se retuvieron; los paquetes de las puertas K1–K5 reciben las dos clases. Es la forma concreta de «puede perder eventos y debe declararlo»: bajo KVM un registro costaba tanto como mil de las operaciones que describía, y una medida tomada con la traza puesta era una medida de la traza. [ADR-010](../decisions/ADR-010-k6-parameters-and-wake-policy.md).

No se denomina «auditado» a una operación solo porque había un ring buffer al que se intentó escribir.

## Recibos de control

Un recibo incluye versión de esquema, época, secuencia, tipo, objeto/grant involucrado, ámbito de origen, invocación padre si existe, resultado y tiempo monotónico. No incorpora por defecto payloads, prompts, secretos ni bytes de archivos.

El perfil `audited-control` reserva espacio antes de admitir una operación cubierta. Si falta capacidad, esa operación se rechaza antes del efecto. La cola y las reservas se cobran; el lector debe consumirla. El perfil ordinario puede muestrear diagnóstico, pero no declara una historia completa.

La revocación y liberación tienen celdas/slots de control reservados que no compiten con el tráfico ordinario. El último estado de barrera y contadores pendientes se mantienen en el objeto preasignado. Una cola llena nunca impide cerrar autoridad. Los detalles repetidos pueden coalescerse en ese canal de emergencia, con rango/secuencia explicitados; no se vende como un evento durable por cada llamada.

El API permite consultar «qué cobertura estaba activa y dónde hubo pérdidas». El consumidor que necesita un hecho durable debe incorporarlo mediante su protocolo de persistencia. No se hace fsync en cada syscall ni se bloquea todo el kernel esperando un daemon de logs.

## Causalidad que se puede sostener

Una invocación hija conserva padre y origen; un servicio asíncrono mantiene el ticket al encolarla. Esto prueba relaciones de delegación observadas. No prueba que todos los bytes que influyeron en una decisión pasaran por esa cadena.

Los contadores por CPU y las relaciones de mensajes forman un orden parcial. Solo las publicaciones de un almacén tienen un orden total propio. Un timestamp mayor no demuestra causalidad, y relojes de dos máquinas no producen por sí mismos un orden de efectos.

Un servicio puede emitir provenance semántica con entradas, algoritmo, versión y cobertura. El kernel autentica el dominio emisor del recibo, no su veracidad científica. Una validación incompleta se conserva como incompleta, aunque la generación del texto parezca segura.

## Seguridad y privacidad del observador

Leer registros requiere una capacidad específica. Un usuario de una tarea no obtiene automáticamente logs de otras; la autoridad de administración puede recibir una vista ampliada. Se evita copiar datos sensibles al log por comodidad de depuración.

El buffer de observabilidad es memoria con cuota y política de retención. Reiniciar cambia época; un lector no concatena números de secuencia reiniciados como una historia continua. El root de administración y un servicio dentro del TCB pueden alterar lo que controlan; no se promete anti-tampering contra ellos sin un anclaje de confianza adicional.

## Qué se comprueba

Se saturan colas para comprobar rechazo previo al efecto, se pierde al consumidor para comprobar recuperación, se intercalan llamadas entre varios núcleos para comprobar origen y orden parcial y se reinicia para comprobar discontinuidad explícita.

Un resultado de Thalyx deberá poder contestar: qué versión leyó, qué operaciones se admitieron, quién autorizó publicar, qué se publicó, qué quedó sin comprobar y qué evidencia falta. Si alguna respuesta depende de inferir intención desde lenguaje, ese límite se muestra en el nivel consumidor.
