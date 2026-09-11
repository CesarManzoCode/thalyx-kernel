/* Link script for native programs on the x86_64-thalyx target.
 *
 * Three page-aligned PT_LOAD segments so the kernel maps each with the rights
 * its program header declares -- a single mixed RWX segment would make W^X
 * unenforceable at the page level no matter what the kernel did -- plus two
 * that are not loaded and exist for what runs inside the image:
 *
 *   - PT_TLS describes the thread-local template. The kernel ignores it; the
 *     runtime copies the template below each thread's thread pointer and reads
 *     its size from the symbols defined here.
 *   - PT_GNU_EH_FRAME points at the exception frame index. The kernel ignores
 *     it too; GCC's unwinder finds it through `_dl_find_object`, which answers
 *     with `__eh_frame_hdr_start`.
 *
 * Constructors are kept, in priority order, and run by the runtime before the
 * program's `th_main`. They sit in the read-only segment: this image is linked
 * at its final address and nothing relocates it, so the arrays are never
 * written after the linker wrote them.
 */
ENTRY(_start)

PHDRS
{
    text     PT_LOAD FLAGS(5);          /* R + X */
    rodata   PT_LOAD FLAGS(4);          /* R     */
    data     PT_LOAD FLAGS(6);          /* R + W */
    tls      PT_TLS FLAGS(4);
    ehframe  PT_GNU_EH_FRAME FLAGS(4);
}

SECTIONS
{
    . = 0x0000000000400000;
    __image_start = .;

    .text : ALIGN(4096) {
        KEEP(*(.thalyx.image))
        *(.text.startup)
        *(.text.unlikely .text.unlikely.*)
        *(.text.hot .text.hot.*)
        *(.text .text.* .gnu.linkonce.t.*)
    } :text

    . = ALIGN(4096);
    .rodata : ALIGN(4096) {
        *(.rodata .rodata.* .gnu.linkonce.r.*)
    } :rodata

    .eh_frame_hdr : {
        __eh_frame_hdr_start = .;
        *(.eh_frame_hdr)
    } :rodata :ehframe

    .eh_frame : ONLY_IF_RO {
        KEEP(*(.eh_frame))
        *(.eh_frame.*)
    } :rodata

    .gcc_except_table : {
        *(.gcc_except_table .gcc_except_table.*)
    } :rodata

    .preinit_array : {
        PROVIDE_HIDDEN(__preinit_array_start = .);
        KEEP(*(.preinit_array))
        PROVIDE_HIDDEN(__preinit_array_end = .);
    } :rodata

    .init_array : {
        PROVIDE_HIDDEN(__init_array_start = .);
        KEEP(*(SORT_BY_INIT_PRIORITY(.init_array.*) SORT_BY_INIT_PRIORITY(.ctors.*)))
        KEEP(*(.init_array .ctors))
        PROVIDE_HIDDEN(__init_array_end = .);
    } :rodata

    .fini_array : {
        PROVIDE_HIDDEN(__fini_array_start = .);
        KEEP(*(SORT_BY_INIT_PRIORITY(.fini_array.*) SORT_BY_INIT_PRIORITY(.dtors.*)))
        KEEP(*(.fini_array .dtors))
        PROVIDE_HIDDEN(__fini_array_end = .);
    } :rodata

    .got : {
        *(.got .got.plt)
    } :rodata

    .data.rel.ro : {
        *(.data.rel.ro.local* .data.rel.ro .data.rel.ro.*)
    } :rodata

    . = ALIGN(4096);
    .data : ALIGN(4096) {
        *(.data .data.* .gnu.linkonce.d.*)
    } :data

    /* The thread-local template. `.tbss` occupies no memory in the image;
     * the runtime reads its size from `__tbss_end`. */
    .tdata : ALIGN(16) {
        __tdata_start = .;
        *(.tdata .tdata.* .gnu.linkonce.td.*)
        __tdata_end = .;
    } :data :tls

    .tbss : {
        *(.tbss .tbss.* .gnu.linkonce.tb.*)
        *(.tcommon)
    } :data :tls
    __tbss_end = ADDR(.tbss) + SIZEOF(.tbss);
    __tls_align = MAX(ALIGNOF(.tdata), ALIGNOF(.tbss));

    .bss : ALIGN(16) {
        __bss_start = .;
        *(.bss .bss.* .gnu.linkonce.b.*)
        *(COMMON)
        . = ALIGN(16);
        __bss_end = .;
    } :data

    . = ALIGN(4096);
    __image_end = .;

    /DISCARD/ : {
        *(.comment)
        *(.note .note.*)
        *(.debug_gdb_scripts)
        *(.gnu.warning)
    }
}
