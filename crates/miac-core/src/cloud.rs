//! cloud.rs —— 小米云 RPC 通道
//!
//! 移植自 v1 所用 `node-mihome` 的 `miCloudProtocol`。用于：
//!   - 设备不在同一局域网时的属性读写（`/home/rpc/{did}`）
//!   - 电量统计（`/v2/user/statistics`）
//!   - 温湿度计读数（`/miotspec/prop/get` 与 `/user/get_user_device_data`）
//!
//! ## 鉴权算法（最容易做错的一块，逐条对照参考实现）
//!
//! 1. **nonce**：12 字节 = 前 8 字节随机 + 后 4 字节「当前分钟数」的大端 i32，
//!    整体 base64。带分钟数是为了防重放。
//! 2. **signedNonce** = base64( SHA256( base64decode(ssecurity) ‖ nonce_raw ) )
//! 3. **signature** = base64( HMAC-SHA256(
//!        key = signedNonce 的**原始字节**,
//!        msg = path & signedNonce_b64 & nonce_b64 & "k=v"… ) )
//!    其中 `k=v` 来自请求参数按 key 排序后拼接。
//! 4. 请求体是 **form-urlencoded**，字段 `_nonce` / `data` / `signature`；
//!    ⚠ 签名不是自定义请求头——早先按 `_s` 请求头实现，服务端直接 401。
//! 5. Cookie 需要带全 sdkVersion / deviceId / userId / serviceToken / locale / channel，
//!    缺项同样可能 401。
//!
//! ## TLS 与代理（本机特殊约束）
//!
//! 本机 Windows schannel 凭据存储不可用（`SEC_E_NO_CREDENTIALS`），走系统 TLS
//! 栈的客户端全都连不上。所以这里用 rustls + 内置根证书（`webpki-roots`），
//! 完全绕开操作系统的证书存储。

use std::collections::HashMap;
use std::io::Read;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use base64::engine::general_purpose::STANDARD;
use base64::Engine;
use hmac::{Hmac, Mac};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use crate::crypto;

type HmacSha256 = Hmac<Sha256>;

/// 云接口对 User-Agent 敏感，照抄参考实现的机型段。
const USERAGENT_MODEL: &str = "ONEPLUS A3010";

pub const SESSION_EXPIRED_MESSAGE: &str = "米家云登录已过期，请在设置中重新扫码登录。";

/// 各国家/地区的云入口主机名。
///
/// 与参考实现一致：中国区**不带**地区前缀，其它地区是 `<country>.`。
pub fn api_host(country: &str) -> String {
    let c = country.trim().to_lowercase();
    if c.is_empty() || c == "cn" || !valid_country_code(&c) {
        "https://api.io.mi.com/app".to_string()
    } else {
        format!("https://{c}.api.io.mi.com/app")
    }
}

fn valid_country_code(country: &str) -> bool {
    let country = country.trim();
    country.is_empty() || (country.len() <= 63
        && !country.starts_with('-') && !country.ends_with('-')
        && country.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-'))
}

fn cookie_value(value: &str) -> bool {
    !value.is_empty() && value.len() <= 16384 && value.bytes().all(|b| {
        matches!(b, 0x21 | 0x23..=0x2b | 0x2d..=0x3a | 0x3c..=0x5b | 0x5d..=0x7e)
    })
}

fn result_array(response: &Value) -> Result<Vec<Value>, CloudError> {
    response.get("result").and_then(Value::as_array).cloned()
        .ok_or_else(|| CloudError::Parse("result 应为数组".into()))
}

/// 云端会话（对应 v1 的 cloud-session.json）。
///
/// 字段名与 JSON 完全一致，保证 v1 → v2 直接复用已有凭据文件。
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize, Default)]
pub struct CloudSession {
    /// 小米账号 ID（数字，存成字符串与 v1 保持一致）
    pub username: String,
    /// 用于签名的主密钥（base64）
    pub ssecurity: crate::secret::SecretString,
    /// 用户 uid
    #[serde(rename = "userId")]
    pub user_id: String,
    /// 服务票据
    #[serde(rename = "serviceToken")]
    pub service_token: crate::secret::SecretString,
    /// 国家/地区代码（可缺省）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub country: Option<String>,
    /// 可选的区域（登录流程里可能带）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub region: Option<String>,
    /// 写入时间（v1 会写，v2 保留以免破坏兼容）
    #[serde(rename = "savedAt", default, skip_serializing_if = "Option::is_none")]
    pub saved_at: Option<String>,
}

impl CloudSession {
    /// 是否具备 RPC 所需的四项（与 v1 `sessionReady()` 判定一致）。
    pub fn ready(&self) -> bool {
        !self.username.trim().is_empty()
            && !self.ssecurity.trim().is_empty()
            && !self.user_id.trim().is_empty()
            && !self.service_token.trim().is_empty()
            && cookie_value(&self.user_id) && cookie_value(&self.service_token)
    }

    pub fn country_code(&self) -> &str {
        self.country
            .as_deref()
            .filter(|c| !c.trim().is_empty())
            .unwrap_or("cn")
    }
}

/// 云端请求错误。
#[derive(Debug)]
pub enum CloudError {
    /// 会话不完整
    NoSession,
    /// 小米的 HTTP 426 也可能表示 SERVICETOKEN_EXPIRED，而不是协议升级。
    SessionExpired,
    /// 传输层失败
    Transport(String),
    /// HTTP 拒绝响应（只保留状态码和业务码，不记录响应里的凭据）。
    Http { status: u16, code: Option<i64>, message: String },
    /// 服务端返回了非 0 的 code
    Api { code: i64, message: String },
    /// 响应无法解析
    Parse(String),
}

impl std::fmt::Display for CloudError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CloudError::NoSession => write!(f, "云端会话不可用，请重新登录米家账号。"),
            CloudError::SessionExpired => write!(f, "{SESSION_EXPIRED_MESSAGE}"),
            CloudError::Transport(m) => write!(f, "云端请求失败：{m}"),
            CloudError::Http { status, code, message } => {
                write!(f, "云端 HTTP {status}")?;
                if let Some(code) = code { write!(f, "（code={code}）")?; }
                if !message.is_empty() { write!(f, " {message}")?; }
                match status {
                    401 | 403 => write!(f, "：登录会话失效或无访问权限，请重新登录米家账号。"),
                    429 => write!(f, "：请求过于频繁，请稍后再试。"),
                    _ => Ok(()),
                }
            }
            CloudError::Api { code, message } => {
                write!(f, "云端返回错误 code={code} {message}")
            }
            CloudError::Parse(m) => write!(f, "云端响应无法解析：{m}"),
        }
    }
}

impl std::error::Error for CloudError {}

impl CloudError {
    pub fn is_session_expired(&self) -> bool {
        matches!(self, Self::SessionExpired)
    }
}

fn response_session_expired(status: u16, value: &Value) -> bool {
    let message = value.get("message").or_else(|| value.get("msg"))
        .and_then(Value::as_str).unwrap_or("").trim();
    message.eq_ignore_ascii_case("SERVICETOKEN_EXPIRED")
        || (status == 401 && message.eq_ignore_ascii_case("auth error"))
}

/// 小米云客户端。
pub struct CloudClient {
    session: CloudSession,
    /// 出口代理（本机必须经代理才能出网）
    proxy: Option<String>,
    /// 固定的客户端设备 ID（登录时生成；这里由 userId 派生，保持稳定）
    client_id: String,
    timeout: Duration,
    agent: ureq::Agent,
    session_expired: AtomicBool,
    config_error: Option<String>,
}

impl CloudClient {
    /// 用会话构造客户端。
    ///
    /// 代理按以下顺序取：显式参数 → `MIAC_PROXY` 环境变量 → 无代理。
    pub fn new(session: CloudSession, proxy: Option<String>) -> Self {
        let proxy = proxy.or_else(|| std::env::var("MIAC_PROXY").ok());
        let timeout = Duration::from_millis(
            std::env::var("MIAC_HTTP_TIMEOUT_MS")
                .ok()
                .and_then(|s| s.parse().ok())
                .unwrap_or(8000),
        );

        // deviceId 必须是稳定值：每次请求都变的话服务端会认为会话异常。
        let client_id = derive_client_id(&session.user_id);

        let mut builder = ureq::AgentBuilder::new().tls_config(crate::tls::connector())
            .redirects(0).timeout(timeout).user_agent(&format!(
            "Android-7.1.1-1.0.0-{USERAGENT_MODEL}-136-{}",
            &client_id[..client_id.len().min(8)]
        ));

        let mut config_error = None;
        if let Some(p) = proxy.as_deref().filter(|p| !p.trim().is_empty()) {
            match ureq::Proxy::new(p) {
                Ok(px) => builder = builder.proxy(px),
                Err(_) => config_error = Some("代理配置无效，请检查 MIAC_PROXY".into()),
            }
        }

        Self { session, proxy, client_id, timeout, agent: builder.build(), session_expired: AtomicBool::new(false), config_error }
    }

    pub fn session(&self) -> &CloudSession {
        &self.session
    }

    pub fn ready(&self) -> bool {
        self.session.ready()
    }

    pub fn proxy_label(&self) -> &str {
        if self.proxy.is_some() { "已配置代理" } else { "直连" }
    }

    pub fn timeout(&self) -> Duration {
        self.timeout
    }

    /// 生成 `_nonce`：12 字节 = 8 随机 + 4 字节「当前分钟数」大端。
    fn generate_nonce() -> Vec<u8> {
        let mut buf = vec![0u8; 12];
        buf[..8].copy_from_slice(&crypto::random_bytes(8));
        let minutes = (std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0)
            / 60) as u32;
        buf[8..].copy_from_slice(&minutes.to_be_bytes());
        buf
    }

    /// `signedNonce` = base64(SHA256(ssecurity_raw ‖ nonce_raw))。
    fn signed_nonce(&self, nonce: &[u8]) -> Result<Vec<u8>, CloudError> {
        let ssecurity = STANDARD
            .decode(self.session.ssecurity.trim())
            .map_err(|e| CloudError::Parse(format!("ssecurity 不是合法 base64：{e}")))?;
        let mut hasher = Sha256::new();
        hasher.update(&ssecurity);
        hasher.update(nonce);
        Ok(hasher.finalize().to_vec())
    }

    /// `signature` = base64(HMAC-SHA256(key = signedNonce 原始字节, msg = path&sn&nonce&k=v…))。
    fn signature(
        path: &str,
        signed_nonce: &[u8],
        nonce_b64: &str,
        params: &[(String, String)],
    ) -> Result<String, CloudError> {
        let signed_nonce_b64 = STANDARD.encode(signed_nonce);

        let mut exps = vec![
            path.to_string(),
            signed_nonce_b64,
            nonce_b64.to_string(),
        ];
        // 参数按 key 排序后拼 `k=v`
        let mut keys: Vec<&(String, String)> = params.iter().collect();
        keys.sort_by(|a, b| a.0.cmp(&b.0));
        for (k, v) in keys {
            exps.push(format!("{k}={v}"));
        }
        let msg = exps.join("&");

        let mut mac = HmacSha256::new_from_slice(signed_nonce)
            .map_err(|e| CloudError::Parse(format!("HMAC key 非法：{e}")))?;
        mac.update(msg.as_bytes());
        Ok(STANDARD.encode(mac.finalize().into_bytes()))
    }

    /// 组装 Cookie（缺项会被判 401，所以照着参考实现写全）。
    fn cookie_header(&self) -> String {
        [
            "sdkVersion=accountsdk-18.8.15".to_string(),
            format!("deviceId={}", self.client_id),
            format!("userId={}", self.session.user_id),
            format!("yetAnotherServiceToken={}", self.session.service_token.as_str()),
            format!("serviceToken={}", self.session.service_token.as_str()),
            "locale=zh_CN".to_string(),
            "channel=MI_APP_STORE".to_string(),
        ]
        .join("; ")
    }

    /// POST 一个 JSON 请求并返回解析后的响应体。
    ///
    /// `path` 以 `/` 开头，例如 `/home/rpc/{did}`。
    pub fn post(&self, path: &str, body: &Value) -> Result<Value, CloudError> {
        if !valid_country_code(self.session.country_code()) {
            return Err(CloudError::Parse("云端国家/地区代码不合法".into()));
        }
        let url = format!("{}{}", api_host(self.session.country_code()), path);
        self.post_at(&url, path, body)
    }

    fn post_at(&self, url: &str, path: &str, body: &Value) -> Result<Value, CloudError> {
        if let Some(error) = &self.config_error { return Err(CloudError::Transport(error.clone())); }
        if !self.session.ready() {
            return Err(CloudError::NoSession);
        }
        if self.session_expired.load(Ordering::Relaxed) {
            return Err(CloudError::SessionExpired);
        }
        let data = body.to_string();

        // 参与签名的「参数」只有 data（_nonce / signature 自身不参与）
        let params = vec![("data".to_string(), data.clone())];

        let nonce_raw = Self::generate_nonce();
        let nonce_b64 = STANDARD.encode(&nonce_raw);
        let signed_nonce = self.signed_nonce(&nonce_raw)?;
        let signature = Self::signature(path, &signed_nonce, &nonce_b64, &params)?;

        // 请求体是 form-urlencoded
        let form = format!(
            "_nonce={}&data={}&signature={}",
            percent_encode(&nonce_b64),
            percent_encode(&data),
            percent_encode(&signature),
        );

        let resp = self
            .agent
            .post(url)
            .set("Content-Type", "application/x-www-form-urlencoded")
            .set("x-xiaomi-protocal-flag-cli", "PROTOCAL-HTTP2")
            .set("mishop-client-id", "180100041079")
            .set("Cookie", &self.cookie_header())
            .send_string(&form)
            .map_err(|e| match e {
                ureq::Error::Status(status, response) => {
                    // 错误页不可信，限长且不把原文写入日志。
                    let mut bytes = Vec::new();
                    let _ = response.into_reader().take(65536).read_to_end(&mut bytes);
                    let value = serde_json::from_slice::<Value>(&bytes).ok();
                    if value.as_ref().is_some_and(|v| response_session_expired(status, v)) {
                        self.session_expired.store(true, Ordering::Relaxed);
                        return CloudError::SessionExpired;
                    }
                    let code = value.as_ref().and_then(|v| v.get("code").and_then(Value::as_i64));
                    let message = value.as_ref().map(|value| self.error_message(value)).unwrap_or_default();
                    CloudError::Http { status, code, message }
                }
                ureq::Error::Transport(_) => CloudError::Transport("网络连接失败，请检查网络和代理配置".into()),
            })?;

        if !(200..300).contains(&resp.status()) {
            return Err(CloudError::Http { status: resp.status(), code: None, message: "云端接口不接受重定向".into() });
        }
        let bytes = crate::io_limits::read(resp.into_reader(), crate::io_limits::HTTP_BYTES)
            .map_err(|e| CloudError::Transport(format!("读取响应失败：{e}")))?;
        let text = String::from_utf8(bytes).map_err(|_| CloudError::Parse("响应不是 UTF-8".into()))?;

        // 登录类接口会带 `&&&START&&&` 前缀，顺手剥掉
        let text = text.strip_prefix("&&&START&&&").unwrap_or(&text).to_string();

        let value: Value = serde_json::from_str(&text).map_err(|e| {
            CloudError::Parse(e.to_string())
        })?;

        if response_session_expired(200, &value) {
            self.session_expired.store(true, Ordering::Relaxed);
            return Err(CloudError::SessionExpired);
        }

        // 统一的 code 检查（0 或缺省都算成功）
        if let Some(code) = value.get("code").and_then(Value::as_i64) {
            if code != 0 {
                let message = self.error_message(&value);
                return Err(CloudError::Api { code, message });
            }
        }
        Ok(value)
    }

    fn error_message(&self, value: &Value) -> String {
        let mut message = value.get("message").or_else(|| value.get("msg"))
            .and_then(Value::as_str).unwrap_or("").to_string();
        for secret in [self.session.ssecurity.as_str(), self.session.service_token.as_str(), self.session.user_id.as_str(), self.session.username.as_str()] {
            if !secret.is_empty() { message = message.replace(secret, "[已隐藏]"); }
        }
        truncate(&message, 160)
    }

    /// 云端 MIoT RPC：`/home/rpc/{did}`。
    ///
    /// 注意：请求体是 `{method, params}`——**不带 id**，
    /// 与局域网 miIO 的报文格式不同（参考实现也是如此）。
    pub fn miio_call(&self, did: &str, method: &str, params: &Value) -> Result<Value, CloudError> {
        if !crate::credentials::valid_did(did) { return Err(CloudError::Parse("设备标识无效".into())); }
        let body = json!({ "method": method, "params": params });
        let resp = self.post(&format!("/home/rpc/{did}"), &body)?;
        if let Some(err) = resp.get("error") {
            let code = err.get("code").and_then(Value::as_i64).unwrap_or(-1);
            let message = self.error_message(err);
            return Err(CloudError::Api { code, message });
        }
        Ok(resp.get("result").cloned().unwrap_or(Value::Null))
    }

    /// 电量统计：`/v2/user/statistics`。
    pub fn statistics(
        &self,
        did: &str,
        key: &str,
        time_start: i64,
        time_end: i64,
        limit: u32,
    ) -> Result<Vec<Value>, CloudError> {
        let body = json!({
            "did": did,
            "key": key,
            "data_type": "stat_day_v3",
            "time_start": time_start,
            "time_end": time_end,
            "limit": limit,
        });
        let resp = self.post("/v2/user/statistics", &body)?;
        result_array(&resp)
    }

    /// 温湿度计历史数据：`/user/get_user_device_data`。
    pub fn device_data(
        &self,
        did: &str,
        key: &str,
        time_start: i64,
        time_end: i64,
        limit: u32,
    ) -> Result<Vec<Value>, CloudError> {
        let body = json!({
            "uid": self.session.user_id,
            "did": did,
            "key": key,
            "type": "prop",
            "time_start": time_start,
            "time_end": time_end,
            "limit": limit,
        });
        let resp = self.post("/user/get_user_device_data", &body)?;
        result_array(&resp)
    }

    /// 温湿度计 MIoT 直读：`/miotspec/prop/get`。
    pub fn prop_get(&self, did: &str, props: &[(u16, u16)]) -> Result<Vec<Value>, CloudError> {
        let params: Vec<Value> = props
            .iter()
            .map(|(siid, piid)| json!({ "did": did, "siid": siid, "piid": piid }))
            .collect();
        let body = json!({ "params": params });
        let resp = self.post("/miotspec/prop/get", &body)?;
        result_array(&resp)
    }

    /// 取设备列表（登录后选设备用）。
    pub fn device_list(&self) -> Result<Vec<Value>, CloudError> {
        let resp = self.post(
            "/home/device_list",
            &json!({ "getVirtualModel": false, "getHuamiDevices": 0 }),
        )?;
        match resp.get("result") {
            Some(Value::Object(o)) => o
                .get("list")
                .and_then(Value::as_array)
                .cloned()
                .ok_or_else(|| CloudError::Parse("设备列表缺少 list 数组".into())),
            Some(Value::Array(a)) => Ok(a.clone()),
            _ => Err(CloudError::Parse("设备列表 result 格式异常".into())),
        }
    }
}

/// 由 userId 派生一个稳定的 deviceId（16 位十进制，FNV-1a 哈希）。
fn derive_client_id(user_id: &str) -> String {
    let mut acc: u64 = 0xcbf2_9ce4_8422_2325; // FNV offset basis
    for b in user_id.bytes() {
        acc ^= b as u64;
        acc = acc.wrapping_mul(0x0000_0100_0000_01b3); // FNV prime
    }
    format!("{:016}", acc % 10_000_000_000_000_000)
}

/// 最小化的 form-urlencoded 百分号编码。
///
/// 规则：字母数字与 `-_.~` 原样，空格转 `+`，其余按 UTF-8 字节转 `%XX`。
fn percent_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len() * 2);
    for b in s.as_bytes() {
        match *b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(*b as char)
            }
            b' ' => out.push('+'),
            other => out.push_str(&format!("%{other:02X}")),
        }
    }
    out
}

fn truncate(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        s.to_string()
    } else {
        s.chars().take(n).collect::<String>() + "…"
    }
}

/// 把「设备历史上报的原始值」换算成温湿度数值。
///
/// 移植自 v1 `lib/controller.js` 的 `cloudRecordNumber()`：部分 BLE 网关上报
/// 原始整数（例如 275 代表 27.5℃），需要除以 10 并做范围校验。
pub fn record_number(record: &Value, kind: &str) -> Option<f64> {
    let mut value = record.get("value")?.clone();

    if let Value::String(s) = &value {
        let t = s.trim();
        value = serde_json::from_str(t)
            .unwrap_or_else(|_| t.parse::<f64>().map(|n| json!(n)).unwrap_or(Value::Null));
    }
    while let Value::Array(a) = &value {
        value = a.first().cloned().unwrap_or(Value::Null);
    }
    if let Value::Object(o) = &value {
        value = o
            .get("value")
            .or_else(|| o.get("v"))
            .cloned()
            .unwrap_or(Value::Null);
    }

    let mut number = value.as_f64()?;
    if !number.is_finite() {
        return None;
    }

    if kind == "temperature" && number.abs() > 100.0 && number.abs() <= 1000.0 {
        number /= 10.0;
    }
    if kind == "humidity" && number > 100.0 && number <= 1000.0 {
        number /= 10.0;
    }
    if kind == "temperature" && !(-50.0..=100.0).contains(&number) {
        return None;
    }
    if kind == "humidity" && !(0.0..=100.0).contains(&number) {
        return None;
    }
    Some(number)
}

/// 解析电量统计里的数值（移植自 v1 `parseStatValue()`）。
pub fn stat_value(record: &Value) -> Option<f64> {
    let mut value = record.get("value")?.clone();
    if let Value::String(s) = &value {
        let t = s.trim();
        value = serde_json::from_str(t)
            .unwrap_or_else(|_| t.parse::<f64>().map(|n| json!(n)).unwrap_or(Value::Null));
    }
    while let Value::Array(a) = &value {
        value = a.first().cloned().unwrap_or(Value::Null);
    }
    value.as_f64().filter(|n| n.is_finite())
}

/// 便于测试：构造一个空会话。
pub fn empty_session_map() -> HashMap<String, String> {
    HashMap::new()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn test_client() -> CloudClient {
        let mut client = CloudClient::new(CloudSession {
            username: "test-user".into(), ssecurity: STANDARD.encode(b"0123456789abcdef").into(),
            user_id: "test-id".into(), service_token: "test-token".into(),
            ..Default::default()
        }, None);
        client.agent = ureq::AgentBuilder::new().redirects(0).try_proxy_from_env(false)
            .timeout(Duration::from_secs(3)).build();
        client
    }

    // 本地假服务器：不使用真实凭据，也不向设备发送控制指令。
    fn mock_response(status: u16, body: impl Into<String>) -> (String, std::thread::JoinHandle<()>) {
        let body = body.into();
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}/app/v2/user/statistics", listener.local_addr().unwrap());
        let handle = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            stream.set_read_timeout(Some(Duration::from_secs(3))).unwrap();
            let mut request = Vec::new();
            let mut chunk = [0; 1024];
            loop {
                let count = stream.read(&mut chunk).unwrap();
                assert!(count > 0);
                request.extend_from_slice(&chunk[..count]);
                if let Some(end) = request.windows(4).position(|w| w == b"\r\n\r\n") {
                    let headers = String::from_utf8_lossy(&request[..end]).to_ascii_lowercase();
                    let length: usize = headers.lines().find_map(|line| line.strip_prefix("content-length:"))
                        .unwrap().trim().parse().unwrap();
                    if request.len() >= end + 4 + length { break; }
                }
            }
            let _ = write!(stream, "HTTP/1.1 {status} Test\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len());
        });
        (url, handle)
    }

    #[test]
    fn oversized_success_and_redirects_are_rejected() {
        let (url, server) = mock_response(200, format!("{{\"code\":0,\"result\":\"{}\"}}", "x".repeat(crate::io_limits::HTTP_BYTES + 1024)));
        assert!(test_client().post_at(&url, "/test", &json!({})).unwrap_err().to_string().contains("大小"));
        server.join().unwrap();
        let (url, server) = mock_response(302, "");
        assert!(matches!(test_client().post_at(&url, "/test", &json!({})), Err(CloudError::Http { status:302, .. })));
        server.join().unwrap();
        assert!(result_array(&json!({"result":null})).is_err());
        assert!(result_array(&json!({"result":[]})).unwrap().is_empty());
    }

    #[test]
    fn expired_426_is_login_error_and_stops_subsequent_network_requests() {
        let (url, server) = mock_response(426, r#"{"code":0,"message":"SERVICETOKEN_EXPIRED"}"#);
        let client = test_client();
        let error = client.post_at(&url, "/v2/user/statistics", &json!({})).unwrap_err();
        assert!(error.is_session_expired());
        assert_eq!(error.to_string(), SESSION_EXPIRED_MESSAGE);
        server.join().unwrap();
        // 即使地址不可用，也必须立即返回过期，不再联网。
        assert!(client.post_at("http://127.0.0.1:1", "/miotspec/prop/get", &json!({}))
            .unwrap_err().is_session_expired());
        assert!(!test_client().session_expired.load(Ordering::Relaxed));
    }

    #[test]
    fn expired_body_with_http_200_is_not_a_success() {
        let (url, server) = mock_response(200, r#"{"code":0,"message":"SERVICETOKEN_EXPIRED"}"#);
        assert!(test_client().post_at(&url, "/v2/user/statistics", &json!({}))
            .unwrap_err().is_session_expired());
        server.join().unwrap();
    }

    #[test]
    fn unrelated_426_and_permission_denial_are_not_assumed_expired() {
        assert!(!response_session_expired(426, &json!({"code": 0, "message": "Upgrade required"})));
        assert!(!response_session_expired(403, &json!({"message": "permission denied"})));
        assert!(response_session_expired(401, &json!({"message": "auth error"})));
    }

    #[test]
    fn ordinary_cloud_success_is_still_parsed() {
        let (url, server) = mock_response(200, r#"{"code":0,"result":[{"value":"1.5"}]}"#);
        let client = test_client();
        let value = client.post_at(&url, "/v2/user/statistics", &json!({})).unwrap();
        assert_eq!(value["result"][0]["value"], "1.5");
        assert!(!client.session_expired.load(Ordering::Relaxed));
        server.join().unwrap();
    }

    #[test]
    fn http_error_message_does_not_leak_session_secrets() {
        let (url, server) = mock_response(403, r#"{"code":-1,"message":"test-token test-id MDEyMzQ1Njc4OWFiY2RlZg=="}"#);
        let error = test_client().post_at(&url, "/v2/user/statistics", &json!({})).unwrap_err().to_string();
        assert!(!error.contains("test-token"));
        assert!(!error.contains("test-id"));
        assert!(!error.contains("MDEyMzQ1Njc4OWFiY2RlZg=="));
        server.join().unwrap();
    }

    #[test]
    fn api_error_message_does_not_leak_session_secrets() {
        let (url, server) = mock_response(200, r#"{"code":-1,"message":"test-token test-id MDEyMzQ1Njc4OWFiY2RlZg=="}"#);
        let error = test_client().post_at(&url, "/v2/user/statistics", &json!({})).unwrap_err().to_string();
        assert!(!error.contains("test-token"));
        assert!(!error.contains("test-id"));
        assert!(!error.contains("MDEyMzQ1Njc4OWFiY2RlZg=="));
        server.join().unwrap();
    }

    #[test]
    fn malformed_country_cannot_change_the_api_authority() {
        for country in ["cn.api.io.mi.com@evil.example/", "cn/", "cn:443", "cn\\", "cn?", "cn#"] {
            assert_eq!(api_host(country), "https://api.io.mi.com/app");
        }
    }

    #[test]
    fn session_ready_requires_four_fields() {
        let mut s = CloudSession::default();
        assert!(!s.ready());
        s.username = "123".into();
        s.ssecurity = "abc".into();
        s.user_id = "456".into();
        assert!(!s.ready());
        s.service_token = "tok".into();
        assert!(s.ready());
        s.service_token = "   ".into();
        assert!(!s.ready());
    }

    #[test]
    fn session_json_roundtrip_is_v1_compatible() {
        let json = r#"{
            "username": "1234567",
            "ssecurity": "AbCdEf==",
            "userId": "987654",
            "serviceToken": "token-value",
            "country": "cn"
        }"#;
        let s: CloudSession = serde_json::from_str(json).unwrap();
        assert_eq!(s.user_id, "987654");
        assert_eq!(s.service_token, "token-value");
        assert!(s.ready());

        let back = serde_json::to_string(&s).unwrap();
        assert!(back.contains("\"userId\":\"987654\""));
        assert!(back.contains("\"serviceToken\":\"token-value\""));
    }

    #[test]
    fn nonce_is_12_bytes_with_minute_stamp() {
        let n = CloudClient::generate_nonce();
        assert_eq!(n.len(), 12, "nonce 必须是 12 字节");
        let minutes = u32::from_be_bytes([n[8], n[9], n[10], n[11]]);
        let now_min = (std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs()
            / 60) as u32;
        assert!(
            minutes.abs_diff(now_min) <= 1,
            "nonce 里的时间戳应是当前分钟数：{minutes} vs {now_min}"
        );
        let n2 = CloudClient::generate_nonce();
        assert_ne!(&n[..8], &n2[..8], "随机段不应重复");
    }

    #[test]
    fn signed_nonce_matches_sha256_of_concat() {
        let ssecurity_b64 = STANDARD.encode(b"0123456789abcdef");
        let session = CloudSession {
            username: "u".into(),
            ssecurity: ssecurity_b64.into(),
            user_id: "1".into(),
            service_token: "t".into(),
            country: Some("cn".into()),
            ..Default::default()
        };
        let client = CloudClient::new(session, None);
        let nonce = b"abcdefghijkl";
        let sn = client.signed_nonce(nonce).unwrap();

        let mut h = Sha256::new();
        h.update(b"0123456789abcdef");
        h.update(nonce);
        assert_eq!(sn, h.finalize().to_vec(), "signedNonce 算法必须与参考实现一致");
    }

    #[test]
    fn signature_is_hmac_over_ordered_exps() {
        let signed_nonce = b"0123456789abcdef0123456789abcdef";
        let nonce_b64 = "AAAAAAAAAAAAAAAA";
        let params = vec![("data".to_string(), r#"{"a":1}"#.to_string())];

        let sig = CloudClient::signature("/home/rpc/1", signed_nonce, nonce_b64, &params).unwrap();

        let sn_b64 = STANDARD.encode(signed_nonce);
        let msg = format!("/home/rpc/1&{sn_b64}&{nonce_b64}&data={}", r#"{"a":1}"#);
        let mut mac = HmacSha256::new_from_slice(signed_nonce).unwrap();
        mac.update(msg.as_bytes());
        let expect = STANDARD.encode(mac.finalize().into_bytes());

        assert_eq!(sig, expect, "signature 必须是 HMAC-SHA256(path&sn&nonce&params)");
    }

    #[test]
    fn signature_key_is_signed_nonce_not_its_base64() {
        // 易错点：HMAC 的 key 是 signedNonce 的**原始字节**，不是它的 base64 字符串。
        let signed_nonce = b"0123456789abcdef";
        let params = vec![("data".to_string(), "x".to_string())];
        let a = CloudClient::signature("/p", signed_nonce, "nnnn", &params).unwrap();

        let as_b64_bytes = STANDARD.encode(signed_nonce).into_bytes();
        let sn_b64 = STANDARD.encode(signed_nonce);
        let mut mac = HmacSha256::new_from_slice(&as_b64_bytes).unwrap();
        mac.update(format!("/p&{sn_b64}&nnnn&data=x").as_bytes());
        let b = STANDARD.encode(mac.finalize().into_bytes());

        assert_ne!(a, b, "用 base64 字符串当 key 会算出不同签名");
    }

    #[test]
    fn percent_encode_form_rules() {
        assert_eq!(percent_encode("abcXYZ019"), "abcXYZ019");
        assert_eq!(percent_encode("-_.~"), "-_.~");
        assert_eq!(percent_encode(" "), "+");
        assert_eq!(percent_encode("{"), "%7B");
        assert_eq!(percent_encode("a=b&c"), "a%3Db%26c");
        assert_eq!(percent_encode("中"), "%E4%B8%AD");
    }

    #[test]
    fn derive_client_id_is_stable_and_numeric() {
        let a = derive_client_id("987654");
        assert_eq!(a, derive_client_id("987654"), "同一 userId 必须得到同一 deviceId");
        assert_eq!(a.len(), 16);
        assert!(a.chars().all(|c| c.is_ascii_digit()));
        assert_ne!(a, derive_client_id("999"));
    }

    #[test]
    fn record_number_handles_ble_gateway_scaling() {
        assert_eq!(record_number(&json!({ "value": "275" }), "temperature"), Some(27.5));
        assert_eq!(record_number(&json!({ "value": [26.8] }), "temperature"), Some(26.8));
        assert_eq!(record_number(&json!({ "value": { "value": 58 } }), "humidity"), Some(58.0));
        assert_eq!(record_number(&json!({ "value": 9999 }), "temperature"), None);
    }

    #[test]
    fn stat_value_parses_string_numbers() {
        assert_eq!(stat_value(&json!({ "value": "6.4" })), Some(6.4));
        assert_eq!(stat_value(&json!({ "value": [3] })), Some(3.0));
        assert_eq!(stat_value(&json!({ "value": "not-a-number" })), None);
    }

    #[test]
    fn api_host_selection() {
        assert_eq!(api_host("cn"), "https://api.io.mi.com/app");
        assert_eq!(api_host(""), "https://api.io.mi.com/app");
        assert_eq!(api_host("CN"), "https://api.io.mi.com/app", "大小写不敏感");
        assert!(api_host("de").starts_with("https://de."));
    }
}
