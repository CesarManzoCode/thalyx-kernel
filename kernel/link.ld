/* Kernel image layout.
 *
 * The image is linked at -2 GiB so the `kernel` code model of
 * x86_64-unknown-none is valid, and every segment is page aligned with its own
 * program header so the loader and the kernel can map text, read-only data and
 * writable data with different rights. A single segment would make W^X
 * unenforceable for the kernel's own image.
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
    . = 0xFFFFFFFF80000000;
    __thalyx_image_start = .;

    .text : ALIGN(4096) {
        __thalyx_text_start = .;
        *(.text .text.*)
        . = ALIGN(4096);
        __thalyx_text_end = .;
    } :text

    . = ALIGN(4096);
    .rodata : ALIGN(4096) {
        __thalyx_rodata_start = .;
        *(.rodata .rodata.*)
        *(.got .got.plt)
        . = ALIGN(4096);
        __thalyx_rodata_end = .;
    } :rodata

    . = ALIGN(4096);
    .data : ALIGN(4096) {
        __thalyx_data_start = .;
        *(.data .data.*)
    } :data

    .bss : ALIGN(16) {
        *(.bss .bss.*)
        *(COMMON)
        . = ALIGN(4096);
        __thalyx_data_end = .;
    } :data

    __thalyx_image_end = .;

    /DISCARD/ : {
        *(.eh_frame)
        *(.eh_frame_hdr)
        *(.comment)
        *(.note .note.*)
    }
}
