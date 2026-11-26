#!/usr/bin/env python3
# Aster — AFP 串口刷写主机端
#
#   python3 tools/afp-flash.py <镜像> --sock /tmp/aster.sock
#   python3 tools/afp-flash.py <镜像> --pty /dev/pts/3
#
# 与引导器里的 flash.c 协议一一对应：
#   'W' → OK → len(4B 大端) → SIZE → mac(32B) → DATA → 数据
import argparse
import socket
import struct
import sys
import os
import time
import hashlib
import hmac

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))

ROOT = os.path.join(os.path.dirname(os.path.abspath(__file__)), "..")
KEY_PATH = os.path.join(ROOT, "boot", "keys", "boot.key")


def load_key():
    with open(KEY_PATH) as f:
        return bytes.fromhex(f.read().strip())


class Link:
    def __init__(self, sock=None, pty=None):
        self.buf = b""
        if sock:
            s = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
            for _ in range(20):
                try:
                    s.connect(sock)
                    break
                except (FileNotFoundError, ConnectionRefusedError):
                    time.sleep(0.2)
            else:
                sys.exit("无法连接 QEMU 串口 socket: %s" % sock)
            self.s = s
        else:
            import serial
            self.s = serial.Serial(pty, 115200, timeout=1)

    def recv(self):
        while True:
            if self.s:
                try:
                    chunk = self.s.recv(4096)
                except OSError:
                    chunk = b""
                if not chunk:
                    return None
                self.buf += chunk
                return chunk
            else:
                data = self.s.read(4096)
                return data

    def expect(self, token, timeout=20):
        end = time.time() + timeout
        while time.time() < end:
            if token in self.buf:
                idx = self.buf.index(token) + len(token)
                self.buf = self.buf[idx:]
                return True
            if not self.recv():
                time.sleep(0.02)
        return False

    def send(self, data):
        if self.s:
            self.s.sendall(data)
        else:
            self.s.write(data)


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("image")
    g = ap.add_mutually_exclusive_group(required=True)
    g.add_argument("--sock")
    g.add_argument("--pty")
    ap.add_argument("--timeout", type=float, default=60)
    args = ap.parse_args()

    with open(args.image, "rb") as f:
        image = f.read()
    if len(image) > 8 * 1024 * 1024:
        sys.exit("镜像超过 8 MiB，引导器会拒绝")

    key = load_key()
    mac = hmac.new(key, image, hashlib.sha256).digest()
    print("[AFP] 镜像 %d bytes, MAC %s" % (len(image), mac.hex()[:32] + "..."))

    link = Link(sock=args.sock, pty=args.pty)
    print("[AFP] 等待引导器就绪 ...")
    link.send(b"W")
    if not link.expect(b"OK", args.timeout):
        sys.exit("握手失败：引导器没回 OK（确认它停在 flash 命令下）")
    print("[AFP] 握手完成，发送长度")
    link.send(struct.pack(">I", len(image)))
    if not link.expect(b"SIZE", args.timeout):
        sys.exit("长度确认失败")
    print("[AFP] 发送 MAC 与数据 %d bytes" % len(image))
    link.send(mac)
    if not link.expect(b"DATA", args.timeout):
        sys.exit("数据确认失败")
    link.send(image)
    print("[AFP] 数据已发送，等待校验结果 ...")
    if link.expect(b"FLASH OK", args.timeout):
        print("[AFP] 成功：镜像已通过 AVP 校验并暂存")
    elif link.expect(b"FLASH REJECT", args.timeout):
        print("[AFP] 失败：引导器拒绝了镜像")
        sys.exit(1)
    else:
        sys.exit("超时，未收到结果")


if __name__ == "__main__":
    main()
