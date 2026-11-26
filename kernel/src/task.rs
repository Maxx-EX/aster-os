// Aster Kernel — 用户态任务与调度启动（阶段二·抢占）
//
// run  ：把同一份用户程序装进槽 A、槽 B，登记两个任务，开定时器，切 EL0。
//        之后 CPU 每 2 ms 被定时器打断一次，调度器在陷阱帧上换人 ——
//        这就是"一个死循环程序不会锁死整机"的底气。
// runf ：单任务，故意越权读内核地址，验证隔离。
use crate::{exceptions, mmu, sched, timer};

include!(concat!(env!("OUT_DIR"), "/user_image.rs"));

/// 新建地址空间并把程序装进去
fn make_space(bin: &[u8], size: usize) -> Result<mmu::Space, &'static str> {
    if size == 0 {
        return Err("内核构建时没有内嵌该用户程序（先 make user）");
    }
    let sp = mmu::new_space().ok_or("页帧分配器耗尽（池 128 MiB）")?;
    mmu::load_image(&sp, bin)?;
    Ok(sp)
}

/// 两个任务 + 定时器抢占
pub fn run_two() -> Result<u64, &'static str> {
    sched::reset();
    let sa = make_space(USER_HELLO, USER_HELLO_SIZE)?;
    let sb = make_space(USER_HELLO, USER_HELLO_SIZE)?;

    if sched::spawn(sa).is_none() || sched::spawn(sb).is_none() {
        return Err("任务表已满");
    }

    crate::println!(
        "[TASK] 已登记 {} 个任务，时间片 {} ms（GICv2 PPI 30）",
        sched::count(),
        timer::SLICE_MS
    );
    let (used, total, bytes) = crate::frame::stats();
    crate::println!(
        "[MM] 页帧池 {} / {} 页已用（{} KiB），每个任务独占 5 页",
        used, total, bytes / 1024
    );
    crate::println!("[TASK] 切换 EL0 —— 两个任务 VA 相同、物理页不同，靠换 TTBR0 隔离");

    timer::arm();
    mmu::activate(&sa);
    let code = exceptions::enter_user_state(mmu::user_va::CODE, mmu::user_va::STACK_TOP);
    Ok(code)
}

/// 抢占验证：槽 A 放纯计算任务（不调 syscall），槽 B 放打印任务。
/// 如果 B 的钱印落在 A 的 BUSY start / BUSY end 之间，说明抢占真的在发生。
pub fn run_preempt_test() -> Result<u64, &'static str> {
    sched::reset();
    let sa = make_space(USER_BUSY, USER_BUSY_SIZE)?;
    let sb = make_space(USER_HELLO, USER_HELLO_SIZE)?;

    if sched::spawn(sa).is_none() || sched::spawn(sb).is_none() {
        return Err("任务表已满");
    }
    crate::println!("[TASK] 任务 A = 纯计算（无 syscall），任务 B = 打印任务，观察输出顺序");
    timer::arm();
    mmu::activate(&sa);
    let code = exceptions::enter_user_state(mmu::user_va::CODE, mmu::user_va::STACK_TOP);
    Ok(code)
}

/// 单任务越权演示
pub fn run_fault() -> Result<u64, &'static str> {
    sched::reset();
    let sa = make_space(USER_FAULT, USER_FAULT_SIZE)?;
    if sched::spawn(sa).is_none() {
        return Err("任务表已满");
    }
    crate::println!("[TASK] 装载越权样本，切 EL0 执行 ld 0x40200000");
    timer::arm();
    mmu::activate(&sa);
    let code = exceptions::enter_user_state(mmu::user_va::CODE, mmu::user_va::STACK_TOP);
    Ok(code)
}


/// 地址空间隔离验证：两个任务写同一个虚拟地址，看会不会互相覆盖
pub fn run_iso() -> Result<u64, &'static str> {
    sched::reset();
    let sa = make_space(USER_ISO, USER_ISO_SIZE)?;
    let sb = make_space(USER_ISO, USER_ISO_SIZE)?;
    if sched::spawn(sa).is_none() || sched::spawn(sb).is_none() {
        return Err("任务表已满");
    }
    crate::println!("[ISO] 两个任务写同一 VA 0x4440_0800，各自读回：'.' = 隔离成功，'X' = 串了");
    timer::arm();
    mmu::activate(&sa);
    let code = exceptions::enter_user_state(mmu::user_va::CODE, mmu::user_va::STACK_TOP);
    Ok(code)
}
