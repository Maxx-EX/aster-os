/* Aster Boot — 通用定时器（CNTPCT_EL0）计时，不依赖中断 */
#ifndef ASTER_TIMER_H
#define ASTER_TIMER_H

#include <stdint.h>

static inline uint64_t timer_ticks(void)
{
    uint64_t v;
    __asm__ volatile("mrs %0, cntpct_el0" : "=r"(v));
    return v;
}

static inline uint64_t timer_freq(void)
{
    uint64_t v;
    __asm__ volatile("mrs %0, cntfrq_el0" : "=r"(v));
    return v;
}

uint64_t timer_us(void);
void     timer_spin_ms(uint32_t ms);

#endif
