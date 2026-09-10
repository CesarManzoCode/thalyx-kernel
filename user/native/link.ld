/* Link script for native C programs on the x86_64-thalyx target.
 *
 * Same shape as user/link.ld -- three page-aligned PT_LOAD segments so the
 * kernel maps each with the rights its program header declares -- with the
 * section names a C toolchain emits added. A single mixed RWX segment would
 * make W^X unenforceable at the page level no matter what the kernel did.
 *
 * .eh_frame and .note are discarded: nothing below this program unwinds, and
 * the kernel's bounded ELF loader accepts at most eight loadable segments.
 */
ENTRY(_start)

PHDRS
{
    text   PT_LOAD FLAGS(5); /* R + X */
    rodata PT_LOAD FLAGS(4); /* R     */
    data   PT_LOAD FLAGS(6); /* R + W */
}

SECTIONS
{
    . = 0x0000000000400000;

    .text : ALIGN(4096) {
        KEEP(*(.thalyx.image))
        *(.text.startup)
        *(.text .text.* .gnu.linkonce.t.*)
    } :text

    . = ALIGN(4096);
    .rodata : ALIGN(4096) {
        *(.rodata .rodata.* .gnu.linkonce.r.*)
        *(.got .got.plt)
        *(.data.rel.ro .data.rel.ro.*)
    } :rodata

    . = ALIGN(4096);
    .data : ALIGN(4096) {
        *(.data .data.* .gnu.linkonce.d.*)
    } :data

    .bss : ALIGN(16) {
        __bss_start = .;
        *(.bss .bss.* .gnu.linkonce.b.*)
        *(COMMON)
        . = ALIGN(16);
        __bss_end = .;
    } :data

    /DISCARD/ : {
        *(.eh_frame)
        *(.eh_frame_hdr)
        *(.comment)
        *(.note .note.*)
        *(.init_array .fini_array)
        *(.debug_gdb_scripts)
    }
}
