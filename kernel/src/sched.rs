// Aster Kernel — 任务表与轮转调度（阶段二·抢占）
//
// 上下文用统一的"陷阱帧"表示，34 个 u64：
//   [0..30]  x0..x30
//   [31]     SP_EL0
//   [32]     ELR_EL1
//   [33]     SPSR_EL1
//
// 这样 IRQ 抢占时不需要另造一套保存逻辑：把帧存进 Task.ctx，
// 把下一个任务的 ctx 写回同一帧，`eret` 出去就已经是另一个任务了。
//
// 注意：这一版所有任务共用同一张 TTBR0 页表，只是各自占不同的 2 MiB 区
// （A: 0x4440_0000，B: 0x4460_0000）。任务之间互相读得到对方的页 ——
// 真正的地址空间隔离要等每个任务一张页表，见 docs/roadmap.md 阶段二 #4。
#![allow(static_mut_refs)]

pub const FRAME_WORDS: usize = 100;
pub const IX_SP_EL0: usize = 31;
pub const IX_ELR: usize = 32;
pub const IX_SPSR: usize = 33;

#[derive(Clone, Copy, PartialEq)]
pub enum State {
    Ready,
    Dead,
}

#[derive(Clone, Copy)]
pub struct Task {
    pub ctx: [u64; FRAME_WORDS],
    pub state: State,
    pub id: u32,
    pub switches: u32,
    /// 该任务自己的地址空间（L1/L2/L3 + 代码页 + 栈页），切任务时换 TTBR0
    pub space: crate::mmu::Space,
}

const MAX_TASKS: usize = 4;

impl Task {
    const EMPTY: Task = Task {
        ctx: [0; FRAME_WORDS],
        state: State::Dead,
        id: 0,
        switches: 0,
        space: crate::mmu::Space::EMPTY,
    };
}

static mut TASKS: [Task; MAX_TASKS] = [Task::EMPTY; MAX_TASKS];
static mut NTASKS: usize = 0;
static mut CURRENT: usize = 0;
static mut TOTAL_SWITCHES: u32 = 0;

/// 登记一个任务：给它一个地址空间，返回任务号（同时作为 getpid 的返回值）
pub fn spawn(space: crate::mmu::Space) -> Option<u32> {
    if !space.valid() {
        return None;
    }
    unsafe {
        if NTASKS >= MAX_TASKS {
            return None;
        }
        let i = NTASKS;
        let id = (i + 1) as u32;
        let mut t = Task::EMPTY;
        t.state = State::Ready;
        t.id = id;
        t.space = space;
        // 所有任务的 VA 布局一致，只是背后的物理页各自独立
        t.ctx[IX_SP_EL0] = crate::mmu::user_va::STACK_TOP;
        t.ctx[IX_ELR] = crate::mmu::user_va::CODE;
        t.ctx[IX_SPSR] = 0; // EL0t、DAIF 全开
        TASKS[i] = t;
        NTASKS += 1;
        Some(id)
    }
}

/// 重新开始一批任务（shell 里第二次 run 之前调用）
pub fn reset() {
    unsafe {
        NTASKS = 0;
        CURRENT = 0;
        TOTAL_SWITCHES = 0;
        for t in TASKS.iter_mut() {
            *t = Task::EMPTY;
        }
    }
}

pub fn switches_total() -> u32 {
    unsafe { TOTAL_SWITCHES }
}

pub fn count() -> usize {
    unsafe { NTASKS }
}

pub fn current_id() -> u32 {
    unsafe {
        if CURRENT < NTASKS {
            TASKS[CURRENT].id
        } else {
            0
        }
    }
}

unsafe fn pick_next() -> Option<usize> {
    if NTASKS == 0 {
        return None;
    }
    for k in 1..=NTASKS {
        let i = (CURRENT + k) % NTASKS;
        if TASKS[i].state == State::Ready {
            return Some(i);
        }
    }
    None
}

unsafe fn load_into(frame: &mut [u64], idx: usize) {
    // 先换地址空间：下一行返回用户态的指令就只能看到新任务的页
    crate::mmu::activate(&TASKS[idx].space);
    frame.copy_from_slice(&TASKS[idx].ctx);
    TASKS[idx].switches = TASKS[idx].switches.saturating_add(1);
    CURRENT = idx;
    TOTAL_SWITCHES = TOTAL_SWITCHES.saturating_add(1);
}

/// 定时器中断里调用：保存当前任务、切到下一个 Ready 任务
pub fn tick(frame: *mut u64) {
    unsafe {
        let mut f = core::slice::from_raw_parts_mut(frame, FRAME_WORDS);

        if CURRENT < NTASKS && TASKS[CURRENT].state == State::Ready {
            TASKS[CURRENT].ctx.copy_from_slice(&f);
            TASKS[CURRENT].state = State::Ready;
        }

        if let Some(n) = pick_next() {
            load_into(&mut f, n);
        } else {
            // 没有别的任务可跑：保持当前上下文原样（等价于这次 tick 白给）
            if CURRENT < NTASKS {
                TASKS[CURRENT].ctx.copy_from_slice(&f);
            }
        }
    }
}

/// 当前任务主动让出 CPU（sched_yield）。帧换人后返回 true。
pub fn yield_current(frame: *mut u64) -> bool {
    unsafe {
        let mut f = core::slice::from_raw_parts_mut(frame, FRAME_WORDS);
        if CURRENT < NTASKS && TASKS[CURRENT].state == State::Ready {
            TASKS[CURRENT].ctx.copy_from_slice(&f);
        }
        match pick_next() {
            Some(n) => {
                load_into(&mut f, n);
                true
            }
            None => false,
        }
    }
}

/// 当前任务调用 exit：标记 Dead 并把 CPU 交给下一个任务。
/// 返回 true = 还有任务在跑（帧已换人）；false = 全跑完了，该回内核。
pub fn exit_current(frame: *mut u64) -> bool {
    unsafe {
        let mut f = core::slice::from_raw_parts_mut(frame, FRAME_WORDS);
        if CURRENT < NTASKS {
            TASKS[CURRENT].state = State::Dead;
        }
        match pick_next() {
            Some(n) => {
                load_into(&mut f, n);
                true
            }
            None => {
                // 所有任务都结束了：切回内核自己的页表再回 shell
                crate::exceptions::activate_kernel_space();
                false
            }
        }
    }
}

/// 当前每个任务占的物理页数（调试用）
pub fn space_count() -> usize {
    unsafe {
        let mut n = 0;
        for i in 0..NTASKS {
            if TASKS[i].space.valid() {
                n += 1;
            }
        }
        n
    }
}

pub fn stats() -> &'static str {
    struct Buf {
        d: [u8; 96],
        n: usize,
    }
    impl core::fmt::Write for Buf {
        fn write_str(&mut self, s: &str) -> core::fmt::Result {
            for &b in s.as_bytes() {
                if self.n < 95 {
                    self.d[self.n] = b;
                    self.n += 1;
                }
            }
            Ok(())
        }
    }
    unsafe {
        static mut B: Buf = Buf { d: [0; 96], n: 0 };
        B.n = 0;
        let _ = core::fmt::write(
            &mut B,
            format_args!("任务数 {}，总切换 {}", NTASKS, TOTAL_SWITCHES),
        );
        B.d[B.n] = 0;
        core::str::from_utf8_unchecked(&B.d[..B.n])
    }
}
