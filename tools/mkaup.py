#!/usr/bin/env python3
"""生成一个 AUP（Aster Update Package）更新包的样例。

包结构（28 字节头 + body）：
  0x00 "AUP1"  / 0x04 version u16 / 0x06 build u16
  0x08 body_size u32 / 0x0C crc32(body) u32
  0x10 ex3[12]  —— 本机构建 EX3 的前 12 字节，确认"打给这台设备的"
用法： python3 tools/mkaup.py <out.aup> <version-hex> [note]
"""
import struct
import sys
import zlib

EX3 = [0x59, 0x94, 0x49, 0x1f, 0xac, 0xe5, 0x06, 0x80,
       0xcf, 0x53, 0xdc, 0x00, 0x93, 0x37, 0xbd, 0xcd]


def main():
    if len(sys.argv) < 3:
        print(__doc__.strip())
        return 2
    out = sys.argv[1]
    ver = int(sys.argv[2], 0)
    note = sys.argv[3] if len(sys.argv) > 3 else "Aster 0.9 kernel, staged via AsterFS"
    body = (note + "\n").encode()
    head = struct.pack("<4sHHII", b"AUP1", ver, 0, len(body), zlib.crc32(body) & 0xFFFFFFFF)
    head += bytes(EX3[:12])
    assert len(head) == 28
    open(out, "wb").write(head + body)
    print(f"[AUP] {out}  {len(head)+len(body)} 字节  ver={ver:#06x} body={len(body)} crc={zlib.crc32(body)&0xFFFFFFFF:08x}")


if __name__ == "__main__":
    sys.exit(main())
