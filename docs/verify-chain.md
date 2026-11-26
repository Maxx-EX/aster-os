# AVP — Aster Verify Protocol

原创的三级镜像校验。目标不是"看起来安全"，而是**每一层都能真的拦住一种攻击**。

## 三层

```
payload ──► SHA-256 ──────────────► L1: 与清单 expected_hash 比对
        └─► HMAC(K, payload) ─────► L2: 与清单 expected_mac 比对
              └─► HMAC(K, BOOT_ID ‖ L2) ► L3: 与清单 expected_chain 比对
```

| 层 | 防什么 | 攻击者没有 K 时 |
| --- | --- | --- |
| L1 完整性 | 传输/存储中被改字节、被截断 | 能过 L1 吗？不能，改一个字节就变 |
| L2 真实性 | 攻击者自己造一个镜像 | 造不出正确 MAC |
| L3 链绑定 | 把旧版本内核装到新引导器上（降级/回滚攻击） | 造不出与新 BOOT_ID 绑定的 chain |

三层全过才 `trusted=1`。任何一层失败，`failed_stage` 记下位置，引导器直接进底层模式，`boot` 命令也拒绝放行（除非有已校验的 AFP 暂存镜像）。

## 清单长什么样

`tools/aster-sign.py` 生成 `boot/manifest.h`：

```c
#define ASTER_BOOT_ID "ASTER-BOOT-ARM64-V1"
#define ASTER_PAYLOAD_BUILDID "aster-qemu"
static const uint8_t ASTER_EXPECTED_HASH[32]  = { ... };
static const uint8_t ASTER_EXPECTED_MAC[32]   = { ... };
static const uint8_t ASTER_EXPECTED_CHAIN[32] = { ... };
static const uint8_t ASTER_BOOT_KEY[32]       = { ... };   // K，只给引导器
```

## 密钥

- `boot/keys/boot.key`：32 字节随机，`make` 首次构建自动生成
- **已在 `.gitignore` 里，不会进仓库**
- 泄露 K = 整个校验体系失效，需要重新生成并重新分布引导器

这是 **HMAC（共享密钥）** 方案，威胁模型是"设备持有者和镜像发行者是同一方，要防的是外部篡改"。

## 已知局限

1. **共享密钥**：任何能读到引导器二进制的人都能读出 K（`strings`/反汇编），进而伪造镜像。真正要对第三方保密的发布流程需要非对称签名（Ed25519），引导器里只放公钥。
2. **没有防回滚计数器**：如果攻击者保留旧版的完整合法镜像（连同旧 MAC），L3 能挡住跨版本混搭，但挡不住整体回滚到旧版本。需要 eFuse/OTP 单调计数器。
3. **没有安全启动硬件**：引导器本身没被签名，替换引导器就绕过了全部校验。真机阶段要靠 BootROM 的信任根。

这三点写在文档里，是为了让后来的人知道边界在哪，而不是以为现在就是完整的可信启动。

## 验证记录

| 测试 | 结果 |
| --- | --- |
| 正常镜像 | L1/L2/L3 全 PASS，1 ms 内完成，进入内核 |
| 篡改 payload 偏移 0x131f4 一个字节 | L1 FAIL，`failed_stage=L1`，拒绝启动 |
| 用未签名的内核替换 payload | L1 可能过、L2 必 FAIL |
| AFP 刷写带正确 MAC | `FLASH OK trusted=1`，可从暂存区启动 |
| AFP 刷写 MAC 不符 | `FLASH REJECT stage=2`，不落盘 |

## 实现注意

`sha256.c` 是手写的，最初有个 bug：`update` 在"缓冲区已有残留数据、本次数据不足以填满"时，末尾把 `buf_len` 覆盖成了本次剩余长度，丢掉了之前缓冲的字节。

表现很典型：L1/L2 都过（payload 是一次大块输入，没触发），L3 挂（`BOOT_ID` 19 字节 + L2 32 字节，两次 `update`，正好触发）。用主机侧 `hmac.new(key, b"ASTER-BOOT-ARM64-V1"+mac, sha256)` 对比才发现两侧不一致。

这个 bug 是 L3 独立把关的价值的最好证明 —— 如果只有一层校验，这个错误会一直藏着。
