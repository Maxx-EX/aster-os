#!/usr/bin/env python3
# Aster — 镜像签名、EX3 生成与清单生成（AVP 的"签署侧"）
#
#   python3 tools/aster-sign.py --build-id X keygen
#   python3 tools/aster-sign.py --build-id X gen-ex3      # 生成 EX3 → boot/ex3.h + kernel/src/ex3.rs
#   python3 tools/aster-sign.py --build-id X sign <image> # 生成 boot/manifest.h
#   python3 tools/aster-sign.py --build-id X mac  <image> # 只打印 MAC（给 AFP 刷写用）
#
# EX3（Aster Exclusive Identifier v3）是本系统的原创校验值：
#   EX3 = HMAC-SHA256(K, BOOT_ID ‖ "EX3" ‖ build_id ‖ nonce)
# 它由只有本机持有的 K 派生，内核与引导器各嵌一份，启动时必须一致才允许内核接管。
import argparse
import hashlib
import hmac
import os
import secrets
import sys

BOOT_ID = b"ASTER-BOOT-ARM64-V1"

ROOT = os.path.join(os.path.dirname(os.path.abspath(__file__)), "..")
KEY_PATH = os.path.join(ROOT, "boot", "keys", "boot.key")
NONCE_PATH = os.path.join(ROOT, "boot", "keys", "ex3.nonce")
EX3_H_PATH = os.path.join(ROOT, "boot", "ex3.h")
EX3_RS_PATH = os.path.join(ROOT, "kernel", "src", "ex3.rs")
MANIFEST_PATH = os.path.join(ROOT, "boot", "manifest.h")


def load_key():
    with open(KEY_PATH, "r") as f:
        raw = f.read().strip()
    key = bytes.fromhex(raw)
    if len(key) != 32:
        sys.exit("密钥长度必须是 32 字节")
    return key


def load_or_create_nonce():
    """EX3 的构建随机数：首次生成后固定，保证同一工作区的 EX3 稳定可复现。"""
    os.makedirs(os.path.dirname(NONCE_PATH), exist_ok=True)
    if os.path.exists(NONCE_PATH):
        raw = open(NONCE_PATH).read().strip()
        if raw:
            n = bytes.fromhex(raw)
            if len(n) == 32:
                return n
    n = secrets.token_bytes(32)
    with open(NONCE_PATH, "w") as f:
        f.write(n.hex() + "\n")
    return n


def ex3_bytes(key, build_id):
    """32 字节原创校验值：绑定引导器版本、构建目标、本次构建随机数。"""
    nonce = load_or_create_nonce()
    msg = BOOT_ID + b"|EX3|" + build_id.encode() + b"|" + nonce
    return hmac.new(key, msg, hashlib.sha256).digest()


def ex3_tag(v):
    return "EX3-%s-%s" % (v[:8].hex().upper(), v[8:16].hex().upper())


def compute(image, build_id):
    key = load_key()
    h = hashlib.sha256(image).digest()
    mac = hmac.new(key, image, hashlib.sha256).digest()
    ex3 = ex3_bytes(key, build_id)
    # 链绑定同时纳入 EX3：换掉内核（EX3 不同）就过不了 L3
    chain = hmac.new(key, BOOT_ID + b"|CHAIN|" + ex3 + mac, hashlib.sha256).digest()
    return key, h, mac, ex3, chain


def c_array(name, data):
    body = ", ".join("0x%02x" % b for b in data)
    return "static const uint8_t %s[%d] = { %s };\n" % (name, len(data), body)


def write_if_changed(path, text, label):
    old = None
    if os.path.exists(path):
        with open(path) as f:
            old = f.read()
    if old == text:
        print("[ASTER-SIGN] %s 无变化，跳过写入" % label)
        return False
    with open(path, "w") as f:
        f.write(text)
    print("[ASTER-SIGN] %s 已更新" % label)
    return True


def cmd_keygen(args):
    key = secrets.token_bytes(32)
    os.makedirs(os.path.dirname(KEY_PATH), exist_ok=True)
    with open(KEY_PATH, "w") as f:
        f.write(key.hex() + "\n")
    print("已生成密钥: %s" % KEY_PATH)
    print("注意：这是开发密钥，正式发布前请重新生成并妥善保管。")


def cmd_gen_ex3(args):
    key = load_key()
    v = ex3_bytes(key, args.build_id)
    tag = ex3_tag(v)

    c = []
    c.append("/* 本文件由 tools/aster-sign.py 自动生成，请勿手工编辑。 */\n")
    c.append("#ifndef ASTER_EX3_H\n#define ASTER_EX3_H\n\n#include <stdint.h>\n\n")
    c.append('#define ASTER_EX3_TAG "%s"\n' % tag)
    c.append(c_array("ASTER_EX3", v))
    c.append("\n#endif /* ASTER_EX3_H */\n")

    rs = []
    rs.append("// 本文件由 tools/aster-sign.py 自动生成，请勿手工编辑。\n")
    rs.append("/// EX3 —— Aster 原创校验值，只在本次构建内有效\n")
    rs.append('pub const EX3_TAG: &str = "%s";\n' % tag)
    rs.append("#[rustfmt::skip]\n")
    rs.append("pub const EX3: [u8; 32] = [\n    ")
    rs.append(", ".join("0x%02x" % b for b in v))
    rs.append(",\n];\n")

    write_if_changed(EX3_H_PATH, "".join(c), "boot/ex3.h")
    write_if_changed(EX3_RS_PATH, "".join(rs), "kernel/src/ex3.rs")
    print("[ASTER-SIGN] EX3 指纹 : %s" % tag)


def cmd_sign(args):
    with open(args.image, "rb") as f:
        image = f.read()
    key, h, mac, ex3, chain = compute(image, args.build_id)
    tag = ex3_tag(ex3)

    manifest = []
    manifest.append("/* 本文件由 tools/aster-sign.py 自动生成，请勿手工编辑。 */\n")
    manifest.append("#ifndef ASTER_MANIFEST_H\n#define ASTER_MANIFEST_H\n\n")
    manifest.append('#include <stdint.h>\n#include "ex3.h"\n\n')
    manifest.append('#define ASTER_BOOT_ID "%s"\n' % BOOT_ID.decode())
    manifest.append('#define ASTER_PAYLOAD_BUILDID "%s"\n' % args.build_id)
    manifest.append("#define ASTER_PAYLOAD_BYTES %d\n\n" % len(image))
    manifest.append(c_array("ASTER_EXPECTED_HASH", h))
    manifest.append(c_array("ASTER_EXPECTED_MAC", mac))
    manifest.append(c_array("ASTER_EXPECTED_CHAIN", chain))
    manifest.append("#define ASTER_BOOT_KEY_LEN 32\n")
    manifest.append(c_array("ASTER_BOOT_KEY", key))
    manifest.append("\n#endif /* ASTER_MANIFEST_H */\n")

    write_if_changed(MANIFEST_PATH, "".join(manifest), "boot/manifest.h")
    print("[ASTER-SIGN] 镜像大小 : %d bytes" % len(image))
    print("[ASTER-SIGN] SHA-256  : %s" % h.hex())
    print("[ASTER-SIGN] HMAC-MAC : %s" % mac.hex())
    print("[ASTER-SIGN] EX3      : %s" % tag)


def cmd_mac(args):
    with open(args.image, "rb") as f:
        image = f.read()
    _, h, mac, _, _ = compute(image, args.build_id)
    print("%s %s" % (mac.hex(), h.hex()))


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--build-id", default="aster-dev", help="构建标识；EX3 与链绑定都依赖它")
    sub = ap.add_subparsers(dest="cmd", required=True)

    k = sub.add_parser("keygen")
    k.set_defaults(func=cmd_keygen)

    g = sub.add_parser("gen-ex3")
    g.add_argument("--build-id", default=argparse.SUPPRESS)
    g.set_defaults(func=cmd_gen_ex3)

    s = sub.add_parser("sign")
    s.add_argument("image")
    s.add_argument("--build-id", default=argparse.SUPPRESS)
    s.set_defaults(func=cmd_sign)

    m = sub.add_parser("mac")
    m.add_argument("image")
    m.add_argument("--build-id", default=argparse.SUPPRESS)
    m.set_defaults(func=cmd_mac)

    args = ap.parse_args()
    args.func(args)


if __name__ == "__main__":
    main()
