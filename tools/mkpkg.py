#!/usr/bin/env python3
"""生成 Aster 包（.ase）。

这不是"把 Python 编译进来"——Aster 还没 ELF 加载器，也没有动态链接。
这里做的是**包格式与安装链路的骨架**：一个 128 字节的清单 + 一个占位程序体，
用来验证 pkg 索引、下载落地、AsterFS 写入、ls 可见、卸载这条完整通路。
真正的包体要等 PKG-2（HTTP 客户端 + 签名校验 + 依赖解析）。
"""
import struct, zlib, sys, os

PKGS = [
    ("python3", "3.12.4", "Python 解释器（占位：真包需 PKG-2 下载）"),
    ("gcc",     "13.2.0", "C/C++ 编译器（占位）"),
    ("git",     "2.44.0", "版本控制（占位）"),
    ("gh",      "2.50.0", "GitHub CLI（占位）"),
]

MAGIC = b"ASEPKG1\x00"

def build(name, ver, desc):
    body = bytes(((ord(name[i % len(name)]) ^ (i * 7)) & 0xFF) for i in range(48))
    head = bytearray()
    head += MAGIC
    head += name.encode().ljust(16, b"\x00")
    head += ver.encode().ljust(12, b"\x00")
    head += struct.pack("<I", len(body))
    head += struct.pack("<I", zlib.crc32(body) & 0xFFFFFFFF)
    head += desc.encode()[:52].ljust(52, b"\x00")
    return bytes(head) + body

out = os.path.join("assets", "pkg")
os.makedirs(out, exist_ok=True)
for name, ver, desc in PKGS:
    blob = build(name, ver, desc)
    path = os.path.join(out, f"{name}.ase")
    open(path, "wb").write(blob)
    print(f"{path}  {len(blob)} bytes")
