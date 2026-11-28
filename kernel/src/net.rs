// Aster Kernel — virtio-net 驱动 + 极简 IPv4 栈（ARP / ICMP / TCP）
//
// 目标很具体：宿主能 ping 通、能用浏览器打开 Aster 的页面。
// 为了做到这一点，必须自己算 ARP 应答、ICMP 校验和、TCP 三段握手与校验和——
// 少一样，宿主那边的协议栈就不认。
//
// 网络配置沿用 QEMU user-mode 网络的约定：guest 10.0.2.15/24、网关 10.0.2.2。
#![allow(dead_code, static_mut_refs)]

use crate::virtio::{self, Avail, Desc, Device, Queue, Used};

pub const IP: [u8; 4] = [10, 0, 2, 15];

// NET-4：DHCP 会改写这三项，常量只作为 slirp 的默认值
static mut OUR_IP: [u8; 4] = IP;
static mut OUR_GW: [u8; 4] = GW;
static mut OUR_DNS: [u8; 4] = [10, 0, 2, 3];
static mut DHCP_OK: bool = false;

pub fn ip() -> [u8; 4] {
    unsafe { OUR_IP }
}

pub fn gw() -> [u8; 4] {
    unsafe { OUR_GW }
}

pub fn dns() -> [u8; 4] {
    unsafe { OUR_DNS }
}

pub fn dhcp_ok() -> bool {
    unsafe { DHCP_OK }
}
pub const MASK: [u8; 4] = [255, 255, 255, 0];
pub const GW: [u8; 4] = [10, 0, 2, 2];
pub const HTTP_PORT: u16 = 80;
const RX_BUFS: usize = 8;
const TX_BUFS: usize = 4;
const BUF_SIZE: usize = 2048;
const HDR_LEN: usize = 12; // virtio_net_hdr + num_buffers（协商了 MRG_RXBUF）

const F_MRG_RXBUF: u64 = 1 << 15;
const F_MAC: u64 = 1 << 5;

// ---------------- 静态内存 ----------------

// legacy 队列要求 desc|avail|[4K 对齐]used 在同一块连续页内存里，所以这里给每个队列
// 开一块 8 KiB、页对齐的原始内存，内部再切分，两种模式共用同一套指针。
const QSIZE: u16 = 8;

#[repr(C, align(4096))]
struct QMem {
    raw: [u8; 8192],
}

static mut RX_MEM: QMem = QMem { raw: [0; 8192] };
static mut TX_MEM: QMem = QMem { raw: [0; 8192] };

/// 从原始内存里切出 (desc, avail, used)
unsafe fn q_parts(mem: *mut u8, size: u16) -> (*mut Desc, *mut Avail, *mut Used) {
    let desc = mem as *mut Desc;
    let avail = mem.add((size as usize) * core::mem::size_of::<Desc>()) as *mut Avail;
    let used = mem.add(4096) as *mut Used;
    (desc, avail, used)
}

static mut RX_BUF: [[u8; BUF_SIZE]; RX_BUFS] = [[0; BUF_SIZE]; RX_BUFS];
static mut TX_BUF: [[u8; BUF_SIZE]; TX_BUFS] = [[0; BUF_SIZE]; TX_BUFS];

static mut RXQ: Queue = Queue::empty();
static mut TXQ: Queue = Queue::empty();
static mut DEV: Option<Device> = None;
static mut MAC: [u8; 6] = [0x52, 0x54, 0x00, 0x12, 0x34, 0x56];

static mut HAVE_NET: bool = false;
static mut GW_MAC: [u8; 6] = [0x52, 0x55, 0x0a, 0x00, 0x02, 0x02];
static mut GW_KNOWN: bool = false;
static mut RX_PENDING: u16 = 0;

// 统计
static mut RX_FRAMES: u32 = 0;
static mut TX_FRAMES: u32 = 0;
static mut TCP_CONNS: u32 = 0;

// ---------------- 初始化 ----------------

pub fn init() -> bool {
    let dev = match virtio::find(virtio::DEV_NET, 0) {
        Some(d) => d,
        None => return false,
    };
    unsafe { DEV = Some(dev) };

    dev.reset();
    let mut st = 0;
    st |= virtio::STATUS_ACK;
    dev.set_status(st);
    st |= virtio::STATUS_DRIVER;
    dev.set_status(st);

    let feats = dev.device_features();
    let want = feats & (F_MRG_RXBUF | F_MAC);
    dev.set_features(want);

    if dev.version == 2 {
        st |= virtio::STATUS_FEATURES_OK;
        dev.set_status(st);
        if dev.status() & virtio::STATUS_FEATURES_OK == 0 {
            crate::println!("[NET] 设备拒绝了特性协商");
            return false;
        }
    }

    if want & F_MAC != 0 {
        for i in 0..6 {
            let b = dev.read_config_u8(i as u64);
            unsafe { MAC[i] = b };
        }
    }

    unsafe {
        let rx_mem = core::ptr::addr_of_mut!(RX_MEM) as *mut u8;
        let tx_mem = core::ptr::addr_of_mut!(TX_MEM) as *mut u8;
        let (rd, ra, ru) = q_parts(rx_mem, QSIZE);
        let (td, ta, tu) = q_parts(tx_mem, QSIZE);

        RXQ.bind(rd, ra, ru, QSIZE);
        TXQ.bind(td, ta, tu, QSIZE);

        let ok = if dev.version == 2 {
            dev.setup_queue(0, rd, ra, ru, QSIZE) && dev.setup_queue(1, td, ta, tu, QSIZE)
        } else {
            dev.setup_queue_legacy(0, rx_mem as u64, QSIZE)
                && dev.setup_queue_legacy(1, tx_mem as u64, QSIZE)
        };
        if !ok {
            crate::println!("[NET] 队列建立失败");
            return false;
        }

        // RX：把 8 个缓冲全部挂上去
        for i in 0..RX_BUFS {
            let d = RXQ.alloc().unwrap();
            let buf = core::ptr::addr_of!(RX_BUF[i]) as u64;
            RXQ.set(d, buf, BUF_SIZE as u32, virtio::DESC_F_WRITE, 0);
            RXQ.submit(d);
        }
        RX_PENDING = RX_BUFS as u16;
    }

    dev.add_status(virtio::STATUS_DRIVER_OK);
    dev.notify(0);
    unsafe { HAVE_NET = true };
    true
}

pub fn gw_known() -> bool {
    unsafe { GW_KNOWN }
}

pub fn available() -> bool {
    unsafe { HAVE_NET }
}

pub fn mac() -> [u8; 6] {
    unsafe { MAC }
}

/// 调试信息：(设备版本, 设备状态, RX avail.idx, RX used.idx, 设备 interrupt status)
pub fn debug_info() -> (u32, u32, u16, u16, u32) {
    unsafe {
        let dev = match DEV {
            Some(d) => d,
            None => return (0, 0, 0, 0, 0),
        };
        (
            dev.version,
            dev.status(),
            (*RXQ.avail).idx,
            (*RXQ.used).idx,
            core::ptr::read_volatile((dev.base + 0x060) as *const u32),
        )
    }
}

pub fn stats() -> (u32, u32, u32) {
    unsafe { (RX_FRAMES, TX_FRAMES, TCP_CONNS) }
}

// ---------------- 收发 ----------------

pub fn poll() {
    if !available() {
        return;
    }
    unsafe {
        let dev = match DEV {
            Some(d) => d,
            None => return,
        };
        let mut got = false;
        loop {
            let (id, len) = match RXQ.pop_used() {
                Some(v) => v,
                None => break,
            };
            let idx = (id as usize) % RX_BUFS;
            if (len as usize) > HDR_LEN {
                RX_FRAMES += 1;
                handle_frame(&RX_BUF[idx][HDR_LEN..len as usize]);
            }
            let base = core::ptr::addr_of!(RX_BUF[idx]) as u64;
            RXQ.set(id as u16, base, BUF_SIZE as u32, virtio::DESC_F_WRITE, 0);
            RXQ.submit(id as u16);
            got = true;
        }
        if got {
            dev.notify(0);
        }
        // 我们不用中断，但设备的 interrupt status 会累积，顺手清掉
        let is = core::ptr::read_volatile((dev.base + 0x060) as *const u32);
        if is != 0 {
            core::ptr::write_volatile((dev.base + 0x064) as *mut u32, is);
        }
    }
}

fn send_frame(payload: &[u8]) -> bool {
    if !available() || payload.len() + HDR_LEN > BUF_SIZE {
        return false;
    }
    unsafe {
        let i = 0usize;
        let buf = core::ptr::addr_of_mut!(TX_BUF[i]) as *mut u8;
        core::ptr::write_bytes(buf, 0, HDR_LEN);
        for (k, b) in payload.iter().enumerate() {
            core::ptr::write_volatile(buf.add(HDR_LEN + k), *b);
        }
        let d = TXQ.alloc().unwrap();
        let addr = buf as u64;
        TXQ.set(d, addr, (HDR_LEN + payload.len()) as u32, 0, 0);
        TXQ.submit(d);
        let dev = DEV.unwrap();
        dev.notify(1);
        // 等设备归还（小流量下直接轮询，够了）
        for _ in 0..1_000_000u32 {
            // 描述符必须还给 free list，不然发几十个包之后 TX 环就空了
            if let Some((id, _len)) = TXQ.pop_used() {
                TXQ.free_chain(id as u16);
                TX_FRAMES += 1;
                return true;
            }
            core::hint::spin_loop();
        }
    }
    false
}

// ---------------- 以太网 ----------------

const ET_ARP: u16 = 0x0806;
const ET_IP: u16 = 0x0800;

fn handle_frame(f: &[u8]) {
    if f.len() < 14 {
        return;
    }
    let etype = u16::from_be_bytes([f[12], f[13]]);
    match etype {
        ET_ARP => handle_arp(f),
        ET_IP => handle_ip(f),
        _ => {}
    }
}

/// 主动发 ARP 请求。QEMU 的 slirp 只有在收到 guest 的帧之后才知道
/// "这个 IP 对应这个 MAC"，在此之前它没有可用的目的地址，宿主侧连接
/// 的 SYN 根本送不到 guest。所以开局必须自己先喊一嗓子。
fn send_arp_request(target: [u8; 4]) -> bool {
    let mut f = [0u8; 42];
    for i in 0..6 {
        f[i] = 0xff; // 广播
    }
    for i in 0..6 {
        f[6 + i] = unsafe { MAC[i] };
    }
    f[12..14].copy_from_slice(&ET_ARP.to_be_bytes());
    f[14..16].copy_from_slice(&1u16.to_be_bytes());
    f[16..18].copy_from_slice(&ET_IP.to_be_bytes());
    f[18] = 6;
    f[19] = 4;
    f[20..22].copy_from_slice(&1u16.to_be_bytes()); // request
    for i in 0..6 {
        f[22 + i] = unsafe { MAC[i] };
    }
    f[28..32].copy_from_slice(&ip());
    for i in 0..6 {
        f[32 + i] = 0;
    }
    f[38..42].copy_from_slice(&target);
    send_frame(&f)
}

fn handle_arp(f: &[u8]) {
    if f.len() < 42 {
        return;
    }
    let op = u16::from_be_bytes([f[20], f[21]]);
    if op == 2 {
        // 这是别人对我们 ARP 的回应，记下它的 MAC 供后续直接发送
        let sender_ip = [f[28], f[29], f[30], f[31]];
        let sender_mac = [f[22], f[23], f[24], f[25], f[26], f[27]];
        unsafe {
            if sender_ip == gw() {
                GW_MAC = sender_mac;
                GW_KNOWN = true;
            }
            ARP_IP = sender_ip;
            ARP_MAC = sender_mac;
        }
        return;
    }
    let tip = &f[38..42];
    if op != 1 || tip != &ip()[..] {
        return;
    }
    // who-has(我们的 IP) -> 回复我们的 MAC
    let mut r = [0u8; 42];
    for i in 0..6 {
        r[i] = f[6 + i]; // dst = 请求方
        r[6 + i] = unsafe { MAC[i] }; // src = 我们
    }
    r[12..14].copy_from_slice(&ET_ARP.to_be_bytes());
    r[14..16].copy_from_slice(&1u16.to_be_bytes()); // ethernet
    r[16..18].copy_from_slice(&ET_IP.to_be_bytes());
    r[18] = 6;
    r[19] = 4;
    r[20..22].copy_from_slice(&2u16.to_be_bytes()); // reply
    for i in 0..6 {
        r[22 + i] = unsafe { MAC[i] };
    }
    r[28..32].copy_from_slice(&ip());
    for i in 0..6 {
        r[32 + i] = f[22 + i]; // 回给请求方 MAC
    }
    for i in 0..4 {
        r[38 + i] = f[28 + i];
    }
    send_frame(&r);
}

fn csum_add(mut sum: u32, data: &[u8]) -> u32 {
    let mut i = 0;
    while i + 1 < data.len() {
        sum += ((data[i] as u32) << 8) | data[i + 1] as u32;
        i += 2;
    }
    if i < data.len() {
        sum += (data[i] as u32) << 8;
    }
    sum
}

fn csum_fold(mut sum: u32) -> u16 {
    while sum >> 16 != 0 {
        sum = (sum & 0xffff) + (sum >> 16);
    }
    !(sum as u16)
}

/// 构造以太网 + IPv4 + 协议头，返回 IP 包总长
fn build_ip(
    out: &mut [u8],
    src_ip: [u8; 4],
    dst_mac: [u8; 6],
    dst_ip: [u8; 4],
    proto: u8,
    payload_len: usize,
) -> usize {
    let mut m = [0u8; 6];
    m.copy_from_slice(&mac());
    out[0..6].copy_from_slice(&dst_mac);
    out[6..12].copy_from_slice(&m);
    out[12..14].copy_from_slice(&ET_IP.to_be_bytes());

    let ip_len = 20 + payload_len;
    out[14] = 0x45;
    out[15] = 0;
    out[16..18].copy_from_slice(&(ip_len as u16).to_be_bytes());
    out[18..20].copy_from_slice(&0u16.to_be_bytes());
    out[20..22].copy_from_slice(&0x4000u16.to_be_bytes()); // DF
    out[22] = 64;
    out[23] = proto;
    out[24..26].copy_from_slice(&0u16.to_be_bytes());
    out[26..30].copy_from_slice(&src_ip);
    out[30..34].copy_from_slice(&dst_ip);

    let hc = csum_fold(csum_add(0, &out[14..34]));
    out[24..26].copy_from_slice(&hc.to_be_bytes());
    14 + ip_len
}

fn handle_ip(f: &[u8]) {
    if f.len() < 34 {
        return;
    }
    let ihl = ((f[14] & 0x0f) as usize) * 4;
    if ihl < 20 || f.len() < 14 + ihl {
        return;
    }
    let proto = f[23];
    let src_ip = [f[26], f[27], f[28], f[29]];
    let dst_ip = [f[30], f[31], f[32], f[33]];
    if dst_ip != ip() && dst_ip != [255, 255, 255, 255] {
        return;
    }
    let src_mac = [f[6], f[7], f[8], f[9], f[10], f[11]];
    let total = u16::from_be_bytes([f[16], f[17]]) as usize;
    let body = &f[14 + ihl..(14 + total).min(f.len())];

    match proto {
        1 => handle_icmp(src_mac, src_ip, body),
        6 => handle_tcp(src_mac, src_ip, body),
        17 => handle_udp(src_ip, body),
        _ => {}
    }
}

fn handle_icmp(src_mac: [u8; 6], src_ip: [u8; 4], b: &[u8]) {
    if b.len() < 8 || b[0] != 8 {
        return;
    }
    // echo reply：改类型、重算校验和
    let mut resp = [0u8; 1500];
    let n = b.len().min(1400);
    resp[..n].copy_from_slice(&b[..n]);
    resp[0] = 0;
    resp[2..4].copy_from_slice(&0u16.to_be_bytes());
    let c = csum_fold(csum_add(0, &resp[..n]));
    resp[2..4].copy_from_slice(&c.to_be_bytes());

    let mut out = [0u8; 1514];
    let total = build_ip(&mut out, ip(), src_mac, src_ip, 1, n);
    out[34..34 + n].copy_from_slice(&resp[..n]);
    let total = 34 + n;
    send_frame(&out[..total]);
}

// ---------------- TCP ----------------

const TCP_FIN: u8 = 0x01;
const TCP_SYN: u8 = 0x02;
const TCP_RST: u8 = 0x04;
const TCP_PSH: u8 = 0x08;
const TCP_ACK: u8 = 0x10;

struct Conn {
    active: bool,
    phase: u8, // 0 关闭 1 SYN_RCVD 2 ESTAB 3 FIN_WAIT
    their_ip: [u8; 4],
    their_mac: [u8; 6],
    their_port: u16,
    our_seq: u32,
    their_seq: u32,
    acked: u32,
}

static mut CONN: Conn = Conn {
    active: false,
    phase: 0,
    their_ip: [0; 4],
    their_mac: [0; 6],
    their_port: 0,
    our_seq: 0,
    their_seq: 0,
    acked: 0,
};

fn tcp_csum(src: [u8; 4], dst: [u8; 4], seg: &[u8]) -> u16 {
    let mut sum = 0u32;
    sum = csum_add(sum, &src);
    sum = csum_add(sum, &dst);
    sum += 6u32; // protocol
    sum += seg.len() as u32;
    sum = csum_add(sum, seg);
    csum_fold(sum)
}

fn send_tcp(
    dst_mac: [u8; 6],
    dst_ip: [u8; 4],
    src_port: u16,
    dst_port: u16,
    seq: u32,
    ack: u32,
    flags: u8,
    payload: &[u8],
) -> bool {
    let mut out = [0u8; 1514];
    let total = build_ip(&mut out, ip(), dst_mac, dst_ip, 6, 20 + payload.len());
    let t = &mut out[34..];
    t[0..2].copy_from_slice(&src_port.to_be_bytes());
    t[2..4].copy_from_slice(&dst_port.to_be_bytes());
    t[4..8].copy_from_slice(&seq.to_be_bytes());
    t[8..12].copy_from_slice(&ack.to_be_bytes());
    t[12] = 5 << 4;
    t[13] = flags;
    t[14..16].copy_from_slice(&8192u16.to_be_bytes());
    t[16..18].copy_from_slice(&0u16.to_be_bytes());
    t[18..20].copy_from_slice(&0u16.to_be_bytes());
    t[20..20 + payload.len()].copy_from_slice(payload);
    let seg_len = 20 + payload.len();
    let c = tcp_csum(ip(), dst_ip, &out[34..34 + seg_len]);
    out[34 + 16..34 + 18].copy_from_slice(&c.to_be_bytes());
    let _ = total;
    send_frame(&out[..34 + seg_len])
}

// ---------------- HTTP 客户端（NET-4） ----------------
//
// 服务器方向的连接状态放在 CONN，客户端另起一套 CLI，两者互不干扰。
// 只做"发一个 GET、把响应整段收下来"这一件事：不做重传、不处理乱序
// （QEMU slirp 是本地回环，丢包乱序基本不会出现；真跑公网要补）。

pub const CLI_PORT: u16 = 49152;

struct Cli {
    on: bool,
    phase: u8, // 0 空闲 1 SYN 已发 2 已建立 3 收完
    dst_ip: [u8; 4],
    dst_mac: [u8; 6],
    dst_port: u16,
    seq: u32,   // 我方下一个要发的序号
    ack: u32,   // 期望对方下一个序号
    err: i32,   // 0 正常，负数是错误码
}

static mut CLI: Cli = Cli {
    on: false,
    phase: 0,
    dst_ip: [0; 4],
    dst_mac: [0; 6],
    dst_port: 0,
    seq: 0,
    ack: 0,
    err: 0,
};

/// 收到的东西先攒在这里，再由调用方决定写盘还是打印
pub const CLI_BUF_MAX: usize = 16384;
static mut CLI_BUF: [u8; CLI_BUF_MAX] = [0; CLI_BUF_MAX];
static mut CLI_LEN: usize = 0;

/// 最近一次 ARP 学到的 (ip, mac)，用于连非网关地址
static mut ARP_IP: [u8; 4] = [0; 4];
static mut ARP_MAC: [u8; 6] = [0; 6];

pub fn cli_len() -> usize {
    unsafe { CLI_LEN }
}

pub fn cli_data() -> &'static [u8] {
    unsafe { &CLI_BUF[..CLI_LEN] }
}

/// 学习任意 IP 的 MAC：先查缓存的网关，再广播 ARP，最多等 ms 毫秒
fn learn_mac(ip: [u8; 4], ms: u64) -> Option<[u8; 6]> {
    unsafe {
        if GW_KNOWN && ip == GW {
            return Some(GW_MAC);
        }
        if ARP_IP == ip && ARP_MAC != [0; 6] {
            return Some(ARP_MAC);
        }
    }
    if !send_arp_request(ip) {
        return None;
    }
    let t0 = crate::timer::now_ms();
    while crate::timer::now_ms() - t0 < ms {
        poll();
        unsafe {
            if ip == GW && GW_KNOWN {
                return Some(GW_MAC);
            }
            if ARP_IP == ip && ARP_MAC != [0; 6] {
                return Some(ARP_MAC);
            }
        }
    }
    None
}

/// 不同网段要走网关：目的 MAC 取网关的，目的 IP 还是目标的（NET-4）
fn route_mac(ip: [u8; 4], ms: u64) -> Option<[u8; 6]> {
    let o = self_ip();
    if ip[0] == o[0] && ip[1] == o[1] && ip[2] == o[2] {
        learn_mac(ip, ms)
    } else {
        learn_mac(gw(), ms)
    }
}

fn self_ip() -> [u8; 4] {
    ip()
}

/// 发一个 GET，把响应整段收进 CLI_BUF。返回响应字节数，负数表示失败。
///
/// -1 网络不可用　-2 ARP 拿不到 MAC　-3 连不上（超时/RST）　-4 收满缓冲
/// 解析 `host[:port][/path]` 并取一次 GET，返回 (body 起点, body 长度)；负数是错误码
pub fn fetch_url(url: &str, timeout_ms: u64) -> Result<(usize, usize), i32> {
    let a = if let Some(p) = url.find("://") { &url[p + 3..] } else { url };
    let mut host = a;
    let mut port: u16 = 80;
    let mut path = "/";
    if let Some(p) = a.find('/') {
        host = &a[..p];
        path = &a[p..];
    }
    if let Some(p) = host.find(':') {
        port = host[p + 1..].parse().unwrap_or(80);
        host = &host[..p];
    }
    let ip = match parse_ip(host) {
        Some(x) => x,
        None => match resolve(host, 900) {
            Some(x) => x,
            None => return Err(-5),
        },
    };
    let r = http_get(ip, port, path, timeout_ms);
    if r < 0 {
        return Err(r);
    }
    let raw = cli_data();
    let mut i = 0usize;
    while i + 3 < raw.len() {
        if raw[i] == b'\r' && raw[i + 1] == b'\n' && raw[i + 2] == b'\r' && raw[i + 3] == b'\n' {
            return Ok((i + 4, raw.len() - (i + 4)));
        }
        i += 1;
    }
    Err(-6)
}

pub fn http_get(ip: [u8; 4], port: u16, path: &str, timeout_ms: u64) -> i32 {
    if !available() {
        return -1;
    }
    let mac = match route_mac(ip, 400) {
        Some(m) => m,
        None => return -2,
    };
    unsafe {
        CLI = Cli {
            on: true,
            phase: 1,
            dst_ip: ip,
            dst_mac: mac,
            dst_port: port,
            seq: 0x2222_0000u32.wrapping_add(crate::timer::freq_hz() as u32 & 0xffff),
            ack: 0,
            err: 0,
        };
        CLI_LEN = 0;
        let isn = CLI.seq;
        let dst_port = CLI.dst_port;
        send_tcp(mac, ip, CLI_PORT, dst_port, isn, 0, TCP_SYN, &[]);
        CLI.seq = isn.wrapping_add(1);
    }

    let t0 = crate::timer::now_ms();
    let mut sent_req = false;
    loop {
        poll();
        unsafe {
            if CLI.phase == 0 {
                break; // 被 RST 或被 FIN 收尾
            }
            // 建连之后立刻发请求（只发一次）
            if CLI.phase == 2 && !sent_req {
                let mut req = [0u8; 256];
                let mut n = 0usize;
                let m = b"GET ";
                req[..m.len()].copy_from_slice(m);
                n += m.len();
                for b in path.as_bytes() {
                    if n < 200 {
                        req[n] = *b;
                        n += 1;
                    }
                }
                let tail = b" HTTP/1.0\r\nHost: aster\r\nUser-Agent: aster/0.9\r\nConnection: close\r\n\r\n";
                for b in tail {
                    if n < 250 {
                        req[n] = *b;
                        n += 1;
                    }
                }
                let s = CLI.seq;
                let a = CLI.ack;
                send_tcp(CLI.dst_mac, CLI.dst_ip, CLI_PORT, CLI.dst_port, s, a, TCP_ACK, &req[..n]);
                CLI.seq = s.wrapping_add(n as u32);
                sent_req = true;
            }
            if CLI.phase == 3 {
                break;
            }
        }
        if crate::timer::now_ms() - t0 > timeout_ms {
            unsafe {
                CLI.err = -3;
                CLI.phase = 0;
            }
            break;
        }
        // 给设备一点喘气时间，别把 poll 打成忙等
        for _ in 0..2000 {
            core::hint::spin_loop();
        }
    }
    unsafe {
        let e = CLI.err;
        CLI.on = false;
        if e != 0 {
            return e;
        }
        if CLI_LEN == 0 {
            return -3;
        }
        CLI_LEN as i32
    }
}

/// 客户端方向的 TCP 处理：由 handle_tcp 在开头分派进来
fn handle_tcp_client(src_mac: [u8; 6], src_ip: [u8; 4], src_port: u16, seq: u32, flags: u8, payload: &[u8]) {
    unsafe {
        if src_ip != CLI.dst_ip || src_port != CLI.dst_port {
            return;
        }
        if flags & TCP_RST != 0 {
            CLI.err = -3;
            CLI.phase = 0;
            return;
        }
        if flags & TCP_SYN != 0 && flags & TCP_ACK != 0 && CLI.phase == 1 {
            CLI.ack = seq.wrapping_add(1);
            CLI.phase = 2;
            let s = CLI.seq;
            let a = CLI.ack;
            send_tcp(src_mac, src_ip, CLI_PORT, CLI.dst_port, s, a, TCP_ACK, &[]);
            return;
        }
        if CLI.phase < 2 {
            return;
        }
        // 只收顺序到达的段：seq 必须等于期望值
        if !payload.is_empty() {
            if seq == CLI.ack {
                let room = CLI_BUF_MAX - CLI_LEN;
                let take = payload.len().min(room);
                CLI_BUF[CLI_LEN..CLI_LEN + take].copy_from_slice(&payload[..take]);
                CLI_LEN += take;
                CLI.ack = CLI.ack.wrapping_add(payload.len() as u32);
                if take < payload.len() {
                    CLI.err = -4;
                }
            }
            let s = CLI.seq;
            let a = CLI.ack;
            send_tcp(src_mac, src_ip, CLI_PORT, CLI.dst_port, s, a, TCP_ACK, &[]);
        }
        if flags & TCP_FIN != 0 {
            let a = CLI.ack.wrapping_add(1);
            CLI.ack = a;
            let s = CLI.seq;
            send_tcp(src_mac, src_ip, CLI_PORT, CLI.dst_port, s, a, TCP_ACK | TCP_FIN, &[]);
            CLI.seq = s.wrapping_add(1);
            CLI.phase = 3;
        }
    }
}

fn handle_tcp(src_mac: [u8; 6], src_ip: [u8; 4], b: &[u8]) {
    if b.len() < 20 {
        return;
    }
    let src_port = u16::from_be_bytes([b[0], b[1]]);
    let dst_port = u16::from_be_bytes([b[2], b[3]]);
    let seq = u32::from_be_bytes([b[4], b[5], b[6], b[7]]);
    let ack = u32::from_be_bytes([b[8], b[9], b[10], b[11]]);
    let doff = ((b[12] >> 4) as usize) * 4;
    let flags = b[13];
    if doff < 20 || b.len() < doff {
        return;
    }
    let payload = &b[doff..];

    // 客户端方向的段先交给 CLI（dst_port 是本方临时端口）
    if dst_port == CLI_PORT && unsafe { CLI.on } {
        handle_tcp_client(src_mac, src_ip, src_port, seq, flags, payload);
        return;
    }

    if flags & TCP_RST != 0 {
        unsafe {
            CONN.active = false;
            CONN.phase = 0;
        }
        return;
    }

    let ours = src_port == HTTP_PORT || dst_port == HTTP_PORT;

    unsafe {
        // 新建连接
        if flags & TCP_SYN != 0 && !CONN.active && dst_port == HTTP_PORT {
            CONN.active = true;
            CONN.phase = 1;
            CONN.their_ip = src_ip;
            CONN.their_mac = src_mac;
            CONN.their_port = src_port;
            CONN.their_seq = seq.wrapping_add(1);
            CONN.our_seq = 0x1234_0000u32.wrapping_add(crate::timer::freq_hz() as u32 & 0xfff);
            CONN.acked = seq.wrapping_add(1);
            TCP_CONNS += 1;
            let isn = CONN.our_seq;
            send_tcp(src_mac, src_ip, HTTP_PORT, src_port, isn, CONN.their_seq, TCP_SYN | TCP_ACK, &[]);
            CONN.our_seq = isn.wrapping_add(1);
            return;
        }

        if !CONN.active || !ours || src_ip != CONN.their_ip {
            return;
        }

        if flags & TCP_FIN != 0 && CONN.phase != 3 {
            CONN.phase = 3;
            let s = CONN.our_seq;
            CONN.our_seq = s.wrapping_add(1);
            send_tcp(src_mac, src_ip, HTTP_PORT, src_port, s, seq.wrapping_add(1), TCP_FIN | TCP_ACK, &[]);
            if payload.is_empty() {
                CONN.active = false;
                CONN.phase = 0;
            }
        }

        if CONN.phase == 1 && flags & TCP_ACK != 0 {
            CONN.phase = 2;
        }

        // HTTP 请求到达：回页面
        if !payload.is_empty() && CONN.phase >= 2 {
            let req_head: &[u8] = payload;
            let is_get = req_head.len() >= 4 && &req_head[0..4] == b"GET ";
            let body = if is_get { http_page() } else { "HTTP/1.0 400 Bad Request\r\n\r\n".as_bytes() };
            let mut hdr = [0u8; 512];
            let head_len = if is_get {
                build_http_head(&mut hdr)
            } else {
                let h = "HTTP/1.0 400 Bad Request\r\nContent-Length: 0\r\n\r\n";
                let n = h.len();
                hdr[..n].copy_from_slice(h.as_bytes());
                n
            };

            let mut sent = 0usize;
            let total = head_len + body.len();
            let mut seqn = CONN.our_seq;
            while sent < total {
                let chunk = (total - sent).min(1200);
                let mut seg = [0u8; 1400];
                let mut n = 0usize;
                if sent < head_len {
                    let take = (head_len - sent).min(chunk);
                    seg[..take].copy_from_slice(&hdr[sent..sent + take]);
                    n += take;
                }
                if n < chunk {
                    let off = sent + n - head_len;
                    let take = (chunk - n).min(body.len() - off);
                    seg[n..n + take].copy_from_slice(&body[off..off + take]);
                    n += take;
                }
                let last = sent + n >= total;
                let fl = TCP_ACK | if last { TCP_PSH | TCP_FIN } else { TCP_PSH };
                send_tcp(src_mac, src_ip, HTTP_PORT, src_port, seqn, seq.wrapping_add(payload.len() as u32), fl, &seg[..n]);
                seqn = seqn.wrapping_add(n as u32);
                if last {
                    seqn = seqn.wrapping_add(1); // FIN 占一个序号
                }
                sent += n;
            }
            CONN.our_seq = seqn;
            if sent >= total {
                CONN.phase = 3;
            }
        }
    }
}

fn build_http_head(out: &mut [u8]) -> usize {
    let s = "HTTP/1.0 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nConnection: close\r\n\r\n";
    out[..s.len()].copy_from_slice(s.as_bytes());
    s.len()
}

fn http_page() -> &'static [u8] {
    HTTP_PAGE.as_bytes()
}

/// 宿主浏览器打开 http://localhost:8080 看到的就是这一页
static HTTP_PAGE: &str = r#"(
<!DOCTYPE html>
<html lang="zh-CN"><head><meta charset="utf-8">
<meta name="viewport" content="width=device-width,initial-scale=1">
<title>Aster · 星芒</title>
<style>
:root{--bg:#0b1119;--card:#152130;--line:#2e4053;--text:#eaf2f8;--dim:#8fa3b0;--accent:#5dade2;--ok:#58d68d}
*{box-sizing:border-box;margin:0;padding:0}
body{background:linear-gradient(160deg,#0b1119 0%,#101c2a 60%,#0b1119 100%);color:var(--text);
font-family:-apple-system,"PingFang SC","Microsoft YaHei",system-ui,sans-serif;min-height:100vh;padding:32px}
.wrap{max-width:920px;margin:0 auto}
.brand{display:flex;align-items:center;gap:14px;margin-bottom:6px}
.logo{width:44px;height:44px;display:grid;grid-template-columns:repeat(2,18px);grid-gap:4px}
.logo i{background:var(--accent);border-radius:3px}
h1{font-size:30px;letter-spacing:3px;font-weight:700}
h1 span{font-size:15px;color:var(--dim);letter-spacing:2px;margin-left:10px;font-weight:400}
.sub{color:var(--dim);font-size:13px;margin:8px 0 26px}
.grid{display:grid;grid-template-columns:repeat(auto-fit,minmax(210px,1fr));gap:16px}
.card{background:var(--card);border:1px solid var(--line);border-radius:14px;padding:18px 20px}
.k{color:var(--dim);font-size:12px;letter-spacing:1px;margin-bottom:10px}
.v{font-size:19px;font-weight:600}
.v.ok{color:var(--ok)}
.v.acc{color:var(--accent)}
.mono{font-family:ui-monospace,Menlo,monospace;font-size:13px;color:var(--accent);word-break:break-all}
.note{margin-top:26px;color:var(--dim);font-size:12.5px;line-height:1.9}
.note b{color:var(--text);font-weight:600}
.pill{display:inline-block;padding:3px 10px;border:1px solid var(--line);border-radius:999px;font-size:11px;color:var(--dim);margin-left:8px}
</style></head>
<body><div class="wrap">
<div class="brand"><div class="logo"><i></i><i></i><i></i><i></i></div>
<h1>ASTER<span>星芒</span></h1></div>
<div class="sub">原创内核 · 无任何第三方内核依赖 · ARM64 <span class="pill">live from guest</span></div>
<div class="grid">
<div class="card"><div class="k">校验 EX3</div><div class="v ok">PASS</div></div>
<div class="card"><div class="k">引导链 AVP</div><div class="v">L1 · L2 · L3</div></div>
<div class="card"><div class="k">特权级</div><div class="v acc">EL0 / EL1</div></div>
<div class="card"><div class="k">调度</div><div class="v">2 ms 抢占</div></div>
<div class="card"><div class="k">网络</div><div class="v ok">10.0.2.15</div></div>
<div class="card"><div class="k">页面来源</div><div class="v acc">内核内 TCP</div></div>
</div>
<div class="card" style="margin-top:16px">
<div class="k">EX3 指纹</div><div class="mono">EX3-5994491FACE50680-CF53DC009337BDCD</div></div>
<div class="note">
这一页不是宿主机上的服务，是 <b>Aster 内核自己算的 ARP、ICMP 和 TCP</b> 发出来的：<br>
三次握手、20 字节 IP 头、TCP 校验和、按 1200 字节分段，全部在内核里完成。<br>
宿主的浏览器只是把它的字节渲染出来。
</div>
</div></body></html>
"#;

// ---------------- UDP（NET-4 的底座） ----------------

fn send_udp(
    dst_mac: [u8; 6],
    src_ip: [u8; 4],
    dst_ip: [u8; 4],
    src_port: u16,
    dst_port: u16,
    data: &[u8],
) -> bool {
    if data.len() + 8 + 34 > 1514 {
        return false;
    }
    let mut out = [0u8; 1514];
    build_ip(&mut out, src_ip, dst_mac, dst_ip, 17, 8 + data.len());
    let u = &mut out[34..];
    u[0..2].copy_from_slice(&src_port.to_be_bytes());
    u[2..4].copy_from_slice(&dst_port.to_be_bytes());
    u[4..6].copy_from_slice(&((8 + data.len()) as u16).to_be_bytes());
    u[6..8].copy_from_slice(&0u16.to_be_bytes()); // 校验和 0 = 不校验（IPv4 允许）
    u[8..8 + data.len()].copy_from_slice(data);
    send_frame(&out[..34 + 8 + data.len()])
}

fn handle_udp(src_ip: [u8; 4], b: &[u8]) {
    if b.len() < 8 {
        return;
    }
    let sport = u16::from_be_bytes([b[0], b[1]]);
    let dport = u16::from_be_bytes([b[2], b[3]]);
    let ulen = (u16::from_be_bytes([b[4], b[5]]) as usize).min(b.len()).max(8);
    let payload = &b[8..ulen];
    let _ = sport;
    match dport {
        DHCP_CLIENT_PORT => dhcp_on_reply(payload),
        DNS_LOCAL_PORT => dns_on_reply(payload, src_ip),
        _ => {}
    }
}

// ---------------- DHCP 客户端 ----------------

pub const DHCP_CLIENT_PORT: u16 = 68;
const DHCP_SERVER_PORT: u16 = 67;

static mut DHCP_XID: u32 = 0;
static mut DHCP_MSG: [u8; 700] = [0; 700];
static mut DHCP_LEN: usize = 0;
static mut DHCP_SERVER: [u8; 4] = [0; 4];
static mut DHCP_OFFERED: [u8; 4] = [0; 4];

fn dhcp_send(kind: u8, req_ip: [u8; 4], server: [u8; 4]) -> bool {
    let mut p = [0u8; 320];
    p[0] = 1; // BOOTREQUEST
    p[1] = 1; // ethernet
    p[2] = 6; // hlen
    p[3] = 0;
    let xid = unsafe { DHCP_XID };
    p[4..8].copy_from_slice(&xid.to_be_bytes());
    p[10..12].copy_from_slice(&0x8000u16.to_be_bytes()); // 让服务器回广播
    let m = mac();
    for i in 0..6 {
        p[28 + i] = m[i];
    }
    p[236..240].copy_from_slice(&[0x63, 0x82, 0x53, 0x63]);
    let mut k = 240usize;
    p[k] = 53;
    p[k + 1] = 1;
    p[k + 2] = kind;
    k += 3;
    if kind == 3 {
        p[k] = 50;
        p[k + 1] = 4;
        p[k + 2..k + 6].copy_from_slice(&req_ip);
        k += 6;
        p[k] = 54;
        p[k + 1] = 4;
        p[k + 2..k + 6].copy_from_slice(&server);
        k += 6;
    }
    p[k] = 12;
    p[k + 1] = 4;
    p[k + 2..k + 6].copy_from_slice(b"astr");
    k += 6;
    p[k] = 55;
    p[k + 1] = 4;
    p[k + 2..k + 6].copy_from_slice(&[1, 3, 6, 51]);
    k += 6;
    p[k] = 255;
    let n = k + 1;
    send_udp(
        [0xff; 6],
        [0, 0, 0, 0],
        [255, 255, 255, 255],
        DHCP_CLIENT_PORT,
        DHCP_SERVER_PORT,
        &p[..n],
    )
}

fn dhcp_on_reply(b: &[u8]) {
    if b.len() < 240 || b[0] != 2 {
        return;
    }
    unsafe {
        if u32::from_be_bytes([b[4], b[5], b[6], b[7]]) != DHCP_XID {
            return;
        }
        let n = b.len().min(700);
        DHCP_MSG[..n].copy_from_slice(&b[..n]);
        DHCP_LEN = n;
    }
}

/// 等一帧 DHCP 回复（最多 ms 毫秒），返回 (msg_type, yiaddr, server, gw, dns)
fn dhcp_wait(ms: u64) -> Option<(u8, [u8; 4], [u8; 4], [u8; 4], [u8; 4])> {
    let t0 = crate::timer::now_ms();
    loop {
        poll();
        let got = unsafe {
            let l = DHCP_LEN;
            if l >= 240 {
                DHCP_LEN = 0;
                let b = &DHCP_MSG[..l];
                let yi = [b[16], b[17], b[18], b[19]];
                let mut kind = 0u8;
                let mut gw = [0u8; 4];
                let mut dns = [0u8; 4];
                let mut server = [0u8; 4];
                let mut i = 240usize;
                while i + 1 < l {
                    let opt = b[i];
                    if opt == 255 {
                        break;
                    }
                    if opt == 0 {
                        i += 1;
                        continue;
                    }
                    let len = b[i + 1] as usize;
                    let v = &b[i + 2..(i + 2 + len).min(l)];
                    match opt {
                        53 if !v.is_empty() => kind = v[0],
                        3 if len >= 4 => gw = [v[0], v[1], v[2], v[3]],
                        6 if len >= 4 => dns = [v[0], v[1], v[2], v[3]],
                        54 if len >= 4 => server = [v[0], v[1], v[2], v[3]],
                        _ => {}
                    }
                    i += 2 + len;
                }
                Some((kind, yi, server, gw, dns))
            } else {
                None
            }
        };
        if let Some(x) = got {
            return Some(x);
        }
        if crate::timer::now_ms().saturating_sub(t0) > ms {
            return None;
        }
    }
}

/// 跑一遍完整的 DISCOVER → OFFER → REQUEST → ACK
pub fn dhcp_acquire(timeout_ms: u64) -> bool {
    if !available() {
        return false;
    }
    unsafe { DHCP_XID = 0x4153_0000 | (crate::timer::now_ms() as u32 & 0xffff) };
    // 先让 slirp 知道我们在（它只对收到的广播做回应）
    send_arp_request(ip());
    dhcp_send(1, [0; 4], [0; 4]);
    let (kind, yi, srv, _, _) = match dhcp_wait(timeout_ms) {
        Some(x) => x,
        None => return false,
    };
    if kind != 2 || yi == [0; 4] {
        return false;
    }
    unsafe {
        DHCP_OFFERED = yi;
        DHCP_SERVER = srv;
    }
    dhcp_send(3, yi, srv);
    let (kind2, yi2, _, gw2, dns2) = match dhcp_wait(timeout_ms) {
        Some(x) => x,
        None => return false,
    };
    if kind2 != 5 {
        return false;
    }
    unsafe {
        OUR_IP = if yi2 == [0; 4] { yi } else { yi2 };
        if gw2 != [0; 4] {
            OUR_GW = gw2;
        }
        if dns2 != [0; 4] {
            OUR_DNS = dns2;
        }
        DHCP_OK = true;
    }
    // 新地址会让之前的 ARP 缓存失效
    unsafe {
        GW_KNOWN = false;
        ARP_MAC = [0; 6];
    }
    let _ = gw();
    true
}

// ---------------- DNS 客户端（只查 A 记录） ----------------

pub const DNS_LOCAL_PORT: u16 = 53053;
static mut DNS_ID: u16 = 0;
static mut DNS_MSG: [u8; 700] = [0; 700];
static mut DNS_LEN: usize = 0;

fn dns_on_reply(b: &[u8], _src: [u8; 4]) {
    if b.len() < 12 {
        return;
    }
    let n = b.len().min(700);
    unsafe {
        DNS_MSG[..n].copy_from_slice(&b[..n]);
        DNS_LEN = n;
    }
}

fn skip_name(b: &[u8], mut i: usize) -> usize {
    while i < b.len() {
        let l = b[i];
        if l == 0 {
            return i + 1;
        }
        if l & 0xc0 == 0xc0 {
            return i + 2;
        }
        i += 1 + l as usize;
    }
    i
}

/// 把 a.b.c 这样的域名编码成 DNS 的 label 序列，返回新的偏移
fn put_qname(b: &mut [u8], mut k: usize, name: &[u8]) -> usize {
    let mut start = 0usize;
    let mut i = 0usize;
    while i <= name.len() {
        if i == name.len() || name[i] == b'.' {
            let len = i - start;
            if len == 0 || k + 1 + len >= b.len() {
                return k;
            }
            b[k] = len as u8;
            k += 1;
            b[k..k + len].copy_from_slice(&name[start..i]);
            k += len;
            start = i + 1;
        }
        i += 1;
    }
    b[k] = 0;
    k + 1
}

pub fn parse_ip(s: &str) -> Option<[u8; 4]> {
    let mut ip = [0u8; 4];
    let mut part = 0usize;
    let mut num = 0u32;
    let mut digits = 0usize;
    for c in s.bytes() {
        if c == b'.' {
            if part >= 3 {
                return None;
            }
            ip[part] = num.min(255) as u8;
            part += 1;
            num = 0;
            digits = 0;
        } else if c.is_ascii_digit() {
            num = num * 10 + (c - b'0') as u32;
            digits += 1;
        } else {
            return None;
        }
    }
    if part != 3 || digits == 0 {
        return None;
    }
    ip[3] = num.min(255) as u8;
    Some(ip)
}

pub fn resolve(name: &str, timeout_ms: u64) -> Option<[u8; 4]> {
    if !available() || name.is_empty() {
        return None;
    }
    // 已经是点分 IP 就直接用
    if let Some(ip) = parse_ip(name) {
        return Some(ip);
    }
    let dns_ip = dns();
    let dns_mac = match route_mac(dns_ip, 600) {
        Some(m) => m,
        None => return None,
    };
    let id = (crate::timer::now_ms() as u16) ^ 0x5a5a;
    unsafe { DNS_ID = id };
    let mut q = [0u8; 300];
    q[0..2].copy_from_slice(&id.to_be_bytes());
    q[2..4].copy_from_slice(&0x0100u16.to_be_bytes()); // 递归查询
    q[4..6].copy_from_slice(&1u16.to_be_bytes());
    let k = put_qname(&mut q, 12, name.as_bytes());
    if k + 4 > q.len() {
        return None;
    }
    q[k..k + 2].copy_from_slice(&1u16.to_be_bytes()); // A
    q[k + 2..k + 4].copy_from_slice(&1u16.to_be_bytes()); // IN
    let n = k + 4;
    if !send_udp(dns_mac, ip(), dns_ip, DNS_LOCAL_PORT, 53, &q[..n]) {
        return None;
    }
    let t0 = crate::timer::now_ms();
    loop {
        poll();
        let got = unsafe {
            let l = DNS_LEN;
            if l >= 12 {
                DNS_LEN = 0;
                let b = &DNS_MSG[..l];
                if u16::from_be_bytes([b[0], b[1]]) == DNS_ID {
                    let an = u16::from_be_bytes([b[6], b[7]]) as usize;
                    let qd = u16::from_be_bytes([b[4], b[5]]) as usize;
                    let mut i = 12usize;
                    for _ in 0..qd {
                        i = skip_name(b, i) + 4;
                    }
                    let mut found = None;
                    for _ in 0..an.min(8) {
                        i = skip_name(b, i);
                        if i + 10 > b.len() {
                            break;
                        }
                        let typ = u16::from_be_bytes([b[i], b[i + 1]]);
                        let rdlen = u16::from_be_bytes([b[i + 8], b[i + 9]]) as usize;
                        let rd = i + 10;
                        if typ == 1 && rdlen == 4 && rd + 4 <= b.len() {
                            found = Some([b[rd], b[rd + 1], b[rd + 2], b[rd + 3]]);
                            break;
                        }
                        i = rd + rdlen;
                    }
                    found
                } else {
                    None
                }
            } else {
                None
            }
        };
        if let Some(x) = got {
            return Some(x);
        }
        if crate::timer::now_ms().saturating_sub(t0) > timeout_ms {
            return None;
        }
    }
}
