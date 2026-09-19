//! miot.rs —— MIoT 属性映射与常量表（**本文件由 tools/gen-miot.js 自动生成**）
//!
//! 目标机型：空调 巨省电 大2匹新一级能效
//!   MIoT: xiaomi.airc.h53h00
//!   urn:miot-spec-v2:device:air-conditioner:0000A004:xiaomi-h53h00:1
//!
//! ⚠ 不要套用其它米家空调的属性表。同为「巨省电」，xiaomi.aircondition.mh4
//!   （2 匹老款）与本机差异很大，例如耗电量：mh4 在 8.1，本机在 20.1。
//!
//! 生成来源：ac-ctl/lib/miot.js（Node 版），保证 v1/v2 属性表逐字节一致。
//! 重新生成：node tools/gen-miot.js

#![allow(dead_code)]

/// 期望的机型标识，用于校验 device.json 里的 model 是否匹配。
pub const EXPECT_MODEL: &str = "xiaomi.airc.h53h00";

/// 米家云国家/地区代码。
pub const COUNTRY: &str = "cn";

/// 属性名 → (siid, piid)。
///
/// 顺序与 lib/miot.js 的 PROP 完全一致，方便逐项对照。
pub const PROPS: &[(&str, (u16, u16))] = &[
    ("on", (2u16, 1u16)),
    ("mode", (2u16, 2u16)),
    ("fault", (2u16, 3u16)),
    ("targetTemp", (2u16, 4u16)),
    ("eco", (2u16, 7u16)),
    ("heater", (2u16, 9u16)),
    ("dryer", (2u16, 10u16)),
    ("sleep", (2u16, 11u16)),
    ("targetHumidity", (2u16, 14u16)),
    ("favoriteOn", (2u16, 19u16)),
    ("favoriteType", (2u16, 20u16)),
    ("fanLevel", (3u16, 2u16)),
    ("horizontalSwing", (3u16, 3u16)),
    ("verticalSwing", (3u16, 4u16)),
    ("windDirection", (3u16, 11u16)),
    ("windSensation", (3u16, 24u16)),
    ("verticalPos", (3u16, 25u16)),
    ("horizontalPos", (3u16, 26u16)),
    ("roomTemp", (4u16, 7u16)),
    ("buzzer", (5u16, 1u16)),
    ("light", (6u16, 1u16)),
    ("lightBright", (6u16, 2u16)),
    ("electricity", (20u16, 1u16)),
    ("dailyOnTime", (8u16, 5u16)),
    ("clean", (9u16, 1u16)),
    ("examine", (9u16, 2u16)),
    ("error", (9u16, 3u16)),
    ("runDuration", (9u16, 5u16)),
    ("fanPercent", (10u16, 1u16)),
    ("indoorHumidity", (10u16, 35u16)),
    ("indoorPipeTemp", (12u16, 1u16)),
    ("indoorFanSpeed", (12u16, 3u16)),
    ("outdoorTemp", (12u16, 7u16)),
    ("outdoorPipeTemp", (12u16, 8u16)),
    ("compressorFreq", (12u16, 11u16)),
    ("outdoorCurrent", (12u16, 13u16)),
    ("outdoorVoltage", (12u16, 14u16)),
    ("faultValue", (13u16, 1u16)),
    ("coolingEffect", (25u16, 1u16)),
    ("heatingEffect", (25u16, 2u16)),
];

/// 按属性名查 (siid, piid)，等价于 v1 的 Controller.resolveProp。
pub fn prop_addr(name: &str) -> Option<(u16, u16)> {
    PROPS.iter().find(|(n, _)| *n == name).map(|(_, a)| *a)
}

/// 米家智能温湿度计 3（miaomiaoce.sensor_ht.t9）经蓝牙网关上报的云端历史键。
///
/// 该类 BLE 设备通常不响应普通 get_properties，需读取最近一条上报记录。
pub const THERMOMETER_HISTORY_KEY_TEMPERATURE: &str = "4100";
pub const THERMOMETER_HISTORY_KEY_HUMIDITY: &str = "4102";

/// 温湿度计的 MIoT 属性（电池 / 温度 / 湿度）。
pub const THERMOMETER_PROPS: &[(&str, (u16, u16))] = &[
    ("battery", (2u16, 1003u16)),
    ("temperature", (3u16, 1001u16)),
    ("humidity", (3u16, 1002u16)),
];

/// 模式值 → 中文名。
pub const MODE_NAME: &[(i64, &str)] = &[
    (2, "制冷"),
    (3, "除湿"),
    (4, "送风"),
    (5, "制热"),
];

/// 风速档位 → 中文名。
pub const FAN_NAME: &[(i64, &str)] = &[
    (0, "自动"),
    (1, "一档"),
    (2, "二档"),
    (3, "三档"),
    (4, "四档"),
    (5, "五档"),
    (6, "六档"),
    (7, "七档"),
    (8, "Max档"),
];

/// 风感值 → 中文名。
pub const WIND_NAME: &[(i64, &str)] = &[
    (0, "关"),
    (1, "上吹风"),
    (2, "下吹风"),
    (3, "循环风"),
    (4, "防直吹"),
];

/// 定格位置 → 中文名。
pub const POS_NAME: &[(i64, &str)] = &[
    (0, "关闭"),
    (1, "上/左"),
    (2, "偏上/偏左"),
    (3, "中间"),
    (4, "偏下/偏右"),
    (5, "下/右"),
];

/// 指示灯亮度 → 中文名。
pub const BRIGHT_NAME: &[(i64, &str)] = &[
    (0, "自动"),
    (1, "中亮"),
    (2, "高亮"),
];

/// 冷热效果档位 → 中文名。
pub const EFFECT_NAME: &[(i64, &str)] = &[
    (1, "弱"),
    (2, "较小"),
    (3, "舒适"),
    (4, "较大"),
    (5, "强"),
];

/// 故障上报标志 13.1 取值（节选）：0 无故障；负值表示该故障已清除。
pub const FAULT_NAME: &[(i64, &str)] = &[
    (4, "F2.4"),
    (5, "F3.2"),
    (6, "P1"),
    (7, "P2.1"),
    (8, "P2.2"),
    (9, "P2.3"),
    (10, "P2.4"),
    (11, "P2.5"),
    (12, "P4"),
    (13, "P5"),
    (14, "P6"),
    (15, "P8"),
    (16, "PA"),
    (17, "PC"),
    (18, "U2"),
    (19, "U3"),
    (20, "U4"),
    (21, "U5"),
    (22, "U6.1"),
    (23, "U6.2"),
    (24, "U8"),
    (25, "U0"),
    (26, "C1"),
    (27, "C2"),
    (28, "C3"),
    (29, "C4"),
    (30, "C5"),
    (31, "U2.1"),
    (32, "E2.1"),
    (33, "E2.6"),
    (34, "E2.5"),
    (35, "E2.2"),
    (36, "F2.6"),
    (37, "F7"),
    (39, "E0.1"),
    (40, "E0.2"),
    (41, "E0.3"),
    (42, "E0.4"),
    (43, "E0.5"),
    (44, "E0.6"),
    (45, "E0.7"),
    (46, "E0.8"),
    (47, "F5"),
    (52, "E0.9"),
    (112, "PA"),
];

/// 状态总览要读的属性集合（顺序即界面显示顺序）。
pub const STATUS_PROPS: &[&str] = &[
    "on",
    "mode",
    "targetTemp",
    "fanLevel",
    "verticalSwing",
    "horizontalSwing",
    "windDirection",
    "eco",
    "sleep",
    "heater",
    "dryer",
    "favoriteOn",
    "roomTemp",
    "indoorHumidity",
    "electricity",
    "faultValue",
];

/// 机器诊断要读的属性集合。
pub const DIAG_PROPS: &[&str] = &[
    "indoorPipeTemp",
    "indoorFanSpeed",
    "outdoorTemp",
    "outdoorPipeTemp",
    "compressorFreq",
    "outdoorCurrent",
    "outdoorVoltage",
    "runDuration",
];

/// 温度预设档位（关机时仅保存，开机后自动应用）。
pub const TEMP_PRESETS: &[f64] = &[
    27.5,
    27.0,
    26.5,
];

/// 模式名 → 模式值（cool / dry / fan / heat）。
pub const MODE_VALUE: &[(&str, i64)] = &[
    ("cool", 2),
    ("dry", 3),
    ("fan", 4),
    ("heat", 5),
];

/// 风感名 → 风感值（off / up / down / circle / noblow）。
pub const WIND_VALUE: &[(&str, i64)] = &[
    ("off", 0),
    ("up", 1),
    ("down", 2),
    ("circle", 3),
    ("noblow", 4),
];

/// 亮度名 → 亮度值（auto / mid / high）。
pub const BRIGHT_VALUE: &[(&str, i64)] = &[
    ("auto", 0),
    ("mid", 1),
    ("high", 2),
];

/// 效果名 → 效果值（weak / small / comfort / big / strong）。
pub const EFFECT_VALUE: &[(&str, i64)] = &[
    ("weak", 1),
    ("small", 2),
    ("comfort", 3),
    ("big", 4),
    ("strong", 5),
];

/// 把 13.1 的数值翻译成可读的故障描述。
///
/// 与 v1 的 `lib/miot.js` `faultInfo()` 行为逐分支对应。
#[derive(Debug, Clone, PartialEq)]
pub struct FaultInfo {
    /// 是否属于「已清除 / 无故障」。
    pub clear: bool,
    /// 展示用文本。
    pub text: String,
    /// 故障徽标（如 "F2.4"、"P1"），无故障时为 None。
    pub badge: Option<String>,
}

pub fn fault_info(value: Option<i64>) -> FaultInfo {
    let Some(value) = value else {
        return FaultInfo { clear: false, text: "读取异常".into(), badge: None };
    };
    if value == 0 {
        return FaultInfo { clear: true, text: "无故障".into(), badge: None };
    }
    let code = FAULT_NAME.iter()
        .find(|(k, _)| *k == value.abs())
        .map(|(_, v)| *v)
        .unwrap_or("未知代码");
    if value < 0 {
        FaultInfo {
            clear: true,
            text: format!("{value}（{code} CLEAR，已清除记录）"),
            badge: Some(code.to_string()),
        }
    } else {
        FaultInfo {
            clear: false,
            text: format!("{value}（{code}，可能为当前故障或锁存/历史上报）"),
            badge: Some(code.to_string()),
        }
    }
}
