/* Aster Boot — PL011 串口驱动 */
#include "uart.h"
#include "board.h"

#define PL011_DR    (*(volatile uint32_t *)(ASTER_UART_BASE + 0x00))
#define PL011_FR    (*(volatile uint32_t *)(ASTER_UART_BASE + 0x18))
#define PL011_IBRD  (*(volatile uint32_t *)(ASTER_UART_BASE + 0x24))
#define PL011_FBRD  (*(volatile uint32_t *)(ASTER_UART_BASE + 0x28))
#define PL011_LCRH  (*(volatile uint32_t *)(ASTER_UART_BASE + 0x2C))
#define PL011_CR    (*(volatile uint32_t *)(ASTER_UART_BASE + 0x30))
#define PL011_ICR   (*(volatile uint32_t *)(ASTER_UART_BASE + 0x44))

#define FR_TXFF (1u << 5)
#define FR_RXFE (1u << 4)

void uart_init(void)
{
    PL011_CR    = 0;                 /* 先关掉 */
    PL011_ICR   = 0x7FF;             /* 清所有中断 */
    /* 115200 8N1，参考时钟 24 MHz（QEMU virt 默认） */
    PL011_IBRD  = 13;
    PL011_FBRD  = 2;
    PL011_LCRH  = (3u << 5) | (1u << 4);   /* WLEN=8bit, FIFO 使能 */
    PL011_CR    = (1u << 0) | (1u << 8) | (1u << 9); /* UARTEN | TXE | RXE */
}

void uart_putc(char c)
{
    while (PL011_FR & FR_TXFF) { }
    PL011_DR = (uint8_t)c;
    if (c == '\n') {
        while (PL011_FR & FR_TXFF) { }
        PL011_DR = '\r';
    }
}

void uart_flush(void)
{
    while (PL011_FR & FR_TXFF) { }
}

void uart_puts(const char *s)
{
    while (*s) uart_putc(*s++);
}

int uart_rx_ready(void)
{
    return (PL011_FR & FR_RXFE) ? 0 : 1;
}

int uart_getc(void)
{
    if (!uart_rx_ready()) return -1;
    return (int)(PL011_DR & 0xFF);
}

int uart_getc_blocking(void)
{
    while (!uart_rx_ready()) { }
    return (int)(PL011_DR & 0xFF);
}

/* ---------- 极简 printf ---------- */
static void put_u64(uint64_t v, int base, int width)
{
    char buf[24];
    int i = 0;
    if (v == 0) { buf[i++] = '0'; }
    while (v && i < (int)sizeof(buf)) {
        int d = (int)(v % (uint64_t)base);
        buf[i++] = (char)(d < 10 ? '0' + d : 'a' + d - 10);
        v /= (uint64_t)base;
    }
    while (width-- > i) uart_putc('0');
    while (i > 0) uart_putc(buf[--i]);
}

void bprintf(const char *fmt, ...)
{
    va_list ap;
    va_start(ap, fmt);
    for (const char *p = fmt; *p; p++) {
        if (*p != '%') { uart_putc(*p); continue; }
        p++;
        int width = 0;
        if (*p == '0') { p++; while (*p >= '0' && *p <= '9') { width = width * 10 + (*p - '0'); p++; } }
        switch (*p) {
        case 'c': uart_putc((char)va_arg(ap, int)); break;
        case 's': { const char *s = va_arg(ap, const char *); uart_puts(s ? s : "(null)"); break; }
        case 'd': { int32_t v = va_arg(ap, int32_t); if (v < 0) { uart_putc('-'); put_u64((uint64_t)(-v), 10, 0); } else put_u64((uint64_t)v, 10, 0); break; }
        case 'u': put_u64((uint64_t)va_arg(ap, uint32_t), 10, 0); break;
        case 'x': put_u64((uint64_t)va_arg(ap, uint32_t), 16, width); break;
        case 'p':
        case 'l': uart_puts("0x"); put_u64((uint64_t)va_arg(ap, void *), 16, width); break;
        case '%': uart_putc('%'); break;
        default:  uart_putc('%'); uart_putc(*p); break;
        }
    }
    va_end(ap);
}

void uart_print_hex(const char *label, uint64_t v)
{
    uart_puts(label);
    uart_puts("0x");
    put_u64(v, 16, 0);
    uart_putc('\n');
}
