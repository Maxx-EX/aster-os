// Aster Kernel — 用户态任务与调度启动（阶段二·抢占）
//
// run  ：把同一份用户程序装进槽 A、槽 B，登记两个任务，开定时器，切 EL0。
//        之后 CPU 每 2 ms 被定时器打断一次，调度器在陷阱帧上换人 ——
//        这就是"一个死循环程序不会锁死整机"的底气。
// runf ：单任务，故意越权读内核地址，验证隔离。
use crate::{exceptions, mmu, sched, timer};

include!(concat!(env!("OUT_DIR"), "/user_image.rs"));

fn load(bin: &[u8], size: usize, code: u64) -> Result<(), &'static str> {
    if size == 0 {
        return Err("内核构建时没有内嵌该用户程序（先 make user）");
    }
    if !mmu::load_user_image(bin, code) {
        return Err("用户程序大于一页（4 KiB），拒绝装载");
    }
    Ok(())
}

/// 两个任务 + 定时器抢占
pub fn run_two() -> Result<u64, &'static str> {
    load(USER_HELLO, USER_HELLO_SIZE, mmu::slot_a::CODE)?;
    load(USER_HELLO, USER_HELLO_SIZE, mmu::slot_b::CODE)?;

    sched::reset();
    if sched::spawn(mmu::slot_a::CODE, mmu::slot_a::STACK_TOP).is_none()
        || sched::spawn(mmu::slot_b::CODE, mmu::slot_b::STACK_TOP).is_none()
    {
        return Err("任务表已满");
    }

    crate::println!(
        "[TASK] 已登记 {} 个任务，时间片 {} ms（GICv2 PPI 30）",
        sched::count(),
        timer::SLICE_MS
    );
    crate::println!("[TASK] 切换 EL0 —— 同一个程序装在两个槽里，靠 getpid 区分自己");

    timer::arm();
    let code = exceptions::enter_user_state(mmu::slot_a::CODE, mmu::slot_a::STACK_TOP);
    Ok(code)
}

/// 抢占验证：槽 A 放纯计算任务（不调 syscall），槽 B 放打印任务。
/// 如果 B 的钱印落在 A 的 BUSY start / BUSY end 之间，说明抢占真的在发生。
pub fn run_preempt_test() -> Result<u64, &'static str> {
    load(USER_BUSY, USER_BUSY_SIZE, mmu::slot_a::CODE)?;
    load(USER_HELLO, USER_HELLO_SIZE, mmu::slot_b::CODE)?;

    sched::reset();
    if sched::spawn(mmu::slot_a::CODE, mmu::slot_a::STACK_TOP).is_none()
        || sched::spawn(mmu::slot_b::CODE, mmu::slot_b::STACK_TOP).is_none()
    {
        return Err("任务表已满");
    }
    crate::println!("[TASK] 槽 A = 纯计算（无 syscall），槽 B = 打印任务，观察输出顺序");
    timer::arm();
    let code = exceptions::enter_user_state(mmu::slot_a::CODE, mmu::slot_a::STACK_TOP);
    Ok(code)
}

/// 单任务越权演示
pub fn run_fault() -> Result<u64, &'static str> {
    load(USER_FAULT, USER_FAULT_SIZE, mmu::slot_a::CODE)?;
    sched::reset();
    if sched::spawn(mmu::slot_a::CODE, mmu::slot_a::STACK_TOP).is_none() {
        return Err("任务表已满");
    }
    crate::println!("[TASK] 装载越权样本，切 EL0 执行 ld 0x40200000");
    timer::arm();
    let code = exceptions::enter_user_state(mmu::slot_a::CODE, mmu::slot_a::STACK_TOP);
    Ok(code)
}
