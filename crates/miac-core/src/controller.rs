//! controller.rs —— 空调控制核心
//!
//! 移植自 v1 `lib/controller.js`，语义逐条对齐。负责：
//!   - 通道选择：局域网优先（~0.4 秒），云端兜底；云端失败自动降级回局域网
//!   - 属性读写：read_props / write_prop / raw_read / raw_write
//!   - 语义操作：开关机 / 温度 / 模式 / 风速 / 摆风 / 风感 / 定格 / ECO …
//!   - 数据读取：状态快照 / 机器诊断 / 温湿度计 / 耗电日历
//!
//! ## 重要背景（2026-09 实测）
//!
//! 局域网写入后米家 App 约 1 秒内同步显示——设备会把属性变化**主动上报**
//! MIoT Cloud，与指令来源无关。所以局域网直连既快又一致，作为默认通道；
//! 云端仅在设备缺 localip/token（异地、跨网段）时使用。
//!
//! ## 线程模型
//!
//! 全部同步阻塞调用。为守住内存指标（25–45 MB）本项目不引入 tokio，
//! 界面侧把 controller 放在单独线程里跑，通过 channel 回传结果。

use std::time::Duration;

use serde_json::{json, Value};

use crate::cloud::{self, CloudClient};
use crate::credentials::{Credentials, DeviceInfo, ThermometerInfo};
use crate::miio::MiioDevice;
use crate::miot;

/// 一次 RPC 调用的出错信息。
#[derive(Debug, Clone)]
pub struct ControllerError(pub String);

impl ControllerError {
    pub fn requires_cloud_login(&self) -> bool {
        self.0.contains(cloud::SESSION_EXPIRED_MESSAGE)
    }
}

impl std::fmt::Display for ControllerError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl std::error::Error for ControllerError {}

impl From<crate::miio::MiioError> for ControllerError {
    fn from(e: crate::miio::MiioError) -> Self {
        ControllerError(e.to_string())
    }
}

impl From<cloud::CloudError> for ControllerError {
    fn from(e: cloud::CloudError) -> Self {
        ControllerError(e.to_string())
    }
}

/// 实际生效的链路。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActiveLink {
    Local,
    Cloud,
}

impl ActiveLink {
    pub fn label(self) -> &'static str {
        match self {
            ActiveLink::Local => "局域网直连",
            ActiveLink::Cloud => "云端 RPC",
        }
    }
}

/// 单个属性的读取结果。
#[derive(Debug, Clone, PartialEq)]
pub enum PropValue {
    /// 读到值
    Ok(Value),
    /// 设备返回了错误码
    Err(i64),
    /// 没有收到该项
    Missing,
}

impl PropValue {
    pub fn as_bool(&self) -> Option<bool> {
        match self {
            PropValue::Ok(Value::Bool(b)) => Some(*b),
            _ => None,
        }
    }

    pub fn as_f64(&self) -> Option<f64> {
        match self {
            PropValue::Ok(v) => v.as_f64(),
            _ => None,
        }
    }

    pub fn as_i64(&self) -> Option<i64> {
        match self {
            PropValue::Ok(v) => v.as_i64().or_else(|| v.as_f64().map(|f| f as i64)),
            _ => None,
        }
    }

    /// 界面展示用文本（与 v1 的显示习惯一致）。
    pub fn display(&self) -> String {
        match self {
            PropValue::Ok(Value::Bool(b)) => {
                if *b { "true".into() } else { "false".into() }
            }
            PropValue::Ok(Value::String(s)) => s.clone(),
            PropValue::Ok(Value::Null) => "null".into(),
            PropValue::Ok(v) => v.to_string(),
            PropValue::Err(code) => format!("err({code})"),
            PropValue::Missing => "—".into(),
        }
    }
}

/// 状态快照（界面主循环用）。
#[derive(Debug, Clone)]
pub struct Snapshot {
    pub link: Option<ActiveLink>,
    pub status: Vec<(&'static str, PropValue)>,
    pub fault: miot::FaultInfo,
}

impl Snapshot {
    pub fn get(&self, name: &str) -> Option<&PropValue> {
        self.status.iter().find(|(n, _)| *n == name).map(|(_, v)| v)
    }
}

/// 温湿度计读数。
#[derive(Debug, Clone)]
pub struct ThermometerReading {
    pub available: bool,
    pub name: String,
    pub temperature: Option<f64>,
    pub humidity: Option<f64>,
    pub battery: Option<f64>,
    pub reason: Option<String>,
}

impl ThermometerReading {
    fn unavailable(reason: impl Into<String>) -> Self {
        Self {
            available: false,
            name: "温湿度计".into(),
            temperature: None,
            humidity: None,
            battery: None,
            reason: Some(reason.into()),
        }
    }
}

/// 耗电日历（与界面日历结构直接对应）。
#[derive(Debug, Clone, Default)]
pub struct PowerStats {
    pub year: i32,
    pub month: u32,
    pub day: u32,
    pub days_in_month: u32,
    /// 当月 1 号是星期几（0 = 周日）
    pub first_weekday: u32,
    pub today_energy: f64,
    pub today_minutes: i64,
    pub month_energy: f64,
    pub month_minutes: i64,
    pub year_energy: f64,
    pub year_minutes: i64,
    /// 当月每日 (日, 电量, 分钟)
    pub daily: Vec<(u32, f64, i64)>,
    /// 12 个月的 (月, 电量, 分钟)
    pub months: Vec<(u32, f64, i64)>,
}

pub fn profile_fault_value(profile: &crate::profile::Profile, values: &[(&str, PropValue)]) -> Option<i64> {
    let name = if profile.model == miot::EXPECT_MODEL { "faultValue" } else { "fault" };
    if !profile.properties.contains_key(name) { return None; }
    match values.iter().find(|(key, _)| *key == name)?.1.clone() {
        PropValue::Ok(Value::String(s)) => s.split(',').next()?.trim().parse().ok(),
        PropValue::Ok(value) => value.as_i64(),
        _ => None,
    }
}

pub fn profile_fault_info(profile: &crate::profile::Profile, values: &[(&str, PropValue)]) -> miot::FaultInfo {
    let name = if profile.model == miot::EXPECT_MODEL { "faultValue" } else { "fault" };
    if !profile.properties.contains_key(name) {
        return miot::FaultInfo { clear: false, text: "该型号未提供故障读数".into(), badge: None };
    }
    let value = profile_fault_value(profile, values);
    if profile.model == miot::EXPECT_MODEL { return miot::fault_info(value); }
    miot::FaultInfo { clear: value == Some(0), text: match value {
        Some(0) => "无故障".into(), Some(n) => format!("故障代码 {n}"), None => "故障读数不可用".into()
    }, badge: None }
}

/// 空调控制器。
pub struct Controller {
    pub profile: crate::profile::Profile,
    pub creds: Credentials,
    /// 通道策略
    pub prefer: crate::Transport,
    /// 当前生效链路（None = 还没连上）
    pub link: Option<ActiveLink>,
    device: Option<DeviceInfo>,
    local: Option<MiioDevice>,
    cloud: Option<CloudClient>,
    login_state: Option<crate::credentials::LoginState>,
    /// 代理（云端用）
    pub proxy: Option<String>,
}

impl Controller {
    pub fn new(creds: Credentials, prefer: crate::Transport) -> Self {
        let proxy = std::env::var("MIAC_PROXY").ok().filter(|s| !s.trim().is_empty());
        Self {
            profile: crate::profile::Profile::default(),
            creds,
            prefer,
            link: None,
            device: None,
            local: None,
            cloud: None,
            login_state: None,
            proxy,
        }
    }

    /// 空调设备信息（已识别并保存的）。
    pub fn device(&self) -> Option<&DeviceInfo> {
        self.device.as_ref()
    }

    /// 当前底层连接是否仍绑定到这组设备凭据。
    ///
    /// did、型号、局域网地址或 token 任一变化都必须重建连接；仅名称和
    /// savedAt 变化不影响控制寻址，可以沿用现有通道。
    pub fn is_bound_to(&self, candidate: &DeviceInfo) -> bool {
        fn normalized(value: Option<&str>) -> Option<&str> {
            value.map(str::trim).filter(|value| !value.is_empty())
        }

        self.device.as_ref().is_some_and(|bound| {
            bound.did.trim() == candidate.did.trim()
                && normalized(bound.model.as_deref()) == normalized(candidate.model.as_deref())
                && normalized(bound.localip.as_deref()) == normalized(candidate.localip.as_deref())
                && normalized(bound.token.as_deref()) == normalized(candidate.token.as_deref())
        })
    }

    pub fn thermometer_device(&self) -> Option<ThermometerInfo> {
        self.login_state.clone().or_else(|| self.creds.read_login()).map(|state| state.thermometer)
            .filter(|sensor| !sensor.did.is_empty())
    }

    pub fn has_cloud_session(&self) -> bool {
        self.login_state.as_ref().is_some_and(|state| state.session.ready())
    }

    pub fn credentials_current(&self) -> bool {
        self.login_state.as_ref().zip(self.creds.read_login().as_ref())
            .is_some_and(|(bound, current)| bound == current)
    }

    /// 选择并初始化通道。
    ///
    /// 策略与 v1 `initTransport()` 一致：
    ///   - `prefer = cloud` → 强制云端
    ///   - `prefer = local` → 强制局域网（缺 localip/token 直接报错）
    ///   - `prefer = auto`  → 有 localip+token 走局域网，否则云端
    pub fn init_transport(&mut self) -> Result<ActiveLink, ControllerError> {
        // Credentials may now describe another device or account. A failed
        // reload must not leave an old connection available for later writes.
        self.dispose();
        self.device = None;
        self.profile = crate::profile::Profile::default();
        let state = self.creds.read_login()
            .ok_or_else(|| ControllerError("还没有设备信息，请先登录米家账号完成设备设置。".into()))?;
        let dev = state.device.clone();
        dev.validate().map_err(|e| ControllerError(e.into()))?;
        self.login_state = Some(state);
        self.device = Some(dev.clone());
        self.profile = crate::profile::load(dev.model.as_deref().unwrap_or(""), self.creds.primary_dir(), self.proxy.as_deref())
            .map_err(|e| ControllerError(format!("读取该型号的 MIoT 规格失败：{e}")))?;

        match self.prefer {
            crate::Transport::Cloud => {
                self.use_cloud()?;
            }
            crate::Transport::Local => {
                if !dev.local_ready() {
                    return Err(ControllerError(
                        "已指定局域网通道，但 device.json 缺少 localip 或 token，请重新登录。".into(),
                    ));
                }
                self.init_local(&dev)?;
            }
            crate::Transport::Auto => {
                if dev.local_ready() {
                    self.init_local(&dev)?;
                } else {
                    self.use_cloud()?;
                }
            }
        }

        self.link.ok_or_else(|| ControllerError("控制通道未初始化".into()))
    }

    fn init_local(&mut self, dev: &DeviceInfo) -> Result<(), ControllerError> {
        if self.local.is_none() {
            let host = dev.localip.as_deref().unwrap_or_default();
            let token = dev.token.as_deref().unwrap_or_default();
            // did 必须一起传：报文头里的设备 ID 要用它（用随机值设备不响应）
            self.local = Some(MiioDevice::connect(host, &dev.did, token)?);
        }
        self.link = Some(ActiveLink::Local);
        Ok(())
    }

    fn use_cloud(&mut self) -> Result<(), ControllerError> {
        let session = self.login_state.as_ref().map(|state| state.session.clone())
            .ok_or_else(|| ControllerError("云端会话不可用（cloud-session.json 缺失），请重新登录米家账号。".into()))?;
        if !session.ready() {
            return Err(ControllerError(
                "云端会话不可用（cloud-session.json 不完整），请重新登录米家账号。".into(),
            ));
        }
        self.cloud = Some(CloudClient::new(session, self.proxy.clone()));
        self.link = Some(ActiveLink::Cloud);
        Ok(())
    }

    /// 发起一次 MIoT RPC，按当前链路分发。
    ///
    /// 云端失败时自动降级回局域网（除非用户强制指定云端），与 v1 行为一致。
    pub fn rpc(&mut self, method: &str, params: Value) -> Result<Value, ControllerError> {
        let did = self
            .device
            .as_ref()
            .map(|d| d.did.clone())
            .ok_or_else(|| ControllerError("还没有设备信息".into()))?;

        if self.link == Some(ActiveLink::Local) {
            let local_result = self
                .local
                .as_mut()
                .ok_or_else(|| ControllerError("局域网连接已失效，请重新连接".into()))?
                .rpc(method, params.clone());
            match local_result {
                Ok(value) => return Ok(value),
                Err(local_error) => {
                    // A device-side rejection is a real result, not a broken
                    // transport. Retrying a write through cloud could apply it twice.
                    if matches!(&local_error, crate::miio::MiioError::Device { .. }) {
                        return Err(local_error.into());
                    }
                    // A timeout does not prove that a write was not applied.
                    // Never send the same write again through another link.
                    if !rpc_retry_safe(method) {
                        return Err(ControllerError(format!(
                            "写入结果未确认：{local_error}；请刷新状态后再操作"
                        )));
                    }
                    // 自动模式下，局域网凭据存在不代表设备当前仍在同一个网络。
                    // 本地 UDP 超时后应尝试云端，而不是永久重连同一个失效 IP。
                    if self.prefer != crate::Transport::Local && self.has_cloud_session() {
                        eprintln!("[通道] 局域网失败（{local_error}），自动尝试云端。");
                        if self.use_cloud().is_ok() {
                            self.local = None;
                            let cloud = self.cloud.as_ref().ok_or_else(|| ControllerError("云端连接未就绪".into()))?;
                            match cloud.miio_call(&did, method, &params) {
                                Ok(value) => return Ok(value),
                                Err(cloud_error) => {
                                    return Err(ControllerError(format!(
                                        "局域网失败：{local_error}；云端也失败：{cloud_error}"
                                    )));
                                }
                            }
                        }
                    }
                    return Err(local_error.into());
                }
            }
        }

        let result = {
            let cloud = self
                .cloud
                .as_ref()
                .ok_or_else(|| ControllerError("云端客户端未初始化".into()))?;
            cloud.miio_call(&did, method, &params)
        };

        match result {
            Ok(v) => Ok(v),
            Err(e) => {
                if matches!(&e, cloud::CloudError::Api { code, .. } if *code != -9999) {
                    return Err(ControllerError(e.to_string()));
                }
                if !rpc_retry_safe(method) {
                    return Err(ControllerError(format!(
                        "写入结果未确认：{e}；请刷新状态后再操作"
                    )));
                }
                // 云端失败时，有本地信息就降级，别让用户卡住
                let dev_info = self.device.clone();
                if self.prefer != crate::Transport::Cloud {
                    if let Some(d) = dev_info.filter(|d| d.local_ready()) {
                        eprintln!("[通道] 云端失败（{e}），自动退回局域网直连。");
                        self.local = None;
                        self.init_local(&d)?;
                        self.cloud = None;
                        let local = self.local.as_mut().expect("刚初始化过");
                        return Ok(local.rpc(method, params)?);
                    }
                }
                Err(ControllerError(e.to_string()))
            }
        }
    }

    // ── 属性读写 ────────────────────────────────────────────────

    /// 按属性名查 (siid, piid)。
    pub fn resolve_prop(name: &str) -> Result<(u16, u16), ControllerError> {
        miot::prop_addr(name).ok_or_else(|| ControllerError(format!("未知属性 \"{name}\"")))
    }

    /// 批量读取，返回按请求顺序排列的结果。
    pub fn read_props(&mut self, names: &[&str]) -> Result<Vec<(&'static str, PropValue)>, ControllerError> {
        let did = self.device.as_ref().map(|d| d.did.clone()).unwrap_or_default();

        // 名字要转成 'static 供界面用：这里要求调用方传的属性名都来自属性表
        let mut addrs = Vec::with_capacity(names.len());
        for n in names {
            if let Some(p) = self.profile.properties.get(*n).filter(|p| p.readable) {
                addrs.push((*n, (p.siid, p.piid)));
            }
        }

        let params: Vec<Value> = addrs
            .iter()
            .map(|(_, (siid, piid))| json!({ "did": did, "siid": siid, "piid": piid }))
            .collect();
        let arr = read_property_batches(&params, |batch| self.rpc("get_properties", Value::Array(batch.to_vec())))?;
        Ok(property_read_results(&self.profile, names, &arr))
    }

    /// 读单个属性（原始 siid/piid）。
    pub fn raw_read(&mut self, siid: u16, piid: u16) -> Result<PropValue, ControllerError> {
        let did = self.device.as_ref().map(|d| d.did.clone()).unwrap_or_default();
        let res = self.rpc(
            "get_properties",
            json!([{ "did": did, "siid": siid, "piid": piid }]),
        )?;
        Ok(parse_first(res))
    }

    /// 写单个属性（原始 siid/piid）。
    pub fn raw_write(
        &mut self,
        siid: u16,
        piid: u16,
        value: Value,
    ) -> Result<PropValue, ControllerError> {
        if self.login_state.is_some() && !self.credentials_current() {
            return Err(ControllerError("设备或账号凭据已变化，请重新连接后操作".into()));
        }
        let did = self.device.as_ref().map(|d| d.did.clone()).unwrap_or_default();
        let res = self.rpc(
            "set_properties",
            json!([{ "did": did, "siid": siid, "piid": piid, "value": value }]),
        )?;
        Ok(parse_first(res))
    }

    /// 按属性名写值。
    pub fn write_prop(&mut self, name: &str, value: Value) -> Result<(), ControllerError> {
        let value = self.profile.validate(name, &value).map_err(ControllerError)?;
        let (siid, piid) = self.profile.addr(name).ok_or_else(|| ControllerError(format!("该型号不支持 {name}")))?;
        let r = self.raw_write(siid, piid, value.clone())?;
        match r {
            PropValue::Ok(_) => Ok(()),
            PropValue::Err(code) => Err(ControllerError(format!(
                "下发失败：{name}={value} → err({code})"
            ))),
            PropValue::Missing => Err(ControllerError(format!(
                "下发失败：{name}={value} → 设备未返回结果"
            ))),
        }
    }

    // ── 校验（与 v1 的 valid* 系列一致）────────────────────────

    /// 温度必须是 16~31、步长 0.5。
    #[deprecated(note = "仅适用于旧 h53h00 规则；真实设备必须使用 profile.validate")]
    pub fn valid_temp(t: f64) -> Result<f64, ControllerError> {
        if !t.is_finite() || !(16.0..=31.0).contains(&t) || ((t * 2.0) - (t * 2.0).round()).abs() > 1e-9
        {
            return Err(ControllerError(format!(
                "温度需为 16~31 之间、以 0.5 ℃ 为步长的数值，收到 \"{t}\""
            )));
        }
        Ok(t)
    }

    /// 模式名/值 → 模式值。
    #[deprecated(note = "仅适用于旧 h53h00 映射；真实设备必须按 profile 选择模式")]
    pub fn valid_mode(m: &Value) -> Result<i64, ControllerError> {
        let v = match m {
            Value::Number(n) => n.as_i64(),
            Value::String(s) => miot::MODE_VALUE
                .iter()
                .find(|(k, _)| k.eq_ignore_ascii_case(s))
                .map(|(_, v)| *v),
            _ => None,
        };
        match v {
            Some(v) if miot::MODE_NAME.iter().any(|(k, _)| *k == v) => Ok(v),
            _ => Err(ControllerError("模式应为 cool / dry / fan / heat".into())),
        }
    }

    /// 风速：auto / 1~7 / max。
    #[deprecated(note = "仅适用于旧 h53h00 映射；真实设备必须按 profile 选择风速")]
    pub fn valid_fan(level: &Value) -> Result<i64, ControllerError> {
        match level {
            Value::String(s) if s == "auto" => return Ok(0),
            Value::String(s) if s == "max" => return Ok(8),
            _ => {}
        }
        match level.as_i64() {
            Some(n) if (0..=8).contains(&n) => Ok(n),
            _ => Err(ControllerError("风速应为 auto / 1~7 / max".into())),
        }
    }

    /// 风感：off / up / down / circle / noblow（也接受 0~4）。
    pub fn valid_wind(direction: &Value) -> Result<i64, ControllerError> {
        if let Value::String(s) = direction {
            if let Some((_, v)) = miot::WIND_VALUE.iter().find(|(k, _)| k == s) {
                return Ok(*v);
            }
        }
        if let Some(n) = direction.as_i64() {
            if (0..=4).contains(&n) {
                return Ok(n);
            }
        }
        Err(ControllerError("风感应为 off / up / down / circle / noblow".into()))
    }

    /// 定格位置：0~5 整数。
    pub fn valid_pos(p: &Value) -> Result<i64, ControllerError> {
        match p.as_i64() {
            Some(n) if (0..=5).contains(&n) => Ok(n),
            _ => Err(ControllerError("位置需为 0~5 的整数".into())),
        }
    }

    // ── 语义操作 ────────────────────────────────────────────────

    pub fn set_power(&mut self, on: bool) -> Result<(), ControllerError> {
        self.write_prop("on", json!(on))
    }

    pub fn set_temp(&mut self, t: f64) -> Result<(), ControllerError> {
        self.write_prop("targetTemp", json!(t))
    }

    pub fn set_mode(&mut self, m: &Value) -> Result<(), ControllerError> {
        let value = if let Some(s) = m.as_str() {
            let label = match s { "cool" => "制冷", "dry" => "除湿", "fan" => "送风", "heat" => "制热", "auto" => "自动", _ => s };
            let v = self.profile.properties.get("mode").and_then(|p| p.choices.iter().find(|(_,n)| n == label)).ok_or_else(|| ControllerError("该型号不支持所选模式".into()))?.0;
            json!(v)
        } else { m.clone() };
        self.write_prop("mode", value)
    }

    pub fn set_fan(&mut self, level: &Value) -> Result<(), ControllerError> {
        let choices = self.profile.properties.get("fanLevel").map(|p| &p.choices);
        let value = match level.as_str() {
            Some("auto") => choices.and_then(|c| c.iter().find(|(_,n)| n == "自动")).map(|(v,_)| json!(v)),
            Some("max") => choices.and_then(|c| c.iter().max_by_key(|(v,_)| v)).map(|(v,_)| json!(v)),
            _ => Some(level.clone()),
        }.ok_or_else(|| ControllerError("该型号不支持该风速".into()))?;
        self.write_prop("fanLevel", value)
    }

    pub fn set_wind(&mut self, d: &Value) -> Result<(), ControllerError> {
        let v = Self::valid_wind(d)?;
        self.write_prop("windDirection", json!(v))
    }

    pub fn set_vertical_pos(&mut self, p: &Value) -> Result<(), ControllerError> {
        let v = Self::valid_pos(p)?;
        self.write_prop("verticalPos", json!(v))
    }

    pub fn set_horizontal_pos(&mut self, p: &Value) -> Result<(), ControllerError> {
        let v = Self::valid_pos(p)?;
        self.write_prop("horizontalPos", json!(v))
    }

    pub fn set_bright(&mut self, level: &Value) -> Result<(), ControllerError> {
        let v = match level {
            Value::String(s) => miot::BRIGHT_VALUE
                .iter()
                .find(|(k, _)| k == s)
                .map(|(_, v)| *v),
            other => other.as_i64(),
        };
        match v {
            Some(v) if (0..=2).contains(&v) => self.write_prop("lightBright", json!(v)),
            _ => Err(ControllerError("亮度应为 auto / mid / high（0~2）".into())),
        }
    }

    pub fn set_effect(&mut self, which: &str, level: &Value) -> Result<(), ControllerError> {
        let v = match level {
            Value::String(s) => miot::EFFECT_VALUE
                .iter()
                .find(|(k, _)| k == s)
                .map(|(_, v)| *v),
            other => other.as_i64(),
        };
        let v = match v {
            Some(v) if (1..=5).contains(&v) => v,
            _ => return Err(ControllerError("效果档位需为 1~5（weak…strong）".into())),
        };
        match which {
            "cool" => self.write_prop("coolingEffect", json!(v)),
            "heat" => self.write_prop("heatingEffect", json!(v)),
            _ => Err(ControllerError("效果类型应为 cool 或 heat".into())),
        }
    }

    /// 布尔类属性的开与关。
    pub fn set_toggle(&mut self, name: &str, on: bool) -> Result<(), ControllerError> {
        if miot::prop_addr(name).is_none() {
            return Err(ControllerError(format!("未知属性 \"{name}\"")));
        }
        self.write_prop(name, json!(on))
    }

    /// 应用温度预设。
    ///
    /// 关机时设备会以 `-5000` 拒绝温度写入；先开机并短暂等待后再重试。
    /// 这个「等压缩机保护延时」的行为与 v1 `applyTempPreset()` 一致。
    pub fn apply_temp_preset(&mut self, value: f64) -> Result<(bool, f64), ControllerError> {
        let t = self.profile.snap_temperature(value);
        self.profile.validate("targetTemp", &json!(t)).map_err(ControllerError)?;
        let state = self.read_props(&["on"])?;
        let turned_on = !confirmed_power_state(&state)?;

        if turned_on {
            self.set_power(true)?;
        }

        // 设备刚开机、或此前的开机指令尚在生效时，都会短暂拒绝温度写入。
        // 即使本次调用读到 on=true，也给 -5000 相同的重试窗口。
        let attempts = 6;
        let mut last_err = None;
        for attempt in 1..=attempts {
            if turned_on || attempt > 1 {
                std::thread::sleep(Duration::from_millis(500));
            }
            match self.set_temp(t) {
                Ok(()) => return Ok((turned_on, t)),
                Err(e) => {
                    // 刚开机时压缩机有保护延时，设备可能还没接受温度写入
                    let may_still_start = e.0.contains("-5000");
                    if !may_still_start || attempt == attempts {
                        last_err = Some(e);
                        break;
                    }
                    last_err = Some(e);
                }
            }
        }
        Err(last_err.unwrap_or_else(|| ControllerError("应用温度预设失败".into())))
    }

    // ── 数据读取 ────────────────────────────────────────────────

    /// 状态快照：空调状态 + 故障解读。
    pub fn snapshot(&mut self) -> Result<Snapshot, ControllerError> {
        let mut names = snapshot_property_names(&self.profile);
        // Read the safety-critical switch and temperature first. A vendor-only
        // diagnostic or energy property must not hide the actual power state.
        let mut status = self.read_props(&["on", "targetTemp"])?;
        confirmed_power_state(&status)?;
        names.retain(|name| !["on", "targetTemp"].contains(name));
        match self.read_props(&names) {
            Ok(extra) => status.extend(extra),
            Err(error) => {
                eprintln!("[状态] 可选属性读取失败：{error}");
                for name in names {
                    if let Some((known, _)) = miot::PROPS.iter().find(|(known, _)| *known == name) {
                        status.push((*known, PropValue::Missing));
                    }
                }
            }
        }
        Ok(Snapshot {
            link: self.link,
            fault: profile_fault_info(&self.profile, &status),
            status,
        })
    }

    /// 机器诊断。
    pub fn diag(&mut self) -> Result<Vec<(&'static str, PropValue)>, ControllerError> {
        self.read_props(miot::DIAG_PROPS)
    }

    /// 自清洁 / 维护状态。
    pub fn maintenance(&mut self) -> Result<(Vec<(&'static str, PropValue)>, bool), ControllerError> {
        let values = self.read_props(&["clean", "examine", "error", "runDuration"])?;
        // clean 形如 "1,..."，首位为 1 表示自清洁运行中
        let running = values
            .iter()
            .find(|(n, _)| *n == "clean")
            .and_then(|(_, v)| match v {
                PropValue::Ok(Value::String(s)) => Some(s.split(',').next() == Some("1")),
                PropValue::Ok(other) => other.as_i64().map(|n| n == 1),
                _ => None,
            })
            .unwrap_or(false);
        Ok((values, running))
    }

    /// 温湿度计读数。
    ///
    /// 温湿度计 3 是蓝牙设备，没有可直接访问的局域网 IP；数据经蓝牙网关同步到
    /// 米家云后读取。失败时返回可显示的状态，不影响空调本地控制。
    pub fn read_thermometer(&mut self) -> ThermometerReading {
        let state = self.login_state.clone().or_else(|| self.creds.read_login());
        let Some(dev) = state.as_ref().map(|s| s.thermometer.clone()).filter(|t| !t.did.is_empty()) else {
            return ThermometerReading::unavailable("未配置");
        };
        if dev.did.trim().is_empty() {
            return ThermometerReading::unavailable("未配置");
        }
        let Some(session) = state.map(|s| s.session).filter(|s| s.ready()) else {
            return ThermometerReading::unavailable("需要登录米家云");
        };

        let cloud = CloudClient::new(session, self.proxy.clone());
        let name = dev.name.clone().unwrap_or_else(|| "温湿度计".into());

        // 路线 1：MIoT 直读
        match cloud.prop_get(&dev.did, &miot::THERMOMETER_PROPS.map_addrs()) {
            Ok(items) => {
                let mut temp = None;
                let mut hum = None;
                let mut battery = None;
                let now = unix_now();
                for item in &items {
                    let siid = item.get("siid").and_then(Value::as_i64).unwrap_or(-1);
                    let piid = item.get("piid").and_then(Value::as_i64).unwrap_or(-1);
                    let code = item.get("code").and_then(Value::as_i64).unwrap_or(-1);
                    if code != 0 {
                        continue;
                    }
                    if item.get("updateTime").and_then(Value::as_i64)
                        .is_some_and(|time| !recent_sensor_time(time, now)) { continue; }
                    let v = item.get("value").and_then(Value::as_f64);
                    match (siid, piid) {
                        (3, 1001) => temp = v,
                        (3, 1002) => hum = v,
                        (2, 1003) => battery = v,
                        _ => {}
                    }
                }
                if let (Some(t), Some(h)) = (temp, hum) {
                    return ThermometerReading {
                        available: true,
                        name,
                        temperature: Some(t),
                        humidity: Some(h),
                        battery,
                        reason: None,
                    };
                }
            }
            Err(e) if e.is_session_expired() => {
                return ThermometerReading::unavailable(e.to_string());
            }
            Err(_) => { /* 回退到历史上报 */ }
        }

        // 路线 2：历史上报（少数旧版蓝牙网关不支持 MIoT 直读）
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);

        let read_last = |key: &str| -> Result<Option<Value>, cloud::CloudError> {
            cloud
                .device_data(&dev.did, key, now - 600, now + 60, 1000)
                .map(|records| latest_sensor_record(records, now))
        };

        let mut error = None;
        let mut read_record = |key| match read_last(key) {
            Ok(record) => record,
            Err(e) => { error = Some(e); None }
        };
        let temp_rec = read_record(miot::THERMOMETER_HISTORY_KEY_TEMPERATURE);
        let hum_rec = read_record(miot::THERMOMETER_HISTORY_KEY_HUMIDITY);

        let temp = temp_rec.as_ref().and_then(|r| cloud::record_number(r, "temperature"));
        let hum = hum_rec.as_ref().and_then(|r| cloud::record_number(r, "humidity"));

        match (temp, hum) {
            (Some(t), Some(h)) => ThermometerReading {
                available: true,
                name,
                temperature: Some(t),
                humidity: Some(h),
                battery: None,
                reason: None,
            },
            _ => ThermometerReading::unavailable(error.map(|e| e.to_string())
                .unwrap_or_else(|| "云端暂无有效温湿度数据".into())),
        }
    }

    /// 耗电日历：20.1 是用电量(kWh)，8.5 是当日开机时长(分钟)。
    pub fn power_stats(&mut self) -> Result<PowerStats, ControllerError> {
        // History aggregation and units are vendor specific, not a standard MIoT contract.
        if self.profile.model != miot::EXPECT_MODEL {
            return Err(ControllerError("该型号暂不支持历史电量统计；实时读数见控制台".into()));
        }
        let dev = self
            .device
            .clone()
            .ok_or_else(|| ControllerError("还没有设备信息".into()))?;
        let session = self
            .creds
            .read_session()
            .filter(|s| s.ready())
            .ok_or_else(|| ControllerError("需要米家云会话，请重新登录米家账号".into()))?;
        let cloud = CloudClient::new(session, self.proxy.clone());

        let (year, month, day, days_in_month, first_weekday) = today_parts();
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);
        let start_of_year = start_of_local_year_unix(year).unwrap_or(now);

        let energy = cloud
            .statistics(&dev.did, "20.1", start_of_year, now + 60, 400)
            .map_err(|e| ControllerError(e.to_string()))?;
        let runtime = cloud
            .statistics(&dev.did, "8.5", start_of_year, now + 60, 400)
            .map_err(|e| ControllerError(e.to_string()))?;

        // 按「年-月-日」聚合
        use std::collections::BTreeMap;
        let mut daily: BTreeMap<(i32, u32, u32), (f64, i64)> = BTreeMap::new();
        let mut merge = |records: &[Value], is_energy: bool| {
            for r in records {
                let Some(ts) = r.get("time").and_then(Value::as_i64) else { continue };
                let Some(v) = cloud::stat_value(r) else { continue };
                let (y, m, d) = ymd_from_unix(ts);
                let e = daily.entry((y, m, d)).or_insert((0.0, 0));
                if is_energy {
                    e.0 = v;
                } else {
                    e.1 = v as i64;
                }
            }
        };
        merge(&energy, true);
        merge(&runtime, false);

        let mut daily_rows: Vec<(u32, f64, i64)> = daily
            .iter()
            .filter(|((y, m, _), _)| *y == year && *m == month)
            .map(|((_, _, d), (e, mins))| (*d, *e, *mins))
            .collect();
        daily_rows.sort_by_key(|(d, _, _)| *d);

        let sum_in = |pred: &dyn Fn(i32, u32) -> bool| -> (f64, i64) {
            daily
                .iter()
                .filter(|((y, m, _), _)| pred(*y, *m))
                .fold((0.0, 0), |acc, (_, (e, mins))| (acc.0 + e, acc.1 + mins))
        };

        let month_totals = sum_in(&|y, m| y == year && m == month);
        let year_totals = sum_in(&|y, _| y == year);
        let today = daily.get(&(year, month, day)).copied().unwrap_or((0.0, 0));

        let months: Vec<(u32, f64, i64)> = (1..=12)
            .map(|m| {
                let (e, mins) = sum_in(&|y, mm| y == year && mm == m);
                (m, e, mins)
            })
            .collect();

        Ok(PowerStats {
            year,
            month,
            day,
            days_in_month,
            first_weekday,
            today_energy: today.0,
            today_minutes: today.1,
            month_energy: month_totals.0,
            month_minutes: month_totals.1,
            year_energy: year_totals.0,
            year_minutes: year_totals.1,
            daily: daily_rows,
            months,
        })
    }

    /// 释放资源。
    pub fn dispose(&mut self) {
        self.local = None;
        self.cloud = None;
        self.link = None;
    }
}

/// `THERMOMETER_PROPS` 的 (siid, piid) 列表（供 prop_get 用）。
trait ThermometerAddrs {
    fn map_addrs(&self) -> Vec<(u16, u16)>;
}

impl ThermometerAddrs for [(&str, (u16, u16))] {
    fn map_addrs(&self) -> Vec<(u16, u16)> {
        self.iter().map(|(_, a)| *a).collect()
    }
}

// A write may have reached the device even when its acknowledgement is lost.
// Only idempotent reads may be attempted through a fallback link.
fn unix_now() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64).unwrap_or(0)
}

fn recent_sensor_time(mut time: i64, now: i64) -> bool {
    if time > 1_000_000_000_000 { time /= 1000; }
    time >= now - 600 && time <= now + 60
}

fn latest_sensor_record(records: Vec<Value>, now: i64) -> Option<Value> {
    records.into_iter().filter(|r| r.get("time").and_then(Value::as_i64)
        .is_some_and(|time| recent_sensor_time(time, now)))
        .max_by_key(|r| r.get("time").and_then(Value::as_i64).unwrap_or(0))
}

fn rpc_retry_safe(method: &str) -> bool {
    method == "get_properties"
}

fn confirmed_power_state(status: &[(&str, PropValue)]) -> Result<bool, ControllerError> {
    status
        .iter()
        .find(|(name, _)| *name == "on")
        .and_then(|(_, value)| value.as_bool())
        .ok_or_else(|| ControllerError("未能读取空调开关状态，请检查设备连接或型号规格".into()))
}

fn snapshot_property_names(profile: &crate::profile::Profile) -> Vec<&'static str> {
    let mut names = miot::STATUS_PROPS.to_vec();
    // These controls are rendered from snapshot state too. Without their
    // readback, toggles stay false and selected numeric values stay at zero.
    for name in ["windSensation", "light", "buzzer", "verticalPos", "horizontalPos", "lightBright", "coolingEffect", "heatingEffect"] {
        if profile.properties.get(name).is_some_and(|prop| prop.readable) {
            names.push(name);
        }
    }
    if profile.model != miot::EXPECT_MODEL { names.push("fault"); }
    names
}

fn property_read_results(
    profile: &crate::profile::Profile,
    names: &[&str],
    results: &[Value],
) -> Vec<(&'static str, PropValue)> {
    let mut out = Vec::with_capacity(names.len());
    for name in names {
        let Some(&(static_name, _)) = miot::PROPS.iter().find(|(known, _)| known == name) else { continue };
        // Unsupported properties occupy their requested slot too; otherwise
        // the result order shifts and repeated missing properties disappear.
        let item = profile.properties.get(*name).filter(|prop| prop.readable)
            .and_then(|prop| results.iter().find(|item| item["siid"].as_u64() == Some(prop.siid as u64)
                && item["piid"].as_u64() == Some(prop.piid as u64)));
        let value = match item.and_then(|item| item.get("code").and_then(Value::as_i64)) {
            Some(0) => PropValue::Ok(item.and_then(|item| item.get("value")).cloned().unwrap_or(Value::Null)),
            Some(code) => PropValue::Err(code),
            None => PropValue::Missing,
        };
        out.push((static_name, value));
    }
    out
}

// Keep packets small: MIoT firmware can time out on oversized property lists.
fn read_property_batches(
    params: &[Value],
    mut read: impl FnMut(&[Value]) -> Result<Value, ControllerError>,
) -> Result<Vec<Value>, ControllerError> {
    fn append(values: &mut Vec<Value>, result: Value) {
        match result {
            Value::Array(items) => values.extend(items),
            other => values.push(other),
        }
    }
    fn recover(
        batch: &[Value],
        read: &mut impl FnMut(&[Value]) -> Result<Value, ControllerError>,
        values: &mut Vec<Value>,
    ) -> Result<bool, ControllerError> {
        if batch.len() < 2 { return Ok(false); }
        let (left, right) = batch.split_at(batch.len() / 2);
        let mut any_success = false;
        for half in [left, right] {
            match read(half) {
                Ok(result) => { append(values, result); any_success = true; }
                Err(error) => {
                    // Authentication expiry applies to the whole request,
                    // including retries for an otherwise optional property.
                    if error.requires_cloud_login() { return Err(error); }
                    any_success |= recover(half, read, values)?;
                }
            }
        }
        Ok(any_success)
    }
    let mut values = Vec::new();
    let mut first_error = None;
    let mut any_success = false;
    for batch in params.chunks(8) {
        match read(batch) {
            Ok(result) => { append(&mut values, result); any_success = true; }
            Err(error) => {
                if error.requires_cloud_login() { return Err(error); }
                if first_error.is_none() { first_error = Some(error); }
                any_success |= recover(batch, &mut read, &mut values)?;
            }
        }
    }
    if any_success || params.is_empty() { Ok(values) } else { Err(first_error.expect("nonempty batch failed")) }
}

fn parse_first(res: Value) -> PropValue {
    let item = match res {
        Value::Array(mut a) if !a.is_empty() => a.remove(0),
        other => other,
    };
    match item.get("code").and_then(Value::as_i64) {
        Some(0) => PropValue::Ok(item.get("value").cloned().unwrap_or(Value::Null)),
        Some(code) => PropValue::Err(code),
        None => PropValue::Missing,
    }
}

// ── 日期换算（不引 chrono，够用且省内存）──────────────────────

/// 今天的 (年, 月, 日, 当月天数, 当月 1 号星期几)。
pub fn today_parts() -> (i32, u32, u32, u32, u32) {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    let (y, m, d) = ymd_from_unix(secs);
    (y, m, d, days_in_month(y, m), first_weekday_of_month(y, m))
}

/// Unix 秒 → (年, 月, 日)，按本机时区（非 Windows 平台退化为 UTC）。
pub fn ymd_from_unix(secs: i64) -> (i32, u32, u32) {
    #[cfg(windows)]
    if let Some(parts) = local_ymd_from_unix(secs) {
        return parts;
    }
    ymd_from_unix_utc(secs)
}

/// Unix 秒 → (年, 月, 日)，按 UTC。公开的 `ymd_from_unix` 在 Windows 上应使用本地日历。
pub(crate) fn ymd_from_unix_utc(secs: i64) -> (i32, u32, u32) {
    let days = secs.div_euclid(86400);
    // 民用历算法：以 1970-01-01 为起点
    let z = days + 719468;
    let era = z.div_euclid(146097);
    let doe = z.rem_euclid(146097);
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let y = if m <= 2 { y + 1 } else { y };
    (y as i32, m, d)
}

#[cfg(windows)]
fn local_ymd_from_unix(secs: i64) -> Option<(i32, u32, u32)> {
    use windows_sys::Win32::Foundation::{FILETIME, SYSTEMTIME};
    use windows_sys::Win32::System::Time::{
        FileTimeToSystemTime, SystemTimeToTzSpecificLocalTime,
    };

    let ticks = (secs as i128 + 11_644_473_600).checked_mul(10_000_000)?;
    if ticks < 0 || ticks > u64::MAX as i128 {
        return None;
    }
    let raw = ticks as u64;
    let file_time = FILETIME { dwLowDateTime: raw as u32, dwHighDateTime: (raw >> 32) as u32 };
    unsafe {
        let mut utc: SYSTEMTIME = std::mem::zeroed();
        let mut local: SYSTEMTIME = std::mem::zeroed();
        if FileTimeToSystemTime(&file_time, &mut utc) == 0
            || SystemTimeToTzSpecificLocalTime(std::ptr::null(), &utc, &mut local) == 0
        {
            return None;
        }
        Some((local.wYear as i32, local.wMonth as u32, local.wDay as u32))
    }
}

#[cfg(windows)]
fn start_of_local_year_unix(year: i32) -> Option<i64> {
    use windows_sys::Win32::Foundation::{FILETIME, SYSTEMTIME};
    use windows_sys::Win32::System::Time::{
        SystemTimeToFileTime, TzSpecificLocalTimeToSystemTime,
    };

    unsafe {
        let local = SYSTEMTIME {
            wYear: year as u16,
            wMonth: 1,
            wDay: 1,
            ..std::mem::zeroed()
        };
        let mut utc: SYSTEMTIME = std::mem::zeroed();
        if TzSpecificLocalTimeToSystemTime(std::ptr::null(), &local, &mut utc) == 0 {
            return None;
        }
        let mut file_time: FILETIME = std::mem::zeroed();
        if SystemTimeToFileTime(&utc, &mut file_time) == 0 {
            return None;
        }
        let ticks = ((file_time.dwHighDateTime as u64) << 32) | file_time.dwLowDateTime as u64;
        Some((ticks / 10_000_000) as i64 - 11_644_473_600)
    }
}

#[cfg(not(windows))]
fn start_of_local_year_unix(year: i32) -> Option<i64> {
    // 非 Windows 构建没有引入时区库；保留 UTC 兜底以确保跨平台可编译。
    let mut days = 0i64;
    for y in 1970..year {
        days += if is_leap(y) { 366 } else { 365 };
    }
    Some(days * 86400)
}

pub fn is_leap(y: i32) -> bool {
    (y % 4 == 0 && y % 100 != 0) || y % 400 == 0
}

pub fn days_in_month(y: i32, m: u32) -> u32 {
    match m {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 => {
            if is_leap(y) {
                29
            } else {
                28
            }
        }
        _ => 30,
    }
}

/// 当月 1 号是星期几（0 = 周日）。
pub fn first_weekday_of_month(y: i32, m: u32) -> u32 {
    // 用「1970-01-01 是星期四」推算
    let mut days: i64 = 0;
    for yy in 1970..y {
        days += if is_leap(yy) { 366 } else { 365 };
    }
    for mm in 1..m {
        days += days_in_month(y, mm) as i64;
    }
    // 1970-01-01 = 周四 = 4
    ((days + 4).rem_euclid(7)) as u32
}

#[cfg(test)]
#[allow(deprecated)]
mod tests {
    use super::*;

    #[test]
    fn historical_sensor_values_select_the_newest_recent_record() {
        let now = 1_800_000_000;
        let records = vec![json!({"time":now-20,"value":"old"}), json!({"time":now-2,"value":"new"}), json!({"time":now-10,"value":"middle"})];
        assert_eq!(latest_sensor_record(records, now).unwrap()["value"], "new");
        assert!(latest_sensor_record(vec![json!({"time":now-86400,"value":"stale"}), json!({"time":now+3600})], now).is_none());
    }

    fn isolated_test_dir(label: &str) -> std::path::PathBuf {
        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
        let directory = std::env::temp_dir().join(format!("miac-{label}-{}-{unique}", std::process::id()));
        std::fs::create_dir(&directory).unwrap();
        directory
    }

    #[test]
    fn failed_reinitialization_clears_the_previous_device_and_link() {
        let directory = isolated_test_dir("controller-reinit-empty");
        let mut controller = Controller::new(Credentials::isolated(&directory), crate::Transport::Local);
        controller.device = Some(DeviceInfo { did: "1".into(), ..Default::default() });
        controller.profile = crate::profile::Profile::legacy();
        controller.link = Some(ActiveLink::Local);

        assert!(controller.init_transport().is_err());
        assert!(controller.link.is_none(), "failed initialization must invalidate the old link");
        assert!(controller.device().is_none(), "failed initialization must invalidate the old device");
        assert!(controller.profile.properties.is_empty());
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn reinitialization_connects_to_the_new_device_address() {
        let directory = isolated_test_dir("controller-reinit-device");
        let token = "00000000000000000000000000000000";
        let new_device = DeviceInfo {
            did: "2".into(), model: Some(miot::EXPECT_MODEL.into()),
            token: Some(token.into()), localip: Some("127.0.0.2".into()), ..Default::default()
        };
        std::fs::write(directory.join(crate::credentials::FILE_DEVICE), serde_json::to_vec(&new_device).unwrap()).unwrap();
        let mut controller = Controller::new(Credentials::isolated(&directory), crate::Transport::Local);
        // Constructing a connection only binds a socket; this test sends no packets.
        controller.local = Some(MiioDevice::connect("127.0.0.1", "1", token).unwrap());
        controller.link = Some(ActiveLink::Local);

        assert_eq!(controller.init_transport().unwrap(), ActiveLink::Local);
        assert_eq!(controller.device().unwrap().did, "2");
        assert_eq!(controller.local.as_ref().unwrap().addr().ip().to_string(), "127.0.0.2");
        assert!(!controller.local.as_ref().unwrap().is_handshaked());
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn property_batch_recovery_propagates_expired_session_without_more_reads() {
        let params = vec![json!({"siid": 2, "piid": 1}), json!({"siid": 2, "piid": 2})];
        let mut calls = 0;
        let result = read_property_batches(&params, |batch| {
            calls += 1;
            if batch.len() == 2 { return Err(ControllerError("batch unavailable".into())); }
            if batch[0]["piid"] == 1 { return Err(cloud::CloudError::SessionExpired.into()); }
            Ok(json!([{"siid": 2, "piid": 2, "code": 0, "value": 2}]))
        });

        assert!(result.as_ref().is_err_and(ControllerError::requires_cloud_login));
        assert_eq!(calls, 2, "stop after the first expired-session response");
    }

    #[test]
    fn mixed_supported_property_reads_preserve_request_order_and_duplicates() {
        let mut profile = crate::profile::Profile::legacy();
        profile.properties.remove("mode");
        let results = vec![
            json!({"siid": 2, "piid": 4, "code": 0, "value": 26.5}),
            json!({"siid": 2, "piid": 1, "code": 0, "value": true}),
        ];
        assert_eq!(property_read_results(&profile, &["mode", "on", "mode", "targetTemp"], &results), vec![
            ("mode", PropValue::Missing), ("on", PropValue::Ok(json!(true))),
            ("mode", PropValue::Missing), ("targetTemp", PropValue::Ok(json!(26.5))),
        ]);
    }

    #[test]
    fn snapshots_include_all_visible_control_values_in_bounded_reads() {
        let profile = crate::profile::Profile::legacy();
        let controls = [
            ("windSensation", json!(true)), ("light", json!(true)), ("buzzer", json!(true)),
            ("verticalPos", json!(4)), ("horizontalPos", json!(5)), ("lightBright", json!(2)),
            ("coolingEffect", json!(4)), ("heatingEffect", json!(5)),
        ];
        let names = snapshot_property_names(&profile);
        let params: Vec<_> = names.iter().filter_map(|name| profile.properties.get(*name)
            .filter(|prop| prop.readable)
            .map(|prop| json!({"siid": prop.siid, "piid": prop.piid}))).collect();
        let mut sizes = Vec::new();
        let results = read_property_batches(&params, |batch| {
            sizes.push(batch.len());
            Ok(Value::Array(batch.iter().rev().map(|request| {
                let value = controls.iter().find(|(name, _)| {
                    let prop = &profile.properties[*name];
                    request["siid"] == prop.siid && request["piid"] == prop.piid
                }).map(|(_, value)| value.clone()).unwrap_or(Value::Null);
                json!({"siid": request["siid"], "piid": request["piid"], "code": 0, "value": value})
            }).collect()))
        }).unwrap();
        let status = property_read_results(&profile, &names, &results);
        for (name, expected) in controls {
            assert_eq!(status.iter().find(|(known, _)| *known == name).map(|(_, value)| value),
                Some(&PropValue::Ok(expected)), "missing snapshot control {name}");
        }
        assert!(sizes.iter().all(|size| *size <= 8));
    }

    #[test]
    fn snapshots_add_only_supported_readable_control_properties() {
        let mut profile = crate::profile::Profile::legacy();
        profile.properties.remove("light");
        profile.properties.get_mut("buzzer").unwrap().readable = false;
        profile.properties.get_mut("windSensation").unwrap().writable = false;

        let names = snapshot_property_names(&profile);
        assert!(!names.contains(&"light"), "unsupported controls must not be requested");
        assert!(!names.contains(&"buzzer"), "write-only controls must not be requested");
        assert!(names.contains(&"windSensation"), "read-only control state must still be displayed");

        let profile = crate::profile::Profile::parse("xiaomi.aircondition.ma7",
            &crate::profile::bundled_specs()["xiaomi.aircondition.ma7"]).unwrap();
        let names = snapshot_property_names(&profile);
        for name in ["windSensation", "light", "buzzer", "verticalPos", "horizontalPos", "lightBright", "coolingEffect", "heatingEffect"] {
            assert_eq!(names.contains(&name), profile.properties.get(name).is_some_and(|prop| prop.readable), "{name}");
        }
    }

    #[test]
    fn date_conversion_matches_known_dates() {
        // 1970-01-01
        assert_eq!(ymd_from_unix(0), (1970, 1, 1));
        // 2026-09-19（本次开发日期）
        let ts = 1789000000; // 2026-09-11 附近，只验证年月合理性
        let (y, m, d) = ymd_from_unix(ts);
        assert_eq!(y, 2026);
        assert!((1..=12).contains(&m));
        assert!((1..=31).contains(&d));
    }

    #[test]
    fn month_geometry() {
        assert_eq!(days_in_month(2026, 2), 28);
        assert_eq!(days_in_month(2024, 2), 29);
        assert_eq!(days_in_month(2000, 2), 29);
        assert_eq!(days_in_month(1900, 2), 28);
        assert_eq!(days_in_month(2026, 9), 30);
        // 2026-09-01 是星期二
        assert_eq!(first_weekday_of_month(2026, 9), 2);
        // 1970-01-01 是星期四
        assert_eq!(first_weekday_of_month(1970, 1), 4);
    }

    #[test]
    fn temperature_validation() {
        assert_eq!(Controller::valid_temp(26.0).unwrap(), 26.0);
        assert_eq!(Controller::valid_temp(26.5).unwrap(), 26.5);
        assert!(Controller::valid_temp(15.5).is_err());
        assert!(Controller::valid_temp(31.5).is_err());
        assert!(Controller::valid_temp(26.3).is_err(), "步长必须是 0.5");
    }

    #[test]
    fn mode_and_fan_validation() {
        assert_eq!(Controller::valid_mode(&json!("cool")).unwrap(), 2);
        assert_eq!(Controller::valid_mode(&json!("heat")).unwrap(), 5);
        assert_eq!(Controller::valid_mode(&json!(3)).unwrap(), 3);
        assert!(Controller::valid_mode(&json!("turbo")).is_err());

        assert_eq!(Controller::valid_fan(&json!("auto")).unwrap(), 0);
        assert_eq!(Controller::valid_fan(&json!("max")).unwrap(), 8);
        assert_eq!(Controller::valid_fan(&json!(7)).unwrap(), 7);
        assert!(Controller::valid_fan(&json!(9)).is_err());
    }

    #[test]
    fn wind_and_position_validation() {
        assert_eq!(Controller::valid_wind(&json!("noblow")).unwrap(), 4);
        assert_eq!(Controller::valid_wind(&json!(2)).unwrap(), 2);
        assert!(Controller::valid_wind(&json!("sideways")).is_err());

        assert_eq!(Controller::valid_pos(&json!(3)).unwrap(), 3);
        assert!(Controller::valid_pos(&json!(6)).is_err());
        assert!(Controller::valid_pos(&json!(-1)).is_err());
    }

    #[test]
    fn prop_value_display_matches_v1_style() {
        assert_eq!(PropValue::Ok(json!(true)).display(), "true");
        assert_eq!(PropValue::Ok(json!(26.5)).display(), "26.5");
        assert_eq!(PropValue::Err(-5000).display(), "err(-5000)");
        assert_eq!(PropValue::Missing.display(), "—");
    }

    #[test]
    fn resolve_prop_rejects_unknown_names() {
        assert_eq!(Controller::resolve_prop("on").unwrap(), (2, 1));
        assert_eq!(Controller::resolve_prop("electricity").unwrap(), (20, 1));
        assert!(Controller::resolve_prop("definitelyNotAProp").is_err());
    }

    #[test]
    fn property_reads_are_bounded_and_keep_energy_results() {
        let params: Vec<_> = (0..17).map(|i| json!({"piid": i})).collect();
        let mut sizes = Vec::new();
        let result = read_property_batches(&params, |batch| {
            sizes.push(batch.len());
            Ok(Value::Array(batch.to_vec()))
        }).unwrap();
        assert_eq!(sizes, vec![8, 8, 1]);
        assert_eq!(result, params);
        assert!(read_property_batches(&[], |_| panic!("empty request")).unwrap().is_empty());
        assert!(read_property_batches(&params, |_| Err(ControllerError("timeout".into()))).is_err());
    }

    #[test]
    fn one_failing_optional_property_does_not_discard_other_results() {
        let params: Vec<_> = (0..8).map(|i| json!({"piid": i})).collect();
        let result = read_property_batches(&params, |batch| {
            if batch.iter().any(|value| value["piid"] == 5) {
                Err(ControllerError("unsupported property".into()))
            } else {
                Ok(Value::Array(batch.to_vec()))
            }
        }).unwrap();
        assert_eq!(result.len(), 7);
        assert!(result.iter().all(|value| value["piid"] != 5));
        assert!(result.iter().any(|value| value["piid"] == 7));
    }

    #[test]
    fn failures_in_both_halves_still_preserve_readable_properties() {
        let params: Vec<_> = (0..8).map(|i| json!({"piid": i})).collect();
        let result = read_property_batches(&params, |batch| {
            if batch.iter().any(|value| value["piid"] == 1 || value["piid"] == 6) {
                Err(ControllerError("unsupported property".into()))
            } else {
                Ok(Value::Array(batch.to_vec()))
            }
        }).unwrap();
        let ids: Vec<_> = result.iter().map(|value| value["piid"].as_i64().unwrap()).collect();
        assert_eq!(ids, vec![0, 2, 3, 4, 5, 7]);
    }

    #[test]
    fn unknown_power_state_must_not_trigger_automatic_power_on() {
        assert_eq!(confirmed_power_state(&[("on", PropValue::Ok(json!(true)))]).unwrap(), true);
        assert_eq!(confirmed_power_state(&[("on", PropValue::Ok(json!(false)))]).unwrap(), false);
        assert!(confirmed_power_state(&[("on", PropValue::Missing)]).is_err());
        assert!(confirmed_power_state(&[("on", PropValue::Err(-4004))]).is_err());
    }

    #[test]
    fn only_reads_are_safe_to_retry_through_another_link() {
        assert!(rpc_retry_safe("get_properties"));
        assert!(!rpc_retry_safe("set_properties"));
    }
}
