// Aster Kernel — 用户态任务（阶段二：单任务、同步系统调用）
//
// 现在还没有调度器：命令把用户程序载入 0x4440_0000，
// 切到 EL0 跑完（靠 svc exit 返回），再回到 shell。
// 抢占式调度、多任务、地址空间切换是下一步的事。
use crate::exceptions;
use crate::mmu;

include!(concat!(env!("OUT_DIR"), "/user_image.rs"));

pub const ENTRY: u64 = mmu::USER_CODE;
pub const STACK_TOP: u64 = mmu::USER_STACK_TOP;

enum Demo {
    Hello,
    Fault,
}

fn image(d: Demo) -> (&'static str, &'static [u8], usize) {
    match d {
        Demo::Hello => ("hello", USER_HELLO, USER_HELLO_SIZE),
        Demo::Fault => ("fault", USER_FAULT, USER_FAULT_SIZE),
    }
}

fn run(d: Demo) -> Result<u64, &'static str> {
    let (name, bytes, size) = image(d);
    if size == 0 {
        return Err("内核构建时没有内嵌该用户程序（先 make user）");
    }
    if !mmu::load_user_image(bytes) {
        return Err("用户程序大于一页（4 KiB），拒绝装载");
    }
    crate::println!(
        "[TASK] 载入 {} {} bytes → {:#x}，栈顶 {:#x}",
        name,
        size,
        ENTRY,
        STACK_TOP
    );
    crate::println!("[TASK] 切换到 EL0（用户态），特权指令与外设从此不可直接访问");
    let code = exceptions::enter_user_state(ENTRY, STACK_TOP);
    Ok(code)
}

/// 正常演示：若干次系统调用后 exit(7)
pub fn run_hello() -> Result<u64, &'static str> {
    run(Demo::Hello)
}

/// 越权演示：故意从 EL0 读 0x4020_0000（内核镜像），应被 MMU 拦下
pub fn run_fault() -> Result<u64, &'static str> {
    run(Demo::Fault)
}
