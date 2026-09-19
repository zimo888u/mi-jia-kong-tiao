//! probe-miio —— miIO 握手探测（对照参考实现逐字节验证）
//!
//! 局域网一直收不到响应时，把实际发出的字节打出来，再和能跑通的参考
//! 实现（v1 用的 node-mihome）对比，就能立刻定位差异。
//!
//! 实测结论（本项目踩过的坑）：
//!   参考实现**第一次不发明文 JSON**，而是发一个 32 字节的「空 hello」：
//!     魔术字 0x2131 | 长度 0x0020 | 0 | 设备ID = 0xFFFFFFFF |
//!     时间戳 = 0xFFFFFFFF | 校验和 = 16 个 0xFF
//!   设备回一个头（含自己的真实设备 ID 与时间戳），**从回包头里**取
//!   设备 ID，之后才用 token 派生的密钥加密真正的 JSON 报文。
//!
//!   我最初直接发了加密的 `miIO.info`，设备完全不响应——和网络无关。

use miac_core::crypto;
use std::net::UdpSocket;
use std::time::Duration;

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect::<Vec<_>>().join("")
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let ip = args.first().cloned().unwrap_or_else(|| "192.0.2.10".into());
    let token_hex = args.get(1).cloned().unwrap_or_else(|| "0".repeat(32));

    let token = miac_core::miio::parse_token(&token_hex).expect("token 解析失败");

    let socket = UdpSocket::bind(("0.0.0.0", 0)).expect("绑定失败");
    socket
        .set_read_timeout(Some(Duration::from_secs(3)))
        .expect("设置超时失败");
    socket.connect((ip.as_str(), 54321)).expect("connect 失败");

    // ── 第 1 步：空 hello（设备不响应加密包，只认这个 32 字节头）──
    let mut hello = Vec::with_capacity(32);
    hello.extend_from_slice(&0x2131u16.to_be_bytes());
    hello.extend_from_slice(&0x0020u16.to_be_bytes()); // 长度 = 32，无载荷
    hello.extend_from_slice(&0u32.to_be_bytes());
    hello.extend_from_slice(&0xffff_ffffu32.to_be_bytes()); // 设备 ID：未知
    hello.extend_from_slice(&0xffff_ffffu32.to_be_bytes()); // 时间戳：未知
    hello.extend_from_slice(&[0xffu8; 16]); // 校验和：全 FF

    println!("hello(32B) = {}", hex(&hello));
    socket.send(&hello).expect("发送失败");

    let mut buf = [0u8; 2048];
    let n = match socket.recv(&mut buf) {
        Ok(n) => n,
        Err(e) => {
            println!("空 hello 也没有响应：{e}");
            return;
        }
    };
    let reply = &buf[..n];
    println!("收到 {n} 字节：");
    println!("  头 16B   = {}", hex(&reply[..16.min(n)]));
    if n >= 32 {
        let dev_id = u32::from_be_bytes([reply[8], reply[9], reply[10], reply[11]]);
        let stamp = u32::from_be_bytes([reply[12], reply[13], reply[14], reply[15]]);
        println!("  设备 ID  = {dev_id}");
        println!("  时间戳   = {stamp} (0x{stamp:08x})");
        println!("  校验和   = {}", hex(&reply[16..32]));
    }

    // ── 第 2 步：用 token 派生的密钥发真正的 JSON 报文 ──
    let key = crypto::md5(&token);
    let mut ivbuf = key.to_vec();
    ivbuf.extend_from_slice(&token);
    let iv = crypto::md5(&ivbuf);

    let dev_id = if n >= 16 {
        u32::from_be_bytes([reply[8], reply[9], reply[10], reply[11]])
    } else {
        0
    };
    let stamp = if n >= 16 {
        u32::from_be_bytes([reply[12], reply[13], reply[14], reply[15]])
    } else {
        0
    };

    let payload = br#"{"id":2,"method":"get_properties","params":[{"did":"1234567890","siid":2,"piid":1}]}"#;
    let enc = crypto::aes_cbc_encrypt(&key, &iv, payload);

    let mut h = Vec::new();
    h.extend_from_slice(&0x2131u16.to_be_bytes());
    h.extend_from_slice(&((32 + enc.len()) as u16).to_be_bytes());
    h.extend_from_slice(&0u32.to_be_bytes());
    h.extend_from_slice(&dev_id.to_be_bytes());
    h.extend_from_slice(&stamp.to_be_bytes());

    let mut csin = h.clone();
    csin.extend_from_slice(&token);
    csin.extend_from_slice(&enc);
    let cksum = crypto::md5(&csin);

    let mut packet = h.clone();
    packet.extend_from_slice(&cksum);
    packet.extend_from_slice(&enc);

    println!();
    println!("请求包({} 字节) 头16B={}", packet.len(), hex(&h));
    socket.send(&packet).expect("发送失败");

    match socket.recv(&mut buf) {
        Ok(n2) => {
            let r = &buf[..n2];
            let plain = crypto::aes_cbc_decrypt(&key, &iv, &r[32.min(n2)..]);
            println!("收到 {n2} 字节，解密后：");
            println!("  {}", String::from_utf8_lossy(&plain));
        }
        Err(e) => println!("第二次请求无响应：{e}"),
    }
}
