#!/usr/bin/env python3
# Aster — AFP 刷写端到端回归测试
#
#   make run-sock &                                  # 另开一个终端
#   python3 tools/test-afp.py --image build/aster-kernel.ast
#
# 流程：3 秒窗口内按 B 进底层模式 → flash → 握手 → 长度 → MAC → 数据
#       → 校验结果 → staged → boot（应从暂存区启动）
import argparse
import hashlib
import hmac
import os
import socket
import struct
import sys
import time

ROOT = os.path.join(os.path.dirname(os.path.abspath(__file__)), "..")
KEY_PATH = os.path.join(ROOT, "boot", "keys", "boot.key")


class Link:
    def __init__(self, path, timeout=30):
        self.s = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        for _ in range(30):
            try:
                self.s.connect(path)
                break
            except (FileNotFoundError, ConnectionRefusedError):
                time.sleep(0.2)
        else:
            sys.exit("连不上串口 socket: %s（先 make run-sock）" % path)
        self.s.settimeout(timeout)
        self.buf = ""

    def pump(self, t=0.4):
        self.s.settimeout(t)
        try:
            while True:
                d = self.s.recv(4096)
                if not d:
                    break
                self.buf += d.decode("utf-8", "replace")
        except socket.timeout:
            pass

    def send(self, data):
        self.s.sendall(data if isinstance(data, bytes) else data.encode())

    def expect(self, token, timeout=20):
        end = time.time() + timeout
        while time.time() < end:
            if token in self.buf:
                i = self.buf.index(token) + len(token)
                self.buf = self.buf[i:]
                return True
            self.pump(0.3)
        return False

    def drain(self):
        self.pump(0.6)
        return self.buf


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--sock", default="/tmp/aster.sock")
    ap.add_argument("--image", default="build/aster-kernel.ast")
    args = ap.parse_args()

    image = open(os.path.join(ROOT, args.image), "rb").read()
    key = bytes.fromhex(open(KEY_PATH).read().strip())
    mac = hmac.new(key, image, hashlib.sha256).digest()

    link = Link(args.sock)
    time.sleep(0.8)
    link.send("b")                      # 3 秒窗口内请求底层模式
    if not link.expect("boot>"):
        sys.exit("没能进入底层模式")
    print("[TEST] 已进入底层模式")

    link.send("flash\n")
    if not link.expect("[AFP] 等待主机握手"):
        sys.exit("引导器没有进入刷写等待。实际输出：%r" % link.buf[-400:])
    print("[TEST] 引导器等待刷写握手")

    link.send(b"W")
    link.expect("OK")
    link.send(struct.pack(">I", len(image)))
    link.expect("SIZE")
    link.send(mac)
    link.expect("DATA")
    link.send(image)

    if link.expect("FLASH OK", 30):
        print("[TEST] 刷写通过 AVP：FLASH OK")
    else:
        print("[TEST] 刷写被拒：", link.drain()[-200:])
        sys.exit(1)

    link.send("staged\n")
    link.pump(1)
    link.send("boot\n")                 # 从暂存区启动
    time.sleep(2.5)
    out = link.drain()
    if "[KERNEL] EX3" in out and "aster>" in out:
        print("[TEST] 已从暂存镜像启动内核，EX3 校验出现在内核侧")
        print("[TEST] PASS")
    else:
        print("[TEST] 输出片段：", out[-500:])
        sys.exit(1)


if __name__ == "__main__":
    main()
