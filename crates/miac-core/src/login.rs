//! login.rs —— 小米账号扫码登录
//!
//! 移植自 v1 的 `mi-qr-login.js` + `app/main.js` 的登录部分。流程：
//!
//! 1. `GET /longPolling/loginUrl` → 拿到二维码图片地址 `qr` 与轮询地址 `lp`
//! 2. `GET qr` → 下载二维码图片（PNG 或 JPEG）
//! 3. 轮询 `lp` 直到用户在米家 App 里确认；响应里带 `userId` / `ssecurity` / `location`
//! 4. `GET location` → 从 Set-Cookie 里取 `serviceToken`
//! 5. 用这套会话拉设备列表 → 用户选空调 → 写 `device.json` / `thermometer.json`
//!
//! `cloud-session.json` 只存米家云 RPC 需要的四项（username / ssecurity /
//! userId / serviceToken），**不存密码**——与 v1 完全一致。
//!
//! ## 两个安全约束（保留 v1 的做法，不要放松）
//!
//! - **只允许跳转到 xiaomi.com / mi.com**：服务端返回的地址要过 `official_url`
//!   校验，避免被引导到第三方域名。
//! - **cookie 用宽松模式解析**：小米偶尔返回不合 RFC 的 cookie 属性，
//!   严格解析会把整次登录搞失败；而登录只需要 name=value 这一对。
//!
//! ## TLS
//!
//! 与 `cloud.rs` 同理，必须用 rustls + 内置根证书（本机 schannel 不可用）。

use std::time::{Duration, Instant};

use serde_json::Value;

use crate::cloud::CloudSession;

/// 登录相关错误。
#[derive(Debug)]
pub enum LoginError {
    /// 地址不合法（不是小米域名）
    BadUrl(String),
    /// 网络问题
    Transport(String),
    /// 响应不是预期内容
    Parse(String),
    /// 二维码过期
    Expired,
    /// 用户取消
    Cancelled,
    /// 其它
    Other(String),
}

impl std::fmt::Display for LoginError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LoginError::BadUrl(m) => write!(f, "小米返回的登录地址不受支持：{m}"),
            LoginError::Transport(m) => write!(f, "无法连接小米登录服务：{m}"),
            LoginError::Parse(m) => write!(f, "小米登录服务返回异常：{m}"),
            LoginError::Expired => write!(f, "二维码已过期，请重新生成"),
            LoginError::Cancelled => write!(f, "已取消登录"),
            LoginError::Other(m) => write!(f, "{m}"),
        }
    }
}

impl std::error::Error for LoginError {}

/// 校验并规范化一个登录相关地址。
///
/// **只允许 https 且主机名以 `xiaomi.com` / `mi.com` 结尾**，
/// 且不允许带用户名密码。这是防止服务端返回的跳转把凭据带到别处。
pub fn official_url(value: &str) -> Result<String, LoginError> {
    let trimmed = value.trim();
    let full = if trimmed.starts_with("http://") || trimmed.starts_with("https://") {
        trimmed.to_string()
    } else if let Some(rest) = trimmed.strip_prefix("//") {
        format!("https://{rest}")
    } else if trimmed.starts_with('/') {
        format!("https://account.xiaomi.com{trimmed}")
    } else {
        format!("https://{trimmed}")
    };

    // 手写解析（不引 url crate）：只需要 scheme / host / 是否带 userinfo
    let after_scheme = full
        .strip_prefix("https://")
        .ok_or_else(|| LoginError::BadUrl(full.clone()))?;
    let authority_end = after_scheme
        .find(['/', '?', '#'])
        .unwrap_or(after_scheme.len());
    let authority = &after_scheme[..authority_end];
    if authority.is_empty() || authority.contains('@') {
        return Err(LoginError::BadUrl(full));
    }
    // 去掉端口再比对主机名
    let host = authority.split(':').next().unwrap_or("").to_ascii_lowercase();
    let ok = host == "xiaomi.com"
        || host.ends_with(".xiaomi.com")
        || host == "mi.com"
        || host.ends_with(".mi.com");
    if !ok {
        return Err(LoginError::BadUrl(full));
    }
    Ok(full)
}

/// 极简 cookie 存储：只按域名字符串匹配保存 name=value。
///
/// 之所以不用成熟实现：小米登录只需要把服务端下发的 cookie 原样回传，
/// 而它偶尔会带不合 RFC 的属性（`looseMode` 要处理的就是这个）。
/// 这里只取每段 Set-Cookie 的第一对 `name=value`，其余属性一律忽略。
#[derive(Default)]
pub struct CookieJar {
    /// (域名后缀, name, value)
    entries: Vec<(String, String, String)>,
}

impl CookieJar {
    pub fn new() -> Self {
        Self::default()
    }

    /// 记录响应里的所有 Set-Cookie。
    ///
    /// 返回本次响应中新出现的 `serviceToken`（登录流程要单独抓它，
    /// 因为它在跳转过程中下发，可能不在最终页面的域名下）。
    pub fn absorb(&mut self, url: &str, set_cookies: &[String]) -> Option<String> {
        let host = host_of(url);
        let mut service_token = None;

        for raw in set_cookies {
            let pair = raw.split(';').next().unwrap_or("").trim();
            let Some(eq) = pair.find('=') else { continue };
            let name = pair[..eq].trim().to_string();
            let value = pair[eq + 1..].trim().trim_matches('"').to_string();
            if name.is_empty() {
                continue;
            }
            if name == "serviceToken" && !value.is_empty() {
                service_token = Some(value.clone());
            }
            // 同名的覆盖旧的
            self.entries.retain(|(d, n, _)| !(d == &host && n == &name));
            self.entries.push((host.clone(), name, value));
        }
        service_token
    }

    /// 取某个 URL 的 cookie 串。
    ///
    /// 匹配规则：cookie 的域名是 URL 域名的后缀（含相等），
    /// 这样 `account.xiaomi.com` 下发的 cookie 在 `sts.api.io.mi.com` 上不会误发。
    pub fn cookie_string(&self, url: &str) -> String {
        let host = host_of(url);
        self.entries
            .iter()
            .filter(|(d, _, _)| host == *d || host.ends_with(&format!(".{d}")))
            .map(|(_, n, v)| format!("{n}={v}"))
            .collect::<Vec<_>>()
            .join("; ")
    }

    /// 查一个 cookie 的值。
    pub fn get(&self, name: &str) -> Option<&str> {
        self.entries
            .iter()
            .find(|(_, n, _)| n == name)
            .map(|(_, _, v)| v.as_str())
    }
}

fn host_of(url: &str) -> String {
    url.strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"))
        .unwrap_or(url)
        .split(['/', '?', '#'])
        .next()
        .unwrap_or("")
        .split(':')
        .next()
        .unwrap_or("")
        .to_ascii_lowercase()
}

/// 扫码挑战：二维码图片 + 轮询地址 + 过期时间。
pub struct Challenge {
    /// 二维码图片原始字节（PNG 或 JPEG）
    pub image: Vec<u8>,
    /// 图片的 MIME
    pub mime: &'static str,
    /// 轮询地址
    pub poll_url: String,
    /// 过期时刻
    pub expires_at: Instant,
}

/// 登录成功后拿到的东西。
#[derive(Debug, Clone)]
pub struct LoginResult {
    pub user_id: String,
    pub ssecurity: String,
    pub service_token: String,
}

impl LoginResult {
    /// 组装成要写盘的 cloud-session（字段与 v1 一致）。
    pub fn to_session(&self, country: &str) -> CloudSession {
        CloudSession {
            username: self.user_id.clone(),
            ssecurity: self.ssecurity.clone(),
            user_id: self.user_id.clone(),
            service_token: self.service_token.clone(),
            country: Some(country.to_string()),
            region: None,
            saved_at: Some(now_iso8601()),
        }
    }
}

/// 米家云设备（登录后选设备用）。
#[derive(Debug, Clone)]
pub struct CloudDevice {
    pub name: String,
    pub model: String,
    pub did: String,
    pub localip: Option<String>,
    pub token: Option<String>,
    pub online: bool,
}

impl CloudDevice {
    /// 从设备列表的一条记录里取字段（字段名与米家云一致）。
    pub fn from_json(v: &Value) -> Option<Self> {
        let did = v.get("did").and_then(Value::as_str)?.to_string();
        Some(Self {
            name: v.get("name").and_then(Value::as_str).unwrap_or("").to_string(),
            model: v.get("model").and_then(Value::as_str).unwrap_or("").to_string(),
            did,
            localip: v
                .get("localip")
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty())
                .map(str::to_string),
            token: v
                .get("token")
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty())
                .map(str::to_string),
            online: v.get("isOnline").and_then(Value::as_bool).unwrap_or(false),
        })
    }

    /// 看起来像空调（与 v1 的判定一致：型号或名字里含空调特征）。
    pub fn looks_like_ac(&self) -> bool {
        let m = self.model.to_ascii_lowercase();
        m.contains("aircondition") || m.contains("airc.") || self.name.contains("空调")
    }

    /// 是米家智能温湿度计 3。
    pub fn is_thermometer(&self) -> bool {
        self.model == "miaomiaoce.sensor_ht.t9" || self.name.contains("温湿度计")
    }
}

/// 登录会话（持有 cookie 与 HTTP 客户端）。
pub struct LoginSession {
    jar: CookieJar,
    agent: ureq::Agent,
    service_token: Option<String>,
}

impl LoginSession {
    /// 建会话。`proxy` 为出口代理（本机必须经代理才能出网）。
    pub fn new(proxy: Option<String>) -> Self {
        let proxy = proxy.or_else(|| std::env::var("MIAC_PROXY").ok());
        let timeout = Duration::from_millis(
            std::env::var("MIAC_LOGIN_TIMEOUT_MS")
                .ok()
                .and_then(|s| s.parse().ok())
                .unwrap_or(15000),
        );
        let mut builder = ureq::AgentBuilder::new()
            .timeout(timeout)
            .redirects(0) // 手动处理跳转，才能逐跳收集 cookie
            .user_agent("MiHome/6.0.0 (Android)");
        if let Some(p) = proxy.as_deref().filter(|p| !p.trim().is_empty()) {
            if let Ok(px) = ureq::Proxy::new(p) {
                builder = builder.proxy(px);
            }
        }
        Self { jar: CookieJar::new(), agent: builder.build(), service_token: None }
    }

    /// 发起一次请求并跟随跳转（每跳收集 Set-Cookie），返回最终响应体文本与最终 URL。
    pub fn request(&mut self, url: &str) -> Result<(String, String), LoginError> {
        let mut current = official_url(url)?;

        for _ in 0..10 {
            let cookie = self.jar.cookie_string(&current);
            let mut req = self.agent.get(&current);
            if !cookie.is_empty() {
                req = req.set("Cookie", &cookie);
            }
            let resp = req
                .call()
                .map_err(|e| LoginError::Transport(e.to_string()))?;

            // 收集 cookie（逐跳都要收）
            let set_cookies: Vec<String> = resp
                .all("set-cookie")
                .into_iter()
                .map(str::to_string)
                .collect();
            if let Some(tok) = self.jar.absorb(&current, &set_cookies) {
                self.service_token = Some(tok);
            }

            let status = resp.status();
            if (300..400).contains(&status) {
                let Some(loc) = resp.header("location") else {
                    return Err(LoginError::Parse("登录跳转缺少地址".into()));
                };
                current = official_url(loc)?;
                continue;
            }
            if !(200..300).contains(&status) {
                return Err(LoginError::Other(format!("小米登录服务 HTTP {status}")));
            }
            let text = resp
                .into_string()
                .map_err(|e| LoginError::Transport(format!("读取响应失败：{e}")))?;
            return Ok((text, current));
        }
        Err(LoginError::Parse("小米登录跳转次数过多".into()))
    }

    /// 取原始字节（下载二维码图片用）。
    pub fn get_bytes(&mut self, url: &str, limit: usize) -> Result<Vec<u8>, LoginError> {
        let url = official_url(url)?;
        let cookie = self.jar.cookie_string(&url);
        let mut req = self.agent.get(&url);
        if !cookie.is_empty() {
            req = req.set("Cookie", &cookie);
        }
        let resp = req
            .call()
            .map_err(|e| LoginError::Transport(e.to_string()))?;
        let set_cookies: Vec<String> = resp
            .all("set-cookie")
            .into_iter()
            .map(str::to_string)
            .collect();
        self.jar.absorb(&url, &set_cookies);

        let mut buf = Vec::new();
        use std::io::Read;
        resp.into_reader()
            .take(limit as u64)
            .read_to_end(&mut buf)
            .map_err(|e| LoginError::Transport(format!("下载二维码失败：{e}")))?;
        Ok(buf)
    }

    /// 生成扫码挑战（拿到二维码图片与轮询地址）。
    pub fn create_challenge(&mut self) -> Result<Challenge, LoginError> {
        let dc = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis())
            .unwrap_or(0);
        let url = format!(
            "https://account.xiaomi.com/longPolling/loginUrl\
             ?_qrsize=480&qs=%3Fsid%3Dxiaomiio%26_json%3Dtrue\
             &callback=https%3A%2F%2Fsts.api.io.mi.com%2Fsts\
             &_hasLogo=false&sid=xiaomiio&serviceParam=&_locale=zh_CN&_dc={dc}"
        );

        let (text, _) = self.request(&url)?;
        let data: Value = parse_xiaomi_json(&text)?;

        let qr = data
            .get("qr")
            .and_then(Value::as_str)
            .ok_or_else(|| {
                LoginError::Parse(format!(
                    "无法生成二维码（code={}）",
                    data.get("code").and_then(Value::as_i64).unwrap_or(0)
                ))
            })?
            .to_string();
        let lp = data
            .get("lp")
            .and_then(Value::as_str)
            .ok_or_else(|| LoginError::Parse("响应里没有轮询地址".into()))?
            .to_string();

        // 二维码图片：PNG 或 JPEG 都接受
        let image = self.get_bytes(&qr, 2 * 1024 * 1024)?;
        let is_png = image.len() > 8 && image[..8] == [0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a];
        let is_jpeg = image.len() > 2 && image[0] == 0xff && image[1] == 0xd8;
        if !is_png && !is_jpeg {
            return Err(LoginError::Parse("小米返回的二维码不是有效图片".into()));
        }

        // 有效期：服务端给的 timeout（秒），夹到 30~300 秒
        let secs = data
            .get("timeout")
            .and_then(Value::as_i64)
            .unwrap_or(180)
            .clamp(30, 300) as u64;

        Ok(Challenge {
            image,
            mime: if is_png { "image/png" } else { "image/jpeg" },
            poll_url: official_url(&lp)?,
            expires_at: Instant::now() + Duration::from_secs(secs),
        })
    }

    /// 等用户扫码确认。
    ///
    /// `tick` 每轮被调用一次，可用于上报进度或检查取消；
    /// 返回 `false` 表示要求中止（用户取消）。
    pub fn wait_for_approval(
        &mut self,
        challenge: &Challenge,
        mut tick: impl FnMut() -> bool,
    ) -> Result<LoginResult, LoginError> {
        while Instant::now() < challenge.expires_at {
            if !tick() {
                return Err(LoginError::Cancelled);
            }

            let (text, _) = match self.request(&challenge.poll_url) {
                Ok(v) => v,
                // 轮询超时是常态（长轮询），继续下一轮
                Err(LoginError::Transport(_)) => continue,
                Err(e) => return Err(e),
            };
            let data: Value = parse_xiaomi_json(&text)?;

            let location = data.get("location").and_then(Value::as_str);

            // userId 可能是 JSON 字符串或数字；location 齐全才算用户确认了。
            let Some(location) = location else {
                return Err(LoginError::Other(format!(
                    "扫码未完成或已失效（code={}），请重新生成二维码",
                    data.get("code").and_then(Value::as_i64).unwrap_or(0)
                )));
            };

            // 用 location 换 serviceToken
            let _ = self.request(location)?;
            let token = self
                .service_token
                .clone()
                .or_else(|| self.jar.get("serviceToken").map(str::to_string))
                .ok_or_else(|| {
                    LoginError::Other("扫码已确认，但小米未返回米家云令牌".into())
                })?;

            return login_result_from_payload(&data, &token);
        }
        Err(LoginError::Expired)
    }
}

fn login_result_from_payload(data: &Value, service_token: &str) -> Result<LoginResult, LoginError> {
    let user_id = data
        .get("userId")
        .and_then(|value| match value {
            Value::String(value) if !value.is_empty() => Some(value.clone()),
            Value::Number(value) => Some(value.to_string()),
            _ => None,
        });
    let ssecurity = data
        .get("ssecurity")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty());
    let (Some(user_id), Some(ssecurity)) = (user_id, ssecurity) else {
        return Err(LoginError::Parse("扫码结果缺少账号凭据".into()));
    };
    if service_token.is_empty() {
        return Err(LoginError::Parse("扫码结果缺少米家云令牌".into()));
    }

    Ok(LoginResult {
        user_id,
        ssecurity: ssecurity.to_string(),
        service_token: service_token.to_string(),
    })
}

/// 解析小米的 JSON 响应（可能带 `&&&START&&&` 前缀）。
pub fn parse_xiaomi_json(text: &str) -> Result<Value, LoginError> {
    let body = text.strip_prefix("&&&START&&&").unwrap_or(text);
    serde_json::from_str(body.trim())
        .map_err(|e| LoginError::Parse(format!("返回了非 JSON 内容：{e}")))
}

/// 用已有会话拉设备列表。
pub fn fetch_devices(
    session: &crate::cloud::CloudSession,
    proxy: Option<String>,
) -> Result<Vec<CloudDevice>, LoginError> {
    let client = crate::cloud::CloudClient::new(session.clone(), proxy);
    let list = client
        .device_list()
        .map_err(|e| LoginError::Other(format!("米家云设备列表读取失败：{e}")))?;
    Ok(list.iter().filter_map(CloudDevice::from_json).collect())
}

/// ISO8601 时间戳（与 v1 的 `new Date().toISOString()` 同格式）。
pub fn now_iso8601() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let (y, mo, d) = crate::controller::ymd_from_unix(secs as i64);
    let day = secs % 86400;
    format!(
        "{y:04}-{mo:02}-{d:02}T{:02}:{:02}:{:02}.000Z",
        day / 3600,
        (day % 3600) / 60,
        day % 60
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn official_url_accepts_xiaomi_only() {
        assert!(official_url("https://account.xiaomi.com/pass/x").is_ok());
        assert!(official_url("https://sts.api.io.mi.com/sts").is_ok());
        assert!(official_url("https://api.io.mi.com/app").is_ok());
        assert!(official_url("/longPolling/loginUrl").is_ok(), "相对路径应补成 account.xiaomi.com");
        assert!(official_url("//account.xiaomi.com/x").is_ok());
        assert!(official_url("account.xiaomi.com/x").is_ok());
    }

    #[test]
    fn official_url_rejects_foreign_and_http() {
        // 非小米域名一律拒绝——防止跳转把凭据带出去
        assert!(official_url("https://evil.com/x").is_err());
        assert!(official_url("https://xiaomi.com.evil.com/x").is_err());
        assert!(official_url("https://notxiaomi.com/x").is_err());
        // http 不允许
        assert!(official_url("http://account.xiaomi.com/x").is_err());
        // 带 userinfo 不允许
        assert!(official_url("https://user:pass@account.xiaomi.com/x").is_err());
    }

    #[test]
    fn cookie_jar_keeps_pairs_and_scopes_by_host() {
        let mut jar = CookieJar::new();
        let tok = jar.absorb(
            "https://account.xiaomi.com/pass",
            &[
                "serviceToken=ABC123; Path=/; HttpOnly".to_string(),
                // 故意带一个不合 RFC 的属性，宽松解析应当仍能保住 name=value
                "userId=42; Version=1; weird==thing".to_string(),
            ],
        );
        assert_eq!(tok.as_deref(), Some("ABC123"));
        assert_eq!(jar.get("serviceToken"), Some("ABC123"));
        assert_eq!(jar.get("userId"), Some("42"));

        let s = jar.cookie_string("https://account.xiaomi.com/pass/next");
        assert!(s.contains("serviceToken=ABC123"), "同域应带上 cookie：{s}");
        // 不相关域名不该带上
        let other = jar.cookie_string("https://example.com/");
        assert!(other.is_empty(), "其它域名不应带 cookie：{other}");
    }

    #[test]
    fn cookie_jar_overwrites_same_name() {
        let mut jar = CookieJar::new();
        jar.absorb("https://a.xiaomi.com/x", &["k=1; Path=/".to_string()]);
        jar.absorb("https://a.xiaomi.com/y", &["k=2; Path=/".to_string()]);
        assert_eq!(jar.get("k"), Some("2"), "同名 cookie 应被覆盖");
    }

    #[test]
    fn cookie_jar_handles_quoted_values() {
        let mut jar = CookieJar::new();
        jar.absorb("https://x.xiaomi.com/", &["serviceToken=\"quoted\"; Path=/".to_string()]);
        assert_eq!(jar.get("serviceToken"), Some("quoted"), "引号应被剥掉");
    }

    #[test]
    fn parse_strips_start_marker() {
        let v = parse_xiaomi_json("&&&START&&&{\"code\":0,\"userId\":123}").unwrap();
        assert_eq!(v.get("userId").and_then(Value::as_i64), Some(123));
        // 不带前缀也能解析
        let v2 = parse_xiaomi_json("{\"code\":0}").unwrap();
        assert_eq!(v2.get("code").and_then(Value::as_i64), Some(0));
        // 非 JSON 报错
        assert!(parse_xiaomi_json("<html>nope</html>").is_err());
    }

    #[test]
    fn qr_approval_accepts_string_user_id() {
        let payload = serde_json::json!({
            "code": 0,
            "userId": "987654321",
            "ssecurity": "SSEC==",
            "location": "https://sts.api.io.mi.com/sts"
        });
        let result = login_result_from_payload(&payload, "TOKEN").unwrap();
        assert_eq!(result.user_id, "987654321");
        assert_eq!(result.ssecurity, "SSEC==");
        assert_eq!(result.service_token, "TOKEN");

        let numeric = serde_json::json!({
            "code": 0,
            "userId": 987654321,
            "ssecurity": "SSEC==",
            "location": "https://sts.api.io.mi.com/sts"
        });
        assert_eq!(
            login_result_from_payload(&numeric, "TOKEN")
                .unwrap()
                .user_id,
            "987654321"
        );
    }

    #[test]
    fn cloud_device_parsing_and_classification() {
        let ac = serde_json::json!({
            "name": "空调", "model": "xiaomi.airc.h53h00", "did": "123",
            "localip": "192.168.1.5", "token": "aabb", "isOnline": true
        });
        let d = CloudDevice::from_json(&ac).unwrap();
        assert_eq!(d.did, "123");
        assert!(d.online);
        assert!(d.looks_like_ac());
        assert!(!d.is_thermometer());

        let th = serde_json::json!({
            "name": "温湿度计 3", "model": "miaomiaoce.sensor_ht.t9", "did": "blt.3.x",
            "isOnline": true
        });
        let t = CloudDevice::from_json(&th).unwrap();
        assert!(t.is_thermometer());
        assert!(!t.looks_like_ac());
        assert_eq!(t.localip, None, "空 localip 应归一成 None");

        // 缺 did 的记录直接丢弃
        assert!(CloudDevice::from_json(&serde_json::json!({"name":"x"})).is_none());
    }

    #[test]
    fn login_result_builds_v1_compatible_session() {
        let r = LoginResult {
            user_id: "987654".into(),
            ssecurity: "SSEC==".into(),
            service_token: "TOK".into(),
        };
        let s = r.to_session("cn");
        assert!(s.ready(), "登录结果应构成可用会话");
        assert_eq!(s.username, "987654");
        assert_eq!(s.user_id, "987654");
        assert_eq!(s.country_code(), "cn");
        // 序列化后字段名必须是 v1 认得的驼峰
        let json = serde_json::to_string(&s).unwrap();
        assert!(json.contains("\"userId\""));
        assert!(json.contains("\"serviceToken\""));
        assert!(json.contains("\"savedAt\""));
    }

    #[test]
    fn iso8601_shape() {
        let t = now_iso8601();
        assert_eq!(t.len(), 24, "应与 Date.toISOString() 同长度：{t}");
        assert!(t.ends_with(".000Z"));
        assert_eq!(&t[4..5], "-");
        assert_eq!(&t[10..11], "T");
    }
}
