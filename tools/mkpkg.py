#!/usr/bin/env python3
"""生成 Aster 包（.ase）与内核侧索引表 kernel/src/pkgidx.rs。

这不是"把 Python 编译进来"——Aster 还没有 ELF 加载器与动态链接。
这里做的是**包格式与安装链路**：144 字节头 + 48 字节占位程序体，
用来验证 pkg 索引 / 分类 / 搜索 / 落盘 / ls 可见这条完整通路。
真正的包体要等 PKG-2（HTTP 客户端 + 签名校验 + 依赖解析）。

头布局（144 字节）：
  0   8  魔数 "ASEPKG1\\0"
  8  16  包名        (ASCII，补零)
 24  12  版本号
 36   4  包体长度    (LE u32)
 40   4  CRC32       (LE u32)
 44   8  分类        (lang/build/vcs/net/edit/sys/db/media/sec/android)
 52  44  一行描述    (UTF-8 截断到 44 字节)
 96  48  包体（占位）

加包只改下面的 PKGS 列表，然后重跑本脚本并重编内核。
"""
import os
import struct
import zlib

# (名字, 版本, 分类, 描述)
PKGS = [
    # ---- lang：语言与运行时 ----
    ("python3", "3.12.4", "lang", "Python 解释器"),
    ("pip", "24.2", "lang", "Python 包安装器"),
    ("numpy", "2.0.1", "lang", "数值计算库"),
    ("pandas", "2.2.2", "lang", "数据帧库"),
    ("flask", "3.0.3", "lang", "轻量 Web 框架"),
    ("gcc", "13.2.0", "lang", "C 编译器"),
    ("g++", "13.2.0", "lang", "C++ 编译器"),
    ("clang", "17.0.6", "lang", "LLVM 编译器"),
    ("llvm", "17.0.6", "lang", "LLVM 后端工具"),
    ("rustc", "1.79.0", "lang", "Rust 编译器"),
    ("cargo", "1.79.0", "lang", "Rust 包管理器"),
    ("go", "1.22.5", "lang", "Go 工具链"),
    ("node", "20.15.1", "lang", "Node.js 运行时"),
    ("npm", "10.7.0", "lang", "Node 包管理器"),
    ("java", "21.0.4", "lang", "OpenJDK 运行时"),
    ("kotlin", "2.0.0", "lang", "Kotlin 编译器"),
    ("php", "8.3.8", "lang", "PHP 运行时"),
    ("perl", "5.40", "lang", "Perl 解释器"),
    ("ruby", "3.3.4", "lang", "Ruby 解释器"),
    ("lua", "5.4.6", "lang", "Lua 解释器"),
    # ---- build：构建工具链 ----
    ("make", "4.3", "build", "GNU make"),
    ("cmake", "3.29.6", "build", "CMake 构建系统"),
    ("ninja", "1.12.1", "build", "Ninja 构建器"),
    ("meson", "1.4.1", "build", "Meson 构建系统"),
    ("pkgconf", "2.1.1", "build", "pkg-config 替代"),
    ("autoconf", "2.72", "build", "Autoconf"),
    ("bison", "3.8.2", "build", "语法分析器生成"),
    ("flex", "2.6.4", "build", "词法分析器生成"),
    ("gradle", "8.7", "build", "Gradle 构建"),
    # ---- vcs：版本控制 ----
    ("git", "2.44.0", "vcs", "版本控制"),
    ("gh", "2.50.0", "vcs", "GitHub CLI"),
    ("svn", "1.14.3", "vcs", "Subversion 客户端"),
    ("hg", "6.7.4", "vcs", "Mercurial 客户端"),
    # ---- net：网络 ----
    ("curl", "8.9.1", "net", "HTTP 客户端"),
    ("wget", "1.24.5", "net", "下载工具"),
    ("openssh", "9.7", "net", "SSH 客户端与服务端"),
    ("rsync", "3.3.0", "net", "增量同步"),
    ("dig", "9.18.26", "net", "DNS 查询"),
    ("nc", "1.10", "net", "TCP/UDP 收发"),
    ("nmap", "7.94", "net", "主机与端口扫描"),
    ("aria2", "1.37", "net", "多线程下载器"),
    ("nginx", "1.26.0", "net", "HTTP 服务器"),
    ("mosquitto", "2.0.18", "net", "MQTT 代理"),
    # ---- edit：编辑与终端 ----
    ("vim", "9.1", "edit", "Vi 编辑器"),
    ("nano", "8.0", "edit", "轻量编辑器"),
    ("emacs", "29.4", "edit", "Emacs 编辑器"),
    ("tmux", "3.4", "edit", "终端复用器"),
    ("screen", "4.9.1", "edit", "会话保持"),
    ("less", "643", "edit", "分页查看"),
    # ---- sys：系统工具 ----
    ("htop", "3.3.0", "sys", "进程监视器"),
    ("tree", "2.1.1", "sys", "目录树"),
    ("jq", "1.7.1", "sys", "JSON 处理器"),
    ("unzip", "6.0", "sys", "ZIP 解包"),
    ("zip", "3.0", "sys", "ZIP 打包"),
    ("tar", "1.35", "sys", "归档工具"),
    ("gzip", "1.13", "sys", "gzip 压缩"),
    ("xz", "5.6.2", "sys", "xz 压缩"),
    ("strace", "6.9", "sys", "系统调用跟踪"),
    ("lsof", "4.99", "sys", "打开文件查看"),
    ("gdb", "14.2", "sys", "调试器"),
    ("coreutils", "9.5", "sys", "标准命令集"),
    ("bash", "5.2.26", "sys", "Bash 外壳"),
    ("zsh", "5.9", "sys", "Zsh 外壳"),
    ("busybox", "1.36.1", "sys", "单文件工具集"),
    # ---- db：数据库 ----
    ("sqlite3", "3.46.0", "db", "嵌入式数据库"),
    ("redis-cli", "7.2.5", "db", "Redis 客户端"),
    # ---- media：多媒体 ----
    ("ffmpeg", "6.1.1", "media", "音视频转码"),
    ("imagemagick", "7.1.1", "media", "图像处理"),
    ("sox", "14.4", "media", "音频处理"),
    # ---- sec：安全 ----
    ("openssl", "3.3.1", "sec", "TLS 与密码学工具"),
    ("gnupg", "2.4.5", "sec", "签名与加密"),
    ("age", "1.1.1", "sec", "简洁加密工具"),
    # ---- android：安卓工具链 ----
    ("adb", "35.0.1", "android", "Android 调试桥"),
    ("fastboot", "35.0.1", "android", "Android 刷机工具"),
    ("aapt2", "35.0.1", "android", "资源打包与编译"),
    ("d8", "35.0.1", "android", "DEX 编译器"),
    ("apksigner", "35.0.1", "android", "APK 签名"),
    ("zipalign", "35.0.1", "android", "APK 对齐"),
    ("sdkmanager", "12.0", "android", "SDK 组件管理"),
    ("ndk", "27.0.1", "android", "原生开发工具链"),
]

CATS = [
    ("lang", "语言与运行时"),
    ("build", "构建工具链"),
    ("vcs", "版本控制"),
    ("net", "网络"),
    ("edit", "编辑与终端"),
    ("sys", "系统工具"),
    ("db", "数据库"),
    ("media", "多媒体"),
    ("sec", "安全"),
    ("android", "安卓工具链"),
]

MAGIC = b"ASEPKG1\x00"
HDR = 96
BODY = 48


def build(name, ver, cat, desc):
    body = bytes(((ord(name[i % len(name)]) ^ (i * 7)) & 0xFF) for i in range(BODY))
    head = bytearray()
    head += MAGIC
    head += name.encode().ljust(16, b"\x00")
    head += ver.encode().ljust(12, b"\x00")
    head += struct.pack("<I", len(body))
    head += struct.pack("<I", zlib.crc32(body) & 0xFFFFFFFF)
    head += cat.encode().ljust(8, b"\x00")
    d = desc.encode()
    while len(d) > 44:  # 按字节截断会切坏 UTF-8，按字符退
        d = d[:-1]
    head += d.ljust(44, b"\x00")
    return bytes(head) + body


def gen_rust(path):
    out = []
    out.append("// 由 tools/mkpkg.py 生成，勿手改。加包请改该脚本的 PKGS 列表后重跑。")
    out.append("")
    out.append("pub struct PkgEnt {")
    out.append("    pub name: &'static str,")
    out.append("    pub ver: &'static str,")
    out.append("    /// lang / build / vcs / net / edit / sys / db / media / sec / android")
    out.append("    pub cat: &'static str,")
    out.append("    pub size: u32,")
    out.append("    pub desc: &'static str,")
    out.append("    pub path: &'static str,")
    out.append("    pub blob: &'static [u8],")
    out.append("}")
    out.append("")
    out.append(f"pub const NPKG: usize = {len(PKGS)};")
    out.append("")
    out.append("pub static PKGS: [PkgEnt; NPKG] = [")
    for name, ver, cat, desc in PKGS:
        out.append(
            '    PkgEnt { name: "%s", ver: "%s", cat: "%s", size: %d, '
            'desc: "%s（占位包体）", path: "/pkg-%s.ase", blob: '
            'include_bytes!("../../assets/pkg/%s.ase") },'
            % (name, ver, cat, HDR + BODY, desc, name, name)
        )
    out.append("];")
    out.append("")
    out.append("/// 分类顺序：界面按这个顺序分组，也决定 pkg list 的组顺序")
    out.append("pub static CATS: [(&str, &str); %d] = [" % len(CATS))
    for c, label in CATS:
        out.append('    ("%s", "%s"),' % (c, label))
    out.append("];")
    open(path, "w", encoding="utf-8").write("\n".join(out) + "\n")


out = os.path.join("assets", "pkg")
os.makedirs(out, exist_ok=True)
for old in os.listdir(out):
    os.unlink(os.path.join(out, old))
for name, ver, cat, desc in PKGS:
    open(os.path.join(out, f"{name}.ase"), "wb").write(build(name, ver, cat, desc))

gen_rust(os.path.join("kernel", "src", "pkgidx.rs"))

cnt = {}
for _, _, c, _ in PKGS:
    cnt[c] = cnt.get(c, 0) + 1
print(f"{len(PKGS)} 个包 → {out}/（每个 {HDR + BODY} 字节）")
print("分类：" + "  ".join(f"{k}={v}" for k, v in cnt.items()))
print("索引表：kernel/src/pkgidx.rs")
