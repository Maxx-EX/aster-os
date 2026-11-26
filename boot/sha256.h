/* Aster Boot — 自研 SHA-256 / HMAC-SHA256
 * 不依赖任何外部密码库，引导器必须能独立完成校验。
 */
#ifndef ASTER_SHA256_H
#define ASTER_SHA256_H

#include <stdint.h>
#include <stddef.h>

typedef struct {
    uint32_t h[8];
    uint64_t total_len;
    uint8_t  buf[64];
    uint32_t buf_len;
} sha256_ctx;

typedef struct {
    sha256_ctx inner;
    sha256_ctx outer;
} hmac_sha256_ctx;

void sha256_init(sha256_ctx *c);
void sha256_update(sha256_ctx *c, const void *data, size_t len);
void sha256_final(sha256_ctx *c, uint8_t out[32]);
void sha256(const void *data, size_t len, uint8_t out[32]);

void hmac_sha256_init(hmac_sha256_ctx *c, const uint8_t *key, size_t key_len);
void hmac_sha256_update(hmac_sha256_ctx *c, const void *data, size_t len);
void hmac_sha256_final(hmac_sha256_ctx *c, uint8_t out[32]);
void hmac_sha256(const uint8_t *key, size_t key_len,
                 const void *data, size_t len, uint8_t out[32]);

/* 常量时间比较，避免时序侧信道 */
int  aster_memeq_ct(const uint8_t *a, const uint8_t *b, size_t n);

#endif
