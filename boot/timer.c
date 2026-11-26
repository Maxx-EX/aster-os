#include "timer.h"

/* 只做 64 位算术，避免在裸机上引入 libgcc 的 128 位除法辅助函数 */
uint64_t timer_us(void)
{
    uint64_t f = timer_freq();
    if (!f) return 0;
    uint64_t div = f / 1000000ull;
    if (!div) div = 1;
    return timer_ticks() / div;
}

void timer_spin_ms(uint32_t ms)
{
    uint64_t start = timer_us();
    while ((timer_us() - start) < (uint64_t)ms * 1000ull) { }
}
