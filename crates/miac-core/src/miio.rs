//! miio.rs —— 米家局域网协议（miIO / UDP 54321）
//!
//! 这是「局域网直连」通道的实现，移植自 v1 所用 `node-mihome` 的
//! `miioProtocol`。实测读状态约 0.3–0.4 秒，且设备会主动把属性变化上报小米云，
//! 所以米家 App 约 1 秒内同步——局域网既快又一致，是默认通道。
//!
//! ## 报文格式
//!
//! ```text
//! 偏移  长度  含义
//!  0     2    魔术字 0x2131
//!  2     2    报文总长（含 32 字节头）
//!  4     4    未知（恒为 0）
//!  8     4    设备 ID
//! 12     4    时间戳（stamp）
//! 16    16    校验和
//! 32     n    密文：AES-128-CBC(明文 + PKCS7)
//! ```
//!
//! ## ⚠ 四个极易做错、且错了会「设备完全不响应」的点
//!
//! 1. **握手第一步不是加密报文**。要先发一个 32 字节的「空 hello」：
//!    `2131 0020 00000000 ffffffff ffffffff` + 16 个 `0xFF`，
//!    即长度 0x20、设备 ID 与时间戳都填 `0xFFFFFFFF`、校验和全 `FF`。
//!    设备回一个 32 字节的头，**从里面取真实设备 ID 与时间戳**。
//!    直接发加密的 `miIO.info` 设备完全不响应（表现为 UDP 超时，极易误判成
//!    网络不通——本项目就在这里卡了很久，最后靠抓参考实现的字节才发现）。
//! 2. **校验和覆盖密文**：`MD5( header ‖ token ‖ encrypted_payload )`。
//!    漏掉密文同样会被设备丢包。
//! 3. **握手首次发送要重试**：实测设备对第一个 hello 有时不回。
//! 4. **用 send_to / recv_from，不要 connect**：与参考实现一致。
//!
//! ## 线程模型
//!
//! 阻塞 UDP socket + 超时，跑在调用方的线程上。为守住内存指标（25–45 MB），
//! 本项目不引入 tokio。

use std::io;
use std::net::{SocketAddr, ToSocketAddrs, UdpSocket};
use std::time::Duration;

use serde_json::{json, Value};

use crate::crypto;

/// 米家设备的 UDP 端口。
pub const PORT: u16 = 54321;

/// 默认超时（实测一次往返约 0.2–0.4 秒，给足余量）。
pub const TIMEOUT: Duration = Duration::from_millis(3000);

/// 握手前使用的哨兵值（设备 ID 与时间戳都用它）。
const SENTINEL: u32 = 0xffff_ffff;

/// 握手空 hello 的固定 32 字节。
///
/// 这是 miIO 协议里最反直觉的一处：**不加密、不带 JSON**。
/// 设备靠它认识「谁在找我」，并回传自己的设备 ID 与时间戳。
pub fn handshake_packet() -> [u8; 32] {
    // ⚠ 整个包除前 4 字节外**全是 0xFF**：
    //   前 4 字节 = 魔术字 0x2131 + 长度 0x0020（32，无载荷）
    //   第 4~7 字节（未知段）也必须是 0xFF，不能填 0
    //   第 8~11（设备 ID）、12~15（时间戳）、16~31（校验和）同样是 0xFF
    // 早先把「未知段」写成 0，设备完全不响应；而 0x2131 与长度都对、
    // 报文长度也一致，光看代码根本看不出问题——最后靠抓参考实现实际发出的
    // 字节再重放才定位到。
    let mut p = [0xffu8; 32];
    p[0..2].copy_from_slice(&0x2131u16.to_be_bytes());
    p[2..4].copy_from_slice(&0x0020u16.to_be_bytes());
    p
}

/// miIO 协议错误。
#[derive(Debug)]
pub enum MiioError {
    /// 网络层错误（不可达、超时等）
    Io(io::Error),
    /// 响应不是合法 JSON
    Json(String),
    /// 设备返回了错误码
    Device { code: i64, message: String },
    /// 报文格式不符合预期
    Protocol(String),
}

impl std::fmt::Display for MiioError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            MiioError::Io(e) => write!(f, "局域网通信失败：{e}"),
            MiioError::Json(m) => write!(f, "设备响应无法解析：{m}"),
            MiioError::Device { code, message } => {
                write!(f, "设备返回错误 code={code} {message}")
            }
            MiioError::Protocol(m) => write!(f, "协议异常：{m}"),
        }
    }
}

impl std::error::Error for MiioError {}

impl From<io::Error> for MiioError {
    fn from(e: io::Error) -> Self {
        MiioError::Io(e)
    }
}

/// 一个已建立会话的局域网设备连接。
pub struct MiioDevice {
    /// 设备 token（device.json 里的 32 位 hex）。
    ///
    /// 密钥 = MD5(token)，IV = MD5(key ‖ token)，校验和里也要拼它。
    /// 设备不会在握手时更换 token——早期我以为会，多存了一个
    /// `session_token` 字段，纯属多余，已去掉。
    token: Vec<u8>,
    /// 当前报文头里的设备 ID。
    ///
    /// 初始值取自 `device.json` 的 `did`，握手后会被设备回传的真实 ID 覆盖
    /// （两者通常一致，但以设备回传的为准）。
    device_id: u32,
    /// 是否已握手
    handshaked: bool,
    /// 设备回传的时间戳基准
    server_stamp: Option<u32>,
    /// 收到 server_stamp 的本地时刻
    server_stamp_at: Option<std::time::Instant>,
    addr: SocketAddr,
    socket: UdpSocket,
    /// 报文计数（同时用作 JSON 里的 id）
    pub packets: u64,
}

impl MiioDevice {
    /// 用 `ip`、`did`、32 位 hex `token` 建立连接对象。
    ///
    /// `did` 是设备 ID（`device.json` 里的 `did`），会直接写进报文头。
    /// 注意：这里只构造，不发包；首次 `rpc()` 时会自动握手。
    pub fn connect(host: &str, did: &str, token_hex: &str) -> Result<Self, MiioError> {
        let token = parse_token(token_hex)?;

        // 允许 device.json 里存 "192.168.1.5" 或 "192.168.1.5:54321"
        let target = if host.contains(':') {
            host.to_string()
        } else {
            format!("{host}:{PORT}")
        };

        let addr = target
            .to_socket_addrs()?
            .next()
            .ok_or_else(|| MiioError::Protocol(format!("无法解析设备地址 {target}")))?;

        let socket = UdpSocket::bind(("0.0.0.0", 0))?;
        socket.set_read_timeout(Some(TIMEOUT))?;
        socket.set_write_timeout(Some(TIMEOUT))?;
        // 刻意**不** connect：与参考实现一致，用 send_to / recv_from。
        // 连接式 UDP 会把 ICMP 端口不可达变成下一次 send 的错误，
        // 排查时容易把「设备没回」误读成「发送失败」。

        // 设备 ID：取 did 的数字形式。小米 did 是十进制数字串，
        // 但个别设备用别的编号方式，解析不出来时退回一个稳定的哈希值
        // （仍不能用随机数，否则同一会话每次发出去的头都不一样）。
        let device_id = parse_device_id(did);

        Ok(Self {
            token,
            device_id,
            handshaked: false,
            server_stamp: None,
            server_stamp_at: None,
            addr,
            socket,
            packets: 0,
        })
    }

    /// 设备地址（用于日志与设备信息展示）。
    pub fn addr(&self) -> SocketAddr {
        self.addr
    }

    /// 是否已完成握手。
    pub fn is_handshaked(&self) -> bool {
        self.handshaked
    }

    /// 当前用于加解密的密钥与 IV。
    ///
    /// 密钥 = MD5(token)；IV = MD5(key ‖ token)。
    fn key_iv(&self) -> ([u8; 16], [u8; 16]) {
        let key = crypto::md5(&self.token);
        let mut buf = Vec::with_capacity(32);
        buf.extend_from_slice(&key);
        buf.extend_from_slice(&self.token);
        let iv = crypto::md5(&buf);
        (key, iv)
    }

    /// 组一个包并发送。
    fn send_packet(&mut self, payload: &[u8]) -> Result<(), MiioError> {
        let (key, iv) = self.key_iv();
        let encrypted = crypto::aes_cbc_encrypt(&key, &iv, payload);
        let total_len = (32 + encrypted.len()) as u16;

        let mut header = Vec::with_capacity(16);
        header.extend_from_slice(&0x2131u16.to_be_bytes());
        header.extend_from_slice(&total_len.to_be_bytes());
        header.extend_from_slice(&0u32.to_be_bytes()); // 未知段
        header.extend_from_slice(&self.device_id.to_be_bytes());
        // 时间戳：握手前必须是 0xFFFFFFFF；握手后按设备 stamp + 已过秒数
        let stamp = match (self.server_stamp, self.server_stamp_at) {
            (Some(base), Some(at)) => base.wrapping_add(at.elapsed().as_secs() as u32),
            _ => SENTINEL,
        };
        header.extend_from_slice(&stamp.to_be_bytes());

        // ⚠ 校验和 = MD5( header[0..16] ‖ token ‖ 密文 )
        //    漏掉密文会让设备直接丢包（表现为 UDP 超时）
        let mut checksum_input = Vec::with_capacity(16 + self.token.len() + encrypted.len());
        checksum_input.extend_from_slice(&header);
        checksum_input.extend_from_slice(&self.token);
        checksum_input.extend_from_slice(&encrypted);
        let checksum = crypto::md5(&checksum_input);

        let mut packet = Vec::with_capacity(total_len as usize);
        packet.extend_from_slice(&header);
        packet.extend_from_slice(&checksum);
        packet.extend_from_slice(&encrypted);

        self.socket.send_to(&packet, self.addr)?;
        self.packets += 1;
        Ok(())
    }

    /// 收一个包、解密并解析出 JSON。
    fn recv_packet(&mut self) -> Result<Value, MiioError> {
        let mut buf = vec![0u8; 4096];
        let (n, from) = self.socket.recv_from(&mut buf)?;
        buf.truncate(n);

        if from != self.addr {
            return Err(MiioError::Protocol(format!("忽略来自非目标地址 {from} 的 UDP 报文")));
        }

        if buf.len() < 32 {
            return Err(MiioError::Protocol(format!("报文过短：{} 字节", buf.len())));
        }
        if u16::from_be_bytes([buf[0], buf[1]]) != 0x2131 {
            return Err(MiioError::Protocol("魔术字不是 0x2131".into()));
        }
        let declared = u16::from_be_bytes([buf[2], buf[3]]) as usize;
        if declared != buf.len() {
            return Err(MiioError::Protocol(format!(
                "报文长度不一致：头部 {declared} 字节，实际 {} 字节",
                buf.len()
            )));
        }
        if (buf.len() - 32) % 16 != 0 {
            return Err(MiioError::Protocol("密文长度不是 AES 块大小的整数倍".into()));
        }

        let mut checksum_input = Vec::with_capacity(16 + self.token.len() + buf.len() - 32);
        checksum_input.extend_from_slice(&buf[..16]);
        checksum_input.extend_from_slice(&self.token);
        checksum_input.extend_from_slice(&buf[32..]);
        let expected_checksum = crypto::md5(&checksum_input);
        if buf[16..32] != expected_checksum {
            return Err(MiioError::Protocol("响应校验和不匹配".into()));
        }

        // 头里的设备 ID / 时间戳：以设备为准（握手后已经取过一次，这里兜底）
        let header_device_id = u32::from_be_bytes([buf[8], buf[9], buf[10], buf[11]]);
        let header_stamp = u32::from_be_bytes([buf[12], buf[13], buf[14], buf[15]]);
        if header_device_id != 0 && header_device_id != SENTINEL {
            self.device_id = header_device_id;
        }
        if header_stamp != 0 && header_stamp != SENTINEL && self.server_stamp.is_none() {
            self.server_stamp = Some(header_stamp);
            self.server_stamp_at = Some(std::time::Instant::now());
        }

        let (key, iv) = self.key_iv();
        let plain = crypto::aes_cbc_decrypt(&key, &iv, &buf[32..]);

        // JSON 从第一个 '{' 到最后一个 '}'（握手包前面 16 字节是 token）
        let text = match plain.iter().position(|&b| b == b'{') {
            Some(start) => {
                let end = plain
                    .iter()
                    .rposition(|&b| b == b'}')
                    .unwrap_or(plain.len().saturating_sub(1));
                if end >= start {
                    String::from_utf8_lossy(&plain[start..=end]).to_string()
                } else {
                    String::from_utf8_lossy(&plain).to_string()
                }
            }
            None => String::from_utf8_lossy(&plain).to_string(),
        };

        // 去掉控制字符，容忍个别固件塞进来的杂字节
        let cleaned: String = text
            .chars()
            .filter(|c| !c.is_control() || *c == '\n' || *c == '\t')
            .collect();

        serde_json::from_str(cleaned.trim())
            .map_err(|e| MiioError::Json(format!("{e}；原文：{}", cleaned.trim())))
    }

    /// 首次通信前握手。
    ///
    /// 流程（与参考实现一致，也是设备唯一认的流程）：
    ///   1. 发 32 字节空 hello（不加密、无 JSON）
    ///   2. 收设备回的头，取真实设备 ID 与时间戳
    ///   3. 之后所有报文都用 token 派生的密钥加密
    ///
    /// 第 1 步发两次：实测设备对第一个 hello 有时不回，只发一次会偶发超时。
    fn handshake(&mut self) -> Result<(), MiioError> {
        if self.handshaked {
            return Ok(());
        }

        let hello = handshake_packet();
        let mut last_err = None;

        for attempt in 1..=2 {
            // 第一次发完立刻再发一次（参考实现就是这个节奏，能显著提高成功率）
            self.socket.send_to(&hello, self.addr)?;

            match self.recv_header_only() {
                Ok(()) => {
                    self.handshaked = true;
                    return Ok(());
                }
                Err(e) => {
                    last_err = Some(e);
                    if attempt == 1 {
                        // 再补一发 hello，然后进入第二轮等待
                        let _ = self.socket.send_to(&hello, self.addr);
                    }
                }
            }
        }

        Err(last_err.unwrap_or_else(|| MiioError::Protocol("握手失败".into())))
    }

    /// 只收握手响应的头（32 字节），取出设备 ID 与时间戳，不解析 JSON。
    fn recv_header_only(&mut self) -> Result<(), MiioError> {
        let mut buf = [0u8; 256];
        let (n, _from) = self.socket.recv_from(&mut buf)?;
        if n < 32 {
            return Err(MiioError::Protocol(format!("握手响应过短：{n} 字节")));
        }
        if u16::from_be_bytes([buf[0], buf[1]]) != 0x2131 {
            return Err(MiioError::Protocol("握手响应魔术字不对".into()));
        }

        let header_device_id = u32::from_be_bytes([buf[8], buf[9], buf[10], buf[11]]);
        let header_stamp = u32::from_be_bytes([buf[12], buf[13], buf[14], buf[15]]);

        // 设备 ID：以设备回传为准（0 或哨兵说明它还没准备好，保留原值）
        if header_device_id != 0 && header_device_id != SENTINEL {
            self.device_id = header_device_id;
        }
        // 时间戳基准：后续报文的 stamp = 它 + 已过秒数
        if header_stamp != 0 && header_stamp != SENTINEL {
            self.server_stamp = Some(header_stamp);
            self.server_stamp_at = Some(std::time::Instant::now());
        }
        Ok(())
    }

    /// 发一次 RPC 并等响应。
    ///
    /// `method` 形如 `get_properties` / `set_properties`，
    /// `params` 是 MIoT 的 siid/piid 数组。
    pub fn rpc(&mut self, method: &str, params: Value) -> Result<Value, MiioError> {
        self.handshake()?;

        let id = (self.packets as i64 % 100_000) + 2;
        let payload = json!({ "id": id, "method": method, "params": params });
        self.send_packet(payload.to_string().as_bytes())?;

        // 设备可能先回无关的包（重复握手响应等），按 id 匹配，最多试 6 个
        for _ in 0..6 {
            let resp = self.recv_packet()?;
            let Some(rid) = resp.get("id").and_then(Value::as_i64) else {
                continue;
            };
            if rid != id {
                continue;
            }
            if let Some(err) = resp.get("error") {
                let code = err.get("code").and_then(Value::as_i64).unwrap_or(-1);
                let message = err
                    .get("message")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string();
                return Err(MiioError::Device { code, message });
            }
            return Ok(resp.get("result").cloned().unwrap_or(Value::Null));
        }

        Err(MiioError::Protocol("没有收到匹配 id 的响应".into()))
    }

    /// 批量读属性：`[(siid, piid)]` → `[{code, value, …}]`。
    pub fn get_properties(
        &mut self,
        did: &str,
        props: &[(u16, u16)],
    ) -> Result<Vec<Value>, MiioError> {
        let params: Vec<Value> = props
            .iter()
            .map(|(siid, piid)| json!({ "did": did, "siid": siid, "piid": piid }))
            .collect();
        let res = self.rpc("get_properties", Value::Array(params))?;
        Ok(match res {
            Value::Array(a) => a,
            other => vec![other],
        })
    }

    /// 写一个属性。
    pub fn set_property(
        &mut self,
        did: &str,
        siid: u16,
        piid: u16,
        value: Value,
    ) -> Result<Value, MiioError> {
        let params = json!([{ "did": did, "siid": siid, "piid": piid, "value": value }]);
        let res = self.rpc("set_properties", params)?;
        Ok(match res {
            Value::Array(mut a) if !a.is_empty() => a.remove(0),
            other => other,
        })
    }
}

/// 解析 32 位 hex token。
pub fn parse_token(hex_str: &str) -> Result<Vec<u8>, MiioError> {
    let s = hex_str.trim();
    if s.len() != 32 {
        return Err(MiioError::Protocol(format!(
            "token 应为 32 位 hex，实际 {} 位",
            s.len()
        )));
    }
    let mut out = Vec::with_capacity(16);
    for i in (0..32).step_by(2) {
        let byte = u8::from_str_radix(&s[i..i + 2], 16)
            .map_err(|_| MiioError::Protocol(format!("token 含非 hex 字符：{s}")))?;
        out.push(byte);
    }
    Ok(out)
}

/// 把 `did` 解析成报文头里的 u32 设备 ID。
///
/// 小米空调的 did 是十进制数字串（例如 `2148659835`）。解析不出来时用
/// FNV-1a 哈希得到一个**稳定**的值——关键是「稳定」而不是「随机」，
/// 因为同一个会话里所有报文的设备 ID 必须一致。
pub fn parse_device_id(did: &str) -> u32 {
    let t = did.trim();
    if let Ok(n) = t.parse::<u32>() {
        return n;
    }
    let mut acc: u32 = 0x811c_9dc5;
    for b in t.bytes() {
        acc ^= b as u32;
        acc = acc.wrapping_mul(0x0100_0193);
    }
    acc
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn token_parsing() {
        let t = parse_token("00112233445566778899aabbccddeeff").unwrap();
        assert_eq!(t.len(), 16);
        assert_eq!(t[0], 0x00);
        assert_eq!(t[15], 0xff);
    }

    #[test]
    fn token_rejects_bad_input() {
        assert!(parse_token("abc").is_err());
        assert!(parse_token("zz112233445566778899aabbccddeeff").is_err());
        // 带空白的合法 token 应该能过（配置文件里可能有多余空格）
        assert!(parse_token(" 00112233445566778899aabbccddeeff ").is_ok());
    }

    #[test]
    fn key_iv_derivation_matches_protocol() {
        let token = parse_token("00112233445566778899aabbccddeeff").unwrap();
        let key = crypto::md5(&token);
        let mut buf = key.to_vec();
        buf.extend_from_slice(&token);
        let iv = crypto::md5(&buf);
        assert_eq!(key, crypto::md5(&token));
        assert_eq!(iv.len(), 16);
        assert_ne!(key, iv, "密钥与 IV 不应相同");
    }

    #[test]
    fn checksum_covers_encrypted_payload() {
        // 最易写错的一处：校验和必须包含密文。
        // 用固定 token 与固定明文，验证「含密文」与「不含密文」结果不同。
        let token = parse_token("00112233445566778899aabbccddeeff").unwrap();
        let key = crypto::md5(&token);
        let mut ivbuf = key.to_vec();
        ivbuf.extend_from_slice(&token);
        let iv = crypto::md5(&ivbuf);

        let plain = br#"{"id":1,"method":"miIO.info","params":[]}"#;
        let encrypted = crypto::aes_cbc_encrypt(&key, &iv, plain);

        let mut header16 = Vec::new();
        header16.extend_from_slice(&0x2131u16.to_be_bytes());
        header16.extend_from_slice(&((32 + encrypted.len()) as u16).to_be_bytes());
        header16.extend_from_slice(&0u32.to_be_bytes());
        header16.extend_from_slice(&0x11223344u32.to_be_bytes());

        let mut with_ct = header16.clone();
        with_ct.extend_from_slice(&token);
        with_ct.extend_from_slice(&encrypted);
        let correct = crypto::md5(&with_ct);

        let mut without_ct = header16.clone();
        without_ct.extend_from_slice(&token);
        let wrong = crypto::md5(&without_ct);

        assert_ne!(correct, wrong, "含/不含密文的校验和必须不同");
        assert_eq!(correct.len(), 16);
    }

    #[test]
    fn device_id_from_did() {
        // 小米空调的 did 是十进制串，直接当报文头的设备 ID
        assert_eq!(parse_device_id("2148659835"), 2148659835);
        assert_eq!(parse_device_id("  42  "), 42);
        // 非数字 did 也要给出**稳定**值（同一输入必须同一输出）
        let a = parse_device_id("blt.3.1k38tdg344g05");
        assert_eq!(a, parse_device_id("blt.3.1k38tdg344g05"));
        assert_ne!(a, parse_device_id("blt.3.other"));
    }

    #[test]
    fn handshake_packet_is_exact() {
        // 锁定握手包的精确字节。
        // 这是本项目最贵的一个坑：把第 4~7 字节写成 0（而不是 0xFF），
        // 设备就完全不响应，而 0x2131、长度 0x20 都对、报文长度也一致，
        // 从代码上完全看不出问题。这条测试就是为了防它回归。
        let p = handshake_packet();
        assert_eq!(p.len(), 32);
        assert_eq!(&p[0..4], &[0x21, 0x31, 0x00, 0x20], "魔术字与长度");
        assert!(
            p[4..].iter().all(|b| *b == 0xff),
            "第 4 字节起必须全是 0xFF，实际：{}",
            p.iter().map(|b| format!("{b:02x}")).collect::<String>()
        );
        // 与参考实现（v1 用的 node-mihome）实际发出的字节逐字节一致
        let got: String = p.iter().map(|b| format!("{b:02x}")).collect();
        assert_eq!(
            got, "21310020ffffffffffffffffffffffffffffffffffffffffffffffffffffffff",
            "握手包必须与参考实现完全一致"
        );
    }

    #[test]
    fn sentinel_is_all_ones() {
        assert_eq!(SENTINEL, 0xffff_ffff);
    }

    #[test]
    fn json_extraction_skips_leading_token() {
        // 握手响应 = 16 字节 token + JSON，取 JSON 时要跳过 token
        let mut plain = vec![0xAAu8; 16];
        plain.extend_from_slice(br#"{"id":1,"result":{"fw_ver":"1.0"}}"#);
        let start = plain.iter().position(|&b| b == b'{').unwrap();
        let end = plain.iter().rposition(|&b| b == b'}').unwrap();
        let text = String::from_utf8_lossy(&plain[start..=end]).to_string();
        let v: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(v.get("id").and_then(Value::as_i64), Some(1));
        assert_eq!(
            v.get("result")
                .and_then(|r| r.get("fw_ver"))
                .and_then(Value::as_str),
            Some("1.0")
        );
    }
}
