---
id: ADR-018
kind: decision
status: accepted
---
# El paquete lleva los programas sin secciones de depuración, el enlazador pliega el código idéntico del kernel y el loader se compila para tamaño

**Problema.** En el baseline de K7 (`vault/private/baseline-k7.json`) los tres tamaños son deterministas y dan lo mismo en las dos réplicas. La carga del kernel (suma de `p_memsz` de los `PT_LOAD` de `kernel.elf`) es de 573 440 bytes. El paquete `boot.tbp` de la imagen de medida pesa 5 951 488 bytes y `bootx64.efi`, 64 000 bytes.

**Evidencia.** Observado en los artefactos de `build/thalyx-k1-measure/`, en el árbol de `759bb62`:

- El paquete lleva cuatro programas Rust: `k5super` (1 487 256 bytes), `k4disk` (1 008 720), `k4store` (1 623 216) y `k5link` (1 811 544), más el plan de 56 bytes. En `k5super`, `.debug_info` ocupa 0x13b970 bytes y `.debug_line`, 0x25ce2. Los segmentos cargables suman unos 0x3c000. Ni el loader, ni el validador ELF del kernel, ni ningún checker lee esas secciones del paquete. `release` lleva `debug = true` para todo el espacio de trabajo. `tools/build_native.py` ya quita las secciones de depuración de las imágenes nativas y deja la copia completa al lado.
- El kernel ocupa tres segmentos alineados a página: texto 0x45000, solo lectura 0xb000 y datos 0x3c000. Esos segmentos tienen contenido real hasta 0x4436f, 0xa658 y 0x8b119, así que les sobran 3 217, 2 472 y 3 815 bytes. Para que el texto baje una página hacen falta 880 bytes menos. Para la de solo lectura, 1 624 bytes. Para los datos, 281 bytes; el relleno de alineación de los datos es de solo 225 bytes. Los datos son tablas cuyo tamaño fijan parámetros del contrato, como `MACHINE`, `THREADS`, `CPUS` y `SCOPES`, y no se tocan.
- El kernel se enlaza sin plegado de código idéntico (`kernel/build.rs`). No se encontró en `kernel/src` ninguna comparación de identidad entre direcciones de funciones ni de constantes (`ptr::eq`, `fn_addr_eq`).
- El loader se compila con `opt-level = 3`, como todo `release`. No calcula digests: lee el paquete y el kernel con el firmware, valida y copia.

**Elección.**

*Programas sin depuración en el paquete.* `tools/build_image.py` copia cada programa Rust a `build/<imagen>/<programa>.debug.elf` tal como sale de cargo y le pasa `strip --strip-debug` a la copia que entra al paquete. Los segmentos cargables, la tabla de símbolos y las cabeceras no cambian, así que el kernel carga los mismos bytes. El módulo deliberadamente malformado de K1 se sigue sacando de `worker.elf` con su segmento movido. El digest de cada módulo cambia y el manifiesto registra el nuevo.

*Plegado en el kernel.* `kernel/build.rs` pasa `--icf=all` a `rust-lld`. Las funciones y las constantes de solo lectura idénticas byte a byte quedan una sola vez. Ninguna instrucción que se ejecuta cambia, solo dónde está.

*Loader para tamaño.* `[profile.release.package.thalyx-boot-uefi]` fija `opt-level = "s"`. El kernel y los programas siguen con `opt-level = 3`.

**Autocomprobación.** No hay un registro nuevo ni un checker nuevo. Los tres tamaños los lee `measure_k7.py`. Las puertas K1 a K6 cargan todos sus módulos con el validador ELF del kernel, así que una copia sin depuración que perdiera un segmento haría fallar el arranque de un dominio. La K1 sigue viendo el rechazo del módulo malformado. Si el plegado juntara dos funciones que el kernel distingue por dirección, se vería en las puertas.

**Diseñado, no medido.** Se busca bajar `k7.size_package`, porque las secciones de depuración son la mayor parte de cada programa, y `k7.size_loader`. Que el plegado baje `k7.size_kernel_load` es una hipótesis. El tamaño cargado se mueve por páginas, y solo baja si lo plegado en un segmento pasa de lo que le falta para perder una página, por ejemplo 880 bytes en el texto. Si no alcanza, este cambio no mueve esa métrica. Costos conocidos: una dirección del kernel puede corresponder a varias funciones plegadas, así que la línea que se resuelva para una falla puede nombrar a otra función con el mismo código. Además, un loader compilado para tamaño puede tardar algo más, lo que cuenta en `k7.boot_guest_ms`. Las cifras de K7 las mide el cierre.
