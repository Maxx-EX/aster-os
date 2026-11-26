// Aster Kernel — 构建脚本：把 C 侧支持代码编译成静态库再交给 rustc 链接
use std::env;
use std::path::PathBuf;
use std::process::Command;

fn main() {
    let out = PathBuf::from(env::var("OUT_DIR").unwrap());
    let manifest = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap());
    let c_dir = manifest.join("c");

    let cc = env::var("ASTER_CC").unwrap_or_else(|_| "aarch64-linux-gnu-gcc".to_string());
    let flags = [
        "-c", "-O2", "-ffreestanding", "-fno-builtin",
        "-fno-stack-protector", "-nostdlib",
        "-mgeneral-regs-only", "-DARMTARGET",
    ];

    let sources = ["aster_c.c", "sha256.c"];
    let mut objs = Vec::new();
    for src in sources {
        let obj = out.join(src.replace(".c", ".o"));
        let status = Command::new(&cc)
            .args(&flags)
            .arg(c_dir.join(src))
            .arg("-o")
            .arg(&obj)
            .status()
            .expect("调用交叉编译器失败");
        if !status.success() {
            panic!("编译 {} 失败", src);
        }
        objs.push(obj);
    }

    let ar = env::var("ASTER_AR").unwrap_or_else(|_| "aarch64-linux-gnu-ar".to_string());
    let lib = out.join("libaster_c.a");
    let _ = std::fs::remove_file(&lib);
    let status = Command::new(&ar)
        .arg("rcs")
        .arg(&lib)
        .args(&objs)
        .status()
        .expect("调用交叉 ar 失败");
    if !status.success() {
        panic!("打包 libaster_c.a 失败");
    }

    println!("cargo:rustc-link-search=native={}", out.display());
    println!("cargo:rustc-link-lib=static=aster_c");
    println!("cargo:rerun-if-changed=c");
}
