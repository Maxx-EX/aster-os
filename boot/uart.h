/* Aster Boot — PL011 串口驱动（QEMU virt: 0x09000000，树莓派走 BOARD 宏） */
#ifndef ASTER_UART_H
#define ASTER_UART_H

#include <stdint.h>
#include <stdarg.h>

void uart_init(void);
void uart_putc(char c);
void uart_puts(const char *s);
void uart_print_hex(const char *label, uint64_t v);
int  uart_rx_ready(void);          /* 1 = 有可读字符 */
int  uart_getc(void);             /* 无数据时返回 -1 */
int  uart_getc_blocking(void);    /* 一直等到有字符 */
void uart_flush(void);

/* 极简 printf：支持 %c %s %d %u %x %p %llx %% */
void bprintf(const char *fmt, ...);

#endif
