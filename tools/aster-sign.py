#!/usr/bin/env python3
# Aster — 镜像签名与清单生成（AVP 的"签署侧"）
#
#   python3 tools/aster-sign.py keygen            # 生成 32 字节引导密钥
#   python3 tools/aster-sign.py sign <image>      # 生成 boot/manifest.h 并打印 MAC
#   python3 tools/aster-sign.py mac  <image>      # 只打印 MAC（给 AFP 刷写用）
#
# 说明：这里用的 HMAC-SHA256 是 AVP 的真实性层，密钥只存在于引导器
# 只读区和构建机上，内核永远拿不到，所以内核无法伪造校验结果。
import argparse
import hashlib
import hmac
import os
import secrets
import sys

BOOT_ID = b"ASTER-BOOT-ARM64-V1"
KEY_PATH = os.path.join(os.path.dirname(os.path.abspath(__file__)), "..", "boot", "keys", "boot.key")
MANIFEST_PATH = os.path.join(os.path.dirname(os.path.abspath(__file__)), "..", "boot", "manifest.h")


def load_key(path):
    with open(path, "r") as f:
        raw = f.read().strip()
    key = bytes.fromhex(raw)
    if len(key) != 32:
        sys.exit("密钥长度必须是 32 字节")
    return key


def compute(image):
    key = load_key(KEY_PATH)
    h = hashlib.sha256(image).digest()
    mac = hmac.new(key, image, hashlib.sha256).digest()
    chain = hmac.new(key, BOOT_ID + mac, hashlib.sha256).digest()
    return key, h, mac, chain


def c_array(name, data):
    body = ", ".join("0x%02x" % b for b in data)
    return "static const uint8_t %s[%d] = { %s };\n" % (name, len(data), body)


def cmd_keygen(args):
    key = secrets.token_bytes(32)
    os.makedirs(os.path.dirname(KEY_PATH), exist_ok=True)
    with open(KEY_PATH, "w") as f:
        f.write(key.hex() + "\n")
    print("已生成密钥: %s" % KEY_PATH)
    print("注意：这是开发密钥，正式发布前请重新生成并妥善保管。")


def cmd_sign(args):
    with open(args.image, "rb") as f:
        image = f.read()
    key, h, mac, chain = compute(image)
    build_id = args.build_id or os.path.basename(args.image)
    manifest = []
    manifest.append("/* 本文件由 tools/aster-sign.py 自动生成，请勿手工编辑。 */\n")
    manifest.append("#ifndef ASTER_MANIFEST_H\n#define ASTER_MANIFEST_H\n\n#include <stdint.h>\n\n")
    manifest.append('#define ASTER_BOOT_ID "%s"\n' % BOOT_ID.decode())
    manifest.append('#define ASTER_PAYLOAD_BUILDID "%s"\n' % build_id)
    manifest.append("#define ASTER_PAYLOAD_BYTES %d\n\n" % len(image))
    manifest.append(c_array("ASTER_EXPECTED_HASH", h))
    manifest.append(c_array("ASTER_EXPECTED_MAC", mac))
    manifest.append(c_array("ASTER_EXPECTED_CHAIN", chain))
    manifest.append("#define ASTER_BOOT_KEY_LEN 32\n")
    manifest.append(c_array("ASTER_BOOT_KEY", key))
    manifest.append("\n#endif /* ASTER_MANIFEST_H */\n")
    text = "".join(manifest)
    old = None
    if os.path.exists(MANIFEST_PATH):
        with open(MANIFEST_PATH) as f:
            old = f.read()
    if old != text:
        with open(MANIFEST_PATH, "w") as f:
            f.write(text)
        print("[ASTER-SIGN] 清单已更新 : %s" % MANIFEST_PATH)
    else:
        print("[ASTER-SIGN] 清单无变化，跳过写入")
    print("[ASTER-SIGN] 镜像大小 : %d bytes" % len(image))
    print("[ASTER-SIGN] SHA-256  : %s" % h.hex())
    print("[ASTER-SIGN] HMAC-MAC : %s" % mac.hex())


def cmd_mac(args):
    with open(args.image, "rb") as f:
        image = f.read()
    _, h, mac, _ = compute(image)
    print("%s %s" % (mac.hex(), h.hex()))


def main():
    ap = argparse.ArgumentParser()
    sub = ap.add_subparsers(dest="cmd", required=True)

    k = sub.add_parser("keygen")
    k.set_defaults(func=cmd_keygen)

    s = sub.add_parser("sign")
    s.add_argument("image")
    s.add_argument("--build-id", default=None)
    s.set_defaults(func=cmd_sign)

    m = sub.add_parser("mac")
    m.add_argument("image")
    m.set_defaults(func=cmd_mac)

    args = ap.parse_args()
    args.func(args)


if __name__ == "__main__":
    main()
