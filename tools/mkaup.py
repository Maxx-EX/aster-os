#!/usr/bin/env python3
"""生成一个 AUP（Aster Update Package）更新包的样例。

包结构（28 字节头 + body）：
  0x00 "AUP1"  / 0x04 version u16 / 0x06 build u16
  0x08 body_size u32 / 0x0C crc32(body) u32
  0x10 ex3[12]  —— 本机构建 EX3 的前 12 字节，确认"打给这台设备的"
用法： python3 tools/mkaup.py <out.aup> <version-hex> [note] [--from-ex3]
      --from-ex3 从 boot/ex3.h 读当前构建的 EX3（CI 里密钥每次新生成，
      不读文件的话 AUP 会绑到写死的旧指纹，update check 必然失败）
"""
import os
import re
import struct
import sys
import zlib

EX3 = [0x59, 0x94, 0x49, 0x1f, 0xac, 0xe5, 0x06, 0x80,
       0xcf, 0x53, 0xdc, 0x00, 0x93, 0x37, 0xbd, 0xcd]

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))


def ex3_from_header():
    """从 boot/ex3.h 的 ASTER_EX3[32] 数组里读出当前 EX3。"""
    path = os.path.join(ROOT, "boot", "ex3.h")
    src = open(path, encoding="utf-8").read()
    m = re.search(r"ASTER_EX3\[32\]\s*=\s*\{([^}]*)\}", src)
    if not m:
        raise SystemExit("boot/ex3.h 里找不到 ASTER_EX3[32]")
    vals = [int(x.strip(), 16) for x in m.group(1).split(",") if x.strip()]
    if len(vals) != 32:
        raise SystemExit(f"EX3 应该是 32 字节，实际 {len(vals)}")
    return vals


def main():
    if len(sys.argv) < 3:
        print(__doc__.strip())
        return 2
    out = sys.argv[1]
    ver = int(sys.argv[2], 0)
    rest = [a for a in sys.argv[3:] if a != "--from-ex3"]
    note = rest[0] if rest else "Aster 0.9 kernel, staged via AsterFS"
    ex3 = ex3_from_header() if "--from-ex3" in sys.argv else EX3
    body = (note + "\n").encode()
    head = struct.pack("<4sHHII", b"AUP1", ver, 0, len(body), zlib.crc32(body) & 0xFFFFFFFF)
    head += bytes(ex3[:12])
    assert len(head) == 28
    open(out, "wb").write(head + body)
    print(f"[AUP] {out}  {len(head)+len(body)} 字节  ver={ver:#06x} body={len(body)} crc={zlib.crc32(body)&0xFFFFFFFF:08x}")


if __name__ == "__main__":
    sys.exit(main())
