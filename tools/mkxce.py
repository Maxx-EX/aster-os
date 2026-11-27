#!/usr/bin/env python3
"""把一段 EL0 扁平二进制打包成 XCE（Aster eXecutable）。

XCE 头 28 字节，全部小端：

    0x00  4  magic      "XCE1"
    0x04  2  version    1
    0x06  2  kind       0 = EL0 可执行（将来留 1 = 数据包，2 = 内核模块）
    0x08  4  entry_va   程序入口虚拟地址（必须落在用户代码页 0x44400000）
    0x0C  4  body_size  body 字节数
    0x10  4  crc32      body 的 IEEE CRC32
    0x14  4  stack_va   期望的栈顶
    0x18  4  flags      0
    0x1C..   body

用法： python3 tools/mkxce.py <in.bin> <out.xce> [entry_va] [stack_va]
"""
import struct
import sys
import zlib

CODE = 0x44400000
STACK = 0x44403000
HDR = 28


def pack(body: bytes, entry: int = CODE, stack: int = STACK, kind: int = 0) -> bytes:
    head = struct.pack(
        "<4sHHIIIII",
        b"XCE1",
        1,
        kind,
        entry,
        len(body),
        zlib.crc32(body) & 0xFFFFFFFF,
        stack,
        0,
    )
    assert len(head) == HDR
    return head + body


def unpack(data: bytes) -> dict:
    if len(data) < HDR:
        raise ValueError("太短，不是 XCE")
    magic, ver, kind, entry, size, crc, stack, flags = struct.unpack("<4sHHIIIII", data[:HDR])
    if magic != b"XCE1":
        raise ValueError(f"魔数不对：{magic!r}")
    if HDR + size > len(data):
        raise ValueError(f"body 长度 {size} 超出文件 {len(data)}")
    body = data[HDR:HDR + size]
    ok = (zlib.crc32(body) & 0xFFFFFFFF) == crc
    return {
        "version": ver,
        "kind": kind,
        "entry": entry,
        "size": size,
        "crc": crc,
        "crc_ok": ok,
        "stack": stack,
        "flags": flags,
        "body": body,
    }


def main() -> int:
    if len(sys.argv) < 3:
        print(__doc__.strip())
        return 2
    src, dst = sys.argv[1], sys.argv[2]
    entry = int(sys.argv[3], 0) if len(sys.argv) > 3 else CODE
    stack = int(sys.argv[4], 0) if len(sys.argv) > 4 else STACK
    body = open(src, "rb").read()
    if not body:
        print("输入是空文件")
        return 1
    if len(body) > 0x1000:
        print(f"body {len(body)} 字节，超过一页（4 KiB）")
        return 1
    out = pack(body, entry, stack)
    open(dst, "wb").write(out)
    info = unpack(out)
    print(
        f"[XCE] {dst}  {len(out)} 字节  entry={info['entry']:#x}  body={info['size']}  "
        f"crc={info['crc']:08x} {'OK' if info['crc_ok'] else 'BAD'}"
    )
    return 0


if __name__ == "__main__":
    sys.exit(main())
