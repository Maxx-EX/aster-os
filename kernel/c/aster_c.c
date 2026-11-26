/* Aster Kernel — C 侧支持代码
 * 这里放"内存安全收益不高、但需要精确控制"的东西：
 *   - 密码学常量的搬运与哈希（与引导器同一份 SHA-256 实现）
 *   - 供 Rust 调用的 C 入口
 */
#include <stdint.h>
#include <stddef.h>

void sha256_init(void *ctx);
void sha256_update(void *ctx, const void *data, size_t len);
void sha256_final(void *ctx, uint8_t out[32]);
void sha256(const void *data, size_t len, uint8_t out[32]);
int  aster_memeq_ct(const uint8_t *a, const uint8_t *b, size_t n);

/* 内核侧复算镜像摘要：对 [base, base+len) 做 SHA-256 写进 out */
void aster_c_hash_image(uint64_t base, uint64_t len, uint8_t out[32])
{
    sha256((const void *)base, (size_t)len, out);
}

/* 常量时间比较的 C 版本，给 Rust 的 chain 校验用 */
int aster_c_ct_eq(const uint8_t *a, const uint8_t *b, uint64_t n)
{
    return aster_memeq_ct(a, b, (size_t)n);
}

/* 编译期字符串，用来证明双语言链接确实通了 */
const char *aster_c_tag(void)
{
    return "C-side OK";
}
