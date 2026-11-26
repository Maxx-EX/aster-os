/* Aster Boot — 内嵌内核镜像的定位与加载 */
#include "image.h"
#include "board.h"

extern uint8_t __payload_start[];
extern uint8_t __payload_end[];

const void *image_payload(void) { return (const void *)__payload_start; }
size_t      image_size(void)    { return (size_t)(__payload_end - __payload_start); }

/* 裸内存拷贝：此时还没有 libc，也不能用 memcpy（可能被编译器内联成 SIMD） */
int image_load_to(uint64_t dst, const void *src, size_t len)
{
    if (len > ASTER_KERNEL_MAX) return -1;
    volatile uint64_t *d = (volatile uint64_t *)dst;
    const uint8_t *s = (const uint8_t *)src;
    for (size_t i = 0; i < len; i++) ((volatile uint8_t *)d)[i] = s[i];
    (void)d;

    /* 让内核能读到刚写入的代码：清数据缓存 + 无效指令缓存 */
    __asm__ volatile("dsb sy" ::: "memory");
    __asm__ volatile("dc cvau, %0" :: "r"(dst));
    __asm__ volatile("dsb sy" ::: "memory");
    __asm__ volatile("ic ialluis" ::: "memory");
    __asm__ volatile("dsb sy; isb" ::: "memory");
    return 0;
}

void image_jump(uint64_t entry, void *arg)
{
    register uint64_t x0 __asm__("x0") = (uint64_t)arg;
    register uint64_t x1 __asm__("x1") = entry;
    __asm__ volatile(
        "dsb sy\n"
        "isb\n"
        "br  x1\n"
        : : "r"(x0), "r"(x1) : "memory");
}
