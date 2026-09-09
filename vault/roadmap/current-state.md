---
id: STATE-001
kind: evidence
status: observed
---
# Estado actual

**2026-09-08 · Fundación 0.1.0 · K0 y K1 completos. K2 en curso: sustrato del kernel construido, vertical de usuario pendiente.**

## Qué existe

Un vault de 40 notas con constitución, 13 contratos de arquitectura, glosario, reconstrucción de Thalyx, 32 fuentes primarias anotadas, ocho decisiones, 23 invariantes, alternativas, integración Linux/nativa, experimentos y ruta de implementación. Se incluyen herramientas documentales y dos modelos finitos de investigación con controles negativos y resultado versionado.

Existe además un kernel que arranca. El workspace tiene loader UEFI, protocolo de arranque, kernel con `arch/x86_64`, ABI de llamadas y cuatro programas de usuario, con toolchain fijada y construcción reproducible. La imagen arranca en QEMU, ejecuta dominios en ring 3, los preempta con timer, contiene sus fallos ilegales y sobrevive. [Qué se ejecutó exactamente](../evidence/k1-protected-boot.md).

La base de evidencia de Thalyx está fijada al commit `0492f72e487e2463b0d7b938365a8b3383364cb9`: inventario de 407 commits alcanzables, 96 archivos del vault, 38 rutas de código/configuración de evidencia y 15 revisiones históricas seleccionadas. Inventariar no significa haber ejecutado ni auditado exhaustivamente cada archivo.

El repositorio Thalyx se mantuvo sin modificaciones durante este trabajo.

## Qué está decidido

Microkernel de capacidades con ámbitos de trabajo; dominios de memoria como frontera adversaria; autoridad por grants/facetas; IPC con origen y cargo; barrera/drenaje/retirada separados; memoria sellada; CPU con cuotas agregadas y recuperación reservada; estado versionado y publicación durable en usuario; x86_64/UEFI y Rust; ABI propio; port de fuente y Linux permanente.

Las decisiones son contratos para implementar, no resultados del sistema. [Registro de decisiones](../decisions/README.md).

## K2 en curso

### Interfaz V0

`abi/schema/v0.json` es la única fuente de la interfaz. `tools/gen_abi.py` deriva de ella los bindings Rust y C y las fixtures; `tools/check_abi.py` comprueba que lo generado coincide byte a byte con el esquema, que ningún número significa dos cosas, que las 325 aserciones de disposición en C se cumplen y que 15 fixtures decodifican en 669 desplazamientos. Cuatro de cuatro comprobaciones pasan.

La tabla asigna cuatro entradas de kernel —consulta de versión, invocación, consulta de límites y salida— y 60 operaciones repartidas en nueve tipos de objeto.

### Sustrato del kernel

El kernel implementa los mecanismos K2 del lado del núcleo: tabla generacional de objetos y capacidades (`obj.rs`), ámbitos con límites, ventana de CPU, barrera, drenaje y retirada (`scope.rs`), objetos de memoria con sellado y copia (`memobj.rs`), IPC copiado con invocaciones, origen y cargo (`ipc.rs`), señales y timers (`events.rs`), log de control con celdas reservadas (`ctrl.rs`), copia de usuario acotada (`ucopy.rs`) y las capacidades de arranque del primer supervisor (`k2boot.rs`).

`kernel/src/api/` es el único punto de admisión: estructura, autoridad y efecto se comprueban en ese orden, y los pasos de autoridad y efecto ocurren bajo una sola toma del cerrojo de la máquina, que la barrera también toma. Las 60 operaciones del esquema tienen manejador y se despachan desde ahí.

`kernel/src/syscall.rs` conecta las cuatro entradas asignadas con ese punto de admisión. Las dos entradas de andamiaje de K1 siguen presentes, marcadas como tales y sin tocar ningún objeto, para que la regresión de K1 se siga ejecutando contra el kernel en el que K2 creció.

El arranque elige la ruta por el paquete: si un módulo declara `SUPERVISOR`, el kernel construye la raíz, el log de control y ese único dominio con su manifiesto explícito de capacidades; si no, cae en los dominios de K1.

## Evidencia ejecutada aquí

| Comprobación | Resultado y alcance |
|---|---|
| Reconstrucción de Thalyx | Lectura estática de código, vault, historial y pruebas existentes. Sin build ni ejecución de Thalyx. |
| MODEL-01 | 294 estados / 615 transiciones, con contraejemplos de las afirmaciones/variantes incorrectas. |
| MODEL-02 | 23 casos de caída del protocolo correcto; variantes incorrectas detectadas. |
| Caso ABA | Confirma la necesidad de generación para rechazar expectativas antiguas sobre contenido repetido. |
| Revisión arquitectónica | Hallazgos y correcciones registrados en [la auditoría](../validation/audit.md). |
| Integridad documental | PASS: 40 IDs y enlaces locales. [Comprobador](../../tools/check_vault.py). Además, 36 destinos de código/historia de Thalyx resueltos contra Git. |
| Esquema ABI | PASS en 4 comprobaciones. [Comprobador](../../tools/check_abi.py). |
| Puerta K1 | PASS en 13 criterios decididos por separado desde los registros del kernel, con controles negativos. [Detalle y límites](../evidence/k1-protected-boot.md). |
| Regresión K1 sobre el sustrato K2 | PASS en los mismos 13 criterios con los mecanismos K2 compilados dentro del kernel. |

## Qué no existe todavía

Del lado de K2 falta lo que da sentido a los mecanismos: no hay programa supervisor, ni servidor, ni cliente, así que **ninguna operación K2 se ha ejecutado desde ring 3**. Sin eso no hay vertical, no hay evidencia EXP-01/02/03/04/06 en alcance UP, no hay controles negativos ejecutados y no hay puerta K2. El empaquetado de imagen todavía no emite módulos de tipo `SUPERVISOR`, de modo que la ruta de arranque K2 del kernel no se ha tomado nunca en una ejecución real.

Que los 60 manejadores compilen y que el despacho los alcance no dice nada sobre si hacen lo que su contrato exige. Esa distinción es el trabajo que queda.

Más allá de K2: SMP, drivers propios, DMA, servicio de estado implementado, Thalyx sobre este kernel, pruebas de hardware físico, mediciones de rendimiento o prueba formal general. El plano de diagnóstico de K1 es temporal y no es el plano de recibos. No se ha retirado ni reemplazado Linux.

## Siguiente trabajo

Construir el lado de usuario de K2 y ejecutarlo: runtime de usuario sobre las cuatro entradas, programa supervisor que reciba el manifiesto de arranque y cree el resto por la interfaz, empaquetado de imagen con módulo `SUPERVISOR`, y la primera vertical —cliente sin permisos ambientales que solicita una operación, deriva autoridad, consume presupuesto y se cancela mientras el servidor retiene trabajo—. Después, controles negativos y puerta K2 independiente.

No hay una elección técnica pendiente que deba devolver el diseño al usuario. [Las preguntas abiertas](open-questions.md) especifican qué dato falta y con qué decisión conservadora avanzar.
