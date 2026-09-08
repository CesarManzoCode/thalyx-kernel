/* Link script shared by every K1 user domain.
 *
 * Segments are split and page-aligned so the kernel can map each one with the
 * rights the ELF program header declares. A single mixed RWX segment would make
 * W^X unenforceable at the page level no matter what the kernel did.
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
        *(.text .text.*)
    } :text

    . = ALIGN(4096);
    .rodata : ALIGN(4096) {
        *(.rodata .rodata.*)
        *(.got .got.plt)
    } :rodata

    . = ALIGN(4096);
    .data : ALIGN(4096) {
        *(.data .data.*)
    } :data

    .bss : ALIGN(16) {
        *(.bss .bss.*)
        *(COMMON)
    } :data

    /DISCARD/ : {
        *(.eh_frame)
        *(.eh_frame_hdr)
        *(.comment)
        *(.note .note.*)
    }
}
