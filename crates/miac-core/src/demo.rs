//! demo.rs —— 第 1 阶段内存原型用的假数据
//!
//! 目的是把「界面能不能跑、内存有多大」和「控制核心对不对」解耦：
//! 第 1 阶段的验收门是「运行 5 分钟总内存 < 45 MB」，只和界面有关，
//! 所以先用这里的数据把五个页面填满，不碰任何网络。
//!
//! 数值取自真机实测的一次快照（见 README 的截图），以便观察真实排版效果
//! （例如温度是 26.0、室温 28.4 这类一位小数）。

use crate::miot;

/// 状态快照（对应 v1 `Controller.snapshot()` 的 `status` 字段）。
#[derive(Debug, Clone)]
pub struct DemoState {
    pub on: bool,
    pub mode: i64,
    pub target_temp: f64,
    pub fan_level: i64,
    pub vertical_swing: bool,
    pub horizontal_swing: bool,
    pub wind_direction: i64,
    pub wind_sensation: bool,
    pub vertical_pos: i64,
    pub horizontal_pos: i64,
    pub eco: bool,
    pub sleep: bool,
    pub heater: bool,
    pub dryer: bool,
    pub favorite_on: bool,
    pub room_temp: f64,
    pub indoor_humidity: i64,
    pub electricity: f64,
    pub fault_value: i64,
    pub light: bool,
    pub buzzer: bool,
    pub light_bright: i64,
    pub cooling_effect: i64,
    pub heating_effect: i64,
}

impl Default for DemoState {
    fn default() -> Self {
        Self {
            on: true,
            mode: 2, // 制冷
            target_temp: 26.0,
            fan_level: 0, // 自动
            vertical_swing: false,
            horizontal_swing: false,
            wind_direction: 0,
            wind_sensation: false,
            vertical_pos: 0,
            horizontal_pos: 0,
            eco: false,
            sleep: false,
            heater: false,
            dryer: false,
            favorite_on: false,
            room_temp: 28.4,
            indoor_humidity: 0,
            electricity: 412.6,
            fault_value: 0,
            light: true,
            buzzer: true,
            light_bright: 0,
            cooling_effect: 3,
            heating_effect: 3,
        }
    }
}

impl DemoState {
    pub fn fault(&self) -> miot::FaultInfo {
        miot::fault_info(Some(self.fault_value))
    }

    pub fn mode_name(&self) -> &'static str {
        miot::MODE_NAME
            .iter()
            .find(|(k, _)| *k == self.mode)
            .map(|(_, v)| *v)
            .unwrap_or("—")
    }

    pub fn fan_name(&self) -> &'static str {
        miot::FAN_NAME
            .iter()
            .find(|(k, _)| *k == self.fan_level)
            .map(|(_, v)| *v)
            .unwrap_or("—")
    }

    pub fn wind_name(&self) -> &'static str {
        miot::WIND_NAME
            .iter()
            .find(|(k, _)| *k == self.wind_direction)
            .map(|(_, v)| *v)
            .unwrap_or("—")
    }
}

/// 诊断读数（对应 v1 `diag()`），顺序与 `miot::DIAG_PROPS` 一致。
pub fn demo_diag() -> Vec<(&'static str, String, &'static str)> {
    vec![
        ("室内管温", "11.5".into(), "℃"),
        ("室内风机", "820".into(), "rpm"),
        ("室外温度", "33.0".into(), "℃"),
        ("室外管温", "38.5".into(), "℃"),
        ("压缩机频率", "42.0".into(), "Hz"),
        ("外机电流", "3.42".into(), "A"),
        ("外机电压", "223.1".into(), "V"),
        ("累计运行", "1284.5".into(), "h"),
        ("风速百分比", "48".into(), "%"),
    ]
}

/// 温湿度计读数。
pub struct DemoThermometer {
    pub available: bool,
    pub temperature: f64,
    pub humidity: f64,
    pub battery: i64,
}

pub fn demo_thermometer() -> DemoThermometer {
    DemoThermometer { available: true, temperature: 26.8, humidity: 58.0, battery: 92 }
}

/// 电量统计（对应 v1 `powerStats()`）。
pub struct DemoPower {
    pub today_energy: f64,
    pub month_energy: f64,
    pub year_energy: f64,
    pub today_minutes: i64,
    pub month_minutes: i64,
    /// 当月每日电量，索引 0 = 1 号
    pub daily: Vec<f64>,
    /// 12 个月电量
    pub months: Vec<f64>,
    pub year: i32,
    pub month: i32,
    pub today: i32,
    pub days_in_month: i32,
    /// 当月 1 号是星期几（0 = 周日）
    pub first_weekday: i32,
}

pub fn demo_power() -> DemoPower {
    // 造一条像真机的曲线：前半月开机多、后半月少，数值带一位小数
    let daily: Vec<f64> = (0..30)
        .map(|i| {
            let base = 3.2 + (i as f64 * 0.37).sin().abs() * 4.1;
            (base * 10.0).round() / 10.0
        })
        .collect();
    let months: Vec<f64> = (0..12)
        .map(|i| ((28.0 + (i as f64 * 0.9).cos().abs() * 90.0) * 10.0).round() / 10.0)
        .collect();

    let sum = |v: &[f64]| v.iter().sum::<f64>();

    DemoPower {
        today_energy: 6.4,
        month_energy: (sum(&daily) * 10.0).round() / 10.0,
        year_energy: (sum(&months) * 10.0).round() / 10.0,
        today_minutes: 412,
        month_minutes: 8940,
        daily,
        months,
        year: 2026,
        month: 9,
        today: 19,
        days_in_month: 30,
        first_weekday: 2, // 2026-09-01 是周二
    }
}

/// 原始属性表的一行（对应界面「全部状态属性」）。
pub fn demo_all_props(state: &DemoState) -> Vec<String> {
    let b = |v: bool| if v { "true" } else { "false" };
    vec![
        format!("on = {}", b(state.on)),
        format!("mode = {}", state.mode),
        format!("targetTemp = {}", state.target_temp),
        format!("fanLevel = {}", state.fan_level),
        format!("verticalSwing = {}", b(state.vertical_swing)),
        format!("horizontalSwing = {}", b(state.horizontal_swing)),
        format!("windDirection = {}", state.wind_direction),
        format!("windSensation = {}", b(state.wind_sensation)),
        format!("verticalPos = {}", state.vertical_pos),
        format!("horizontalPos = {}", state.horizontal_pos),
        format!("eco = {}", b(state.eco)),
        format!("sleep = {}", b(state.sleep)),
        format!("heater = {}", b(state.heater)),
        format!("dryer = {}", b(state.dryer)),
        format!("favoriteOn = {}", b(state.favorite_on)),
        format!("roomTemp = {}", state.room_temp),
        format!("indoorHumidity = {}", state.indoor_humidity),
        format!("electricity = {}", state.electricity),
        format!("faultValue = {}", state.fault_value),
        format!("light = {}", b(state.light)),
        format!("buzzer = {}", b(state.buzzer)),
        format!("lightBright = {}", state.light_bright),
        format!("coolingEffect = {}", state.cooling_effect),
        format!("heatingEffect = {}", state.heating_effect),
    ]
}

/// 供属性选择器用的属性名列表（即 `miot::PROPS` 的名字）。
pub fn prop_names() -> Vec<String> {
    miot::PROPS.iter().map(|(n, _)| (*n).to_string()).collect()
}
