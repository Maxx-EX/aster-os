/* Aster — 引导器交给内核的启动信息 */
#ifndef ASTER_BOOTINFO_H
#define ASTER_BOOTINFO_H

#include <stdint.h>

#define ASTER_BOOT_MAGIC 0x41535445524B4552ULL   /* "ASTERKER" */

struct aster_bootinfo {
    uint64_t magic;
    uint64_t dtb;               /* 设备树物理地址 */
    uint64_t payload_base;      /* 内核在内存中的地址 */
    uint64_t payload_size;
    uint8_t  image_hash[32];    /* 引导器算出的内核摘要，内核会独立复算并比对 */
    uint8_t  image_mac[32];     /* AVP L2 结果 */
    uint8_t  ex3[32];           /* EX3 原创校验值，内核内嵌的那份必须与之一致 */
    uint32_t avp_elapsed_us;
    uint32_t trusted;
};

#endif
