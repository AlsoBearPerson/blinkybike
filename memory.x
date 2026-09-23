MEMORY {
    /*
     * On the Pi Pico 2 W, we have 4MiB of flash on the PCB.
     *
     * Following the common memory.x reused across projects,
     * we allow the linker to freely place text and rodata type content
     * into the first 2MiB of that space.
     */
    FLASH : ORIGIN = 0x10000000, LENGTH = 2048K

    /*
     * Carve out the next 1MiB flash as our fixed-location firmware stash space,
     * the main firmware blob uses 226KiB of that, currently.
     */
    EXTRA_FLASH : ORIGIN = 0x10200000, LENGTH = 1M

    /*
     * Final 1MiB of flash left intentionally unused,
     * for potential runtime use for BLE pairing keys or other persistent state.
     */

    /*
     * RAM consists of 8 banks, SRAM0-SRAM7, with a striped mapping.
     * This is usually good for performance, as it distributes load on
     * those banks evenly.
     */
    RAM : ORIGIN = 0x20000000, LENGTH = 512K
    /*
     * RAM banks 8 and 9 use a direct mapping. They can be used to have
     * memory areas dedicated for some specific job, improving predictability
     * of access times.
     * Example: Separate stacks for core0 and core1.
     */
    SRAM8 : ORIGIN = 0x20080000, LENGTH = 4K
    SRAM9 : ORIGIN = 0x20081000, LENGTH = 4K
}

SECTIONS {
    /* ### Boot ROM info
     *
     * Goes after .vector_table, to keep it in the first 4K of flash
     * where the Boot ROM (and picotool) can find it
     */
    .start_block : ALIGN(4)
    {
        __start_block_addr = .;
        KEEP(*(.start_block));
        KEEP(*(.boot_info));
    } > FLASH

} INSERT AFTER .vector_table;

/* move .text to start /after/ the boot info */
_stext = ADDR(.start_block) + SIZEOF(.start_block);

SECTIONS {
    /* ### Picotool 'Binary Info' Entries
     *
     * Picotool looks through this block (as we have pointers to it in our
     * header) to find interesting information.
     */
    .bi_entries : ALIGN(4)
    {
        /* We put this in the header */
        __bi_entries_start = .;
        /* Here are the entries */
        KEEP(*(.bi_entries));
        /* Keep this block a nice round size */
        . = ALIGN(4);
        /* We put this in the header */
        __bi_entries_end = .;
    } > FLASH
} INSERT AFTER .text;

SECTIONS {
    /* ### Boot ROM extra info
     *
     * Goes after everything in our program, so it can contain a signature.
     */
    .end_block : ALIGN(4)
    {
        __end_block_addr = .;
        KEEP(*(.end_block));
    } > FLASH

} INSERT AFTER .uninit;

PROVIDE(start_to_end = __end_block_addr - __start_block_addr);
PROVIDE(end_to_start = __start_block_addr - __end_block_addr);

/*
 * Place things marked as firmware blobs in our firmware stash space
 */
SECTIONS {
    .firmware : ALIGN(4)
    {
        KEEP(*(.firmware));
    } > EXTRA_FLASH
} INSERT AFTER .end_block;
