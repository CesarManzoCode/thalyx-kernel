---
id: ADR-017
kind: decision
status: accepted
---
# La libc nativa copia y llena con instrucciones de cadena y conserva bloques pequeños, `calloc` no vuelve a limpiar memoria recién entregada, el motor no calcula un digest que no escribe y una página de imagen se copia sin ponerla antes a cero

**Problema.** En el baseline de K7 (`vault/private/baseline-k7.json`) las razones nativo / `linux-nomitig` de K6 para el motor residente son, réplicas A y B: `engine.load` 2,40 y 2,66; `engine.infer` con los prompts 0, 1 y 4 entre 1,097 y 1,105; `engine.cancel` 2,47 y 2,73. En la campaña A, `engine.load` es del orden de 3,0 a 3,4 ms en Linux, `engine.infer:0` de 1,63 ms y `engine.cancel` de 4,5 a 4,8 ms. Los protocolos están en `user/k6super/src/entries.rs` y `tests/k6/plat_native.c`. El motor es `user/nengine/engine.cpp` sobre llama.cpp y la libc de `user/native`. La respuesta byte a byte del motor es un criterio de la puerta K6 y no cambia.

**Evidencia.** Observado en el código, en el árbol de `023028d`:

- El modelo de la fixture es pequeño: 470 304 bytes, dos capas y 64 de ancho de embedding (`build/reference/tiny.gguf`). Así que una inferencia está dominada por lo que rodea al cálculo, como copias, rellenos, asignaciones y entradas al kernel, más que por el producto de matrices.
- `memcpy` y `memset` (`user/native/src/string.c`) avanzaban ocho bytes por paso a cualquier tamaño. `memmove` con el destino dentro del origen copiaba byte a byte, y `memcmp` comparaba byte a byte. El brazo Linux usa los de glibc. Cada petición vacía la memoria de contexto del motor (`llama_memory_clear(..., true)`), un `memset` de unos 256 KiB con 512 tokens de contexto, dos capas y K y V en f16.
- `free` (`user/native/src/heap.c`) coalescía cada bloque con sus vecinos y `malloc` lo volvía a partir. `bin_of` buscaba el bin con un bucle de hasta 24 pasos por llamada. El muestreo con gramática que usa `engine.cancel` asigna y libera vectores pequeños en cada token.
- `calloc` ponía a cero lo que `malloc` le daba aunque viniera de una arena recién creada, que el kernel entrega a cero (`FrameAllocator::alloc*`, contrato de [memoria](../architecture/memory.md)).
- `th_main` del motor evaluaba `fnv` sobre todo el modelo como argumento de `say` aunque el motor estuviera callado. Ese FNV-1a hace una multiplicación encadenada por byte, y K6, que corre el motor callado, no escribía la nota. Ese cálculo cae dentro de lo que `engine.load` mide.
- `domain::build` (`kernel/src/domain.rs`) pedía un marco a cero por cada página de un segmento ELF y después copiaba encima los bytes del archivo. La imagen del motor mapea unas 1 200 páginas, casi todas de archivo, así que la mayor parte de esas páginas se escribía dos veces.

**Elección.**

*Copias y rellenos.* `memcpy` y `memset` usan `rep movsb` y `rep stosb` desde 256 bytes. Por debajo avanzan dieciséis bytes por paso. `memmove` copia hacia delante en todo caso salvo uno: el destino empieza dentro del origen. En ese caso copia desde el final, ocho bytes por paso, y cada paso lee sus ocho bytes antes de escribirlos. `memcmp` compara ocho bytes por paso mientras son iguales y busca la diferencia byte a byte dentro de la palabra, así que el signo es el del primer byte distinto. Las cuatro funciones llevan `no-tree-loop-distribute-patterns` para que el compilador no convierta sus bucles en una llamada a ellas mismas. El resultado de cada llamada es el mismo.

*Bloques pequeños conservados.* `free` guarda los bloques de hasta 544 bytes en una lista por tamaño, de hasta 64 cada una, y `malloc` devuelve uno de esos si tiene el tamaño pedido. Un bloque conservado sigue marcado en uso, así que sus vecinos no se coalescen con él. Antes de crecer, y antes de rechazar, `allocate` devuelve todos los conservados a los bins, con la misma coalescencia, y vuelve a buscar. Así un programa puede asignar lo mismo que antes. `bin_of` usa `clz`, con los mismos bins. `heap_in_use` no cuenta los bloques conservados.

*`calloc` sobre memoria recién entregada.* El asignador recuerda qué parte de la arena más nueva no se ha repartido todavía. Esa parte es cero salvo la cabecera y los enlaces del bloque libre que empieza en ella. Si el bloque que `calloc` recibe sale de esa parte, solo pone a cero los primeros dieciséis bytes, que son esos enlaces.

*El digest del modelo.* El motor calcula el FNV del modelo solo cuando escribe su nota, es decir, sin `arg2`. K5 sigue recibiendo la nota con el mismo valor. K6 no la recibía antes ni la recibe ahora.

*Páginas de imagen.* `FrameAllocator::alloc_filled` toma un marco como `alloc`, copia en él los bytes dados y pone a cero el resto. `alloc` es `alloc_filled` sin bytes. `domain::build` le pasa los bytes de archivo de cada página. El contenido de cada marco al quedar alcanzable es el mismo que antes, y cada byte se escribe antes de que el marco sea alcanzable. La regla del contrato de memoria se cumple igual: las páginas nuevas no llevan información residual.

**Autocomprobación.** No hay un registro nuevo ni un checker nuevo. La puerta K6 ya compara la respuesta del motor byte a byte con la referencia en cada muestra de `engine.infer`. Una copia, un relleno o una asignación que cambiara un resultado aparecería ahí como `mismatch`. La puerta K5 lee el digest del modelo que el motor no callado escribe, y la nota `ENGINE_SCOPE_PAGES` sigue diciendo cuánta memoria retiene el motor. Las puertas K1 a K6 cargan todas sus imágenes con `domain::build`, así que una página de imagen mal copiada haría fallar el arranque de un dominio.

**Diseñado, no medido.** Se busca bajar `k7.k6_engine_load_0`, por el digest que ya no se calcula, la copia sin cero previo de las páginas de imagen, `calloc` y los rellenos con `rep stosb`. También se busca bajar `k7.k6_engine_infer_0`, `_1` y `_4`, por las copias, los rellenos y los bloques conservados. Y se busca bajar `k7.k6_engine_cancel_0`, que incluye una inferencia completa después de la valla, por lo mismo y por el muestreo con gramática que el motor hace hasta notar la cancelación. No se sabe cuánto pesa cada parte.

Hay causas que este cambio no toca:

- Cada lectura del reloj sigue siendo una entrada al kernel. En Linux esa lectura no entra al kernel (vDSO).
- La memoria del motor se entrega y se pone a cero al crearla, no al tocarla.
- El hilo del motor ligado a la petición cancelada sigue sujeto al presupuesto de medio ciclo del ámbito que pregunta.

Es una hipótesis que esas causas expliquen lo que quede de `engine.load` y `engine.cancel`. Conservar bloques pequeños retiene hasta unos 1,1 MB de bloques sin coalescer en el peor caso, y los devuelve antes de crecer o rechazar. Las cifras de K7 las mide el cierre.
