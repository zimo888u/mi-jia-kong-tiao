//! Small JSON interface for conversational control. Never prints credentials.

use miac_core::controller::{Controller, PropValue};
use miac_core::credentials::Credentials;
use miac_core::settings::Settings;
use miac_core::Transport;
use serde_json::{json, Map, Value};

#[derive(Debug, PartialEq)]
enum Action {
    Help,
    Status,
    Capabilities,
    Room(RoomSource),
    Power(bool),
    Temp(f64),
    Mode(String),
    Fan(Value),
    Swing(&'static str, bool),
}

#[derive(Debug, PartialEq)]
enum RoomSource {
    Auto,
    AirConditioner,
    Thermometer,
}

fn parse(args: &[String]) -> Result<(Action, Option<Transport>), String> {
    let cloud = args.iter().any(|a| a == "--cloud");
    let local = args.iter().any(|a| a == "--local");
    if cloud && local {
        return Err("不能同时指定 --cloud 和 --local".into());
    }
    let transport = if cloud {
        Some(Transport::Cloud)
    } else if local {
        Some(Transport::Local)
    } else {
        None
    };
    let words: Vec<&str> = args
        .iter()
        .filter(|a| *a != "--cloud" && *a != "--local")
        .map(String::as_str)
        .collect();
    let action = match words.as_slice() {
        [] | ["help"] | ["--help"] => Action::Help,
        ["status"] => Action::Status,
        ["capabilities"] => Action::Capabilities,
        ["room"] | ["room", "auto"] => Action::Room(RoomSource::Auto),
        ["room", "ac"] => Action::Room(RoomSource::AirConditioner),
        ["room", "thermometer"] => Action::Room(RoomSource::Thermometer),
        ["power", "on"] => Action::Power(true),
        ["power", "off"] => Action::Power(false),
        ["temp", value] => {
            let n: f64 = value.parse().map_err(|_| "温度必须是数字".to_string())?;
            if !n.is_finite() {
                return Err("温度必须是有限数值".into());
            }
            Action::Temp(n)
        }
        ["mode", value @ ("cool" | "heat" | "dry" | "fan" | "auto")] => {
            Action::Mode((*value).into())
        }
        ["fan", value @ ("auto" | "max")] => Action::Fan(json!(value)),
        ["fan", value] => {
            let n: i64 = value.parse().map_err(|_| "风速必须是 auto、max 或整数档位".to_string())?;
            Action::Fan(json!(n))
        }
        ["swing", "vertical", "on"] => Action::Swing("verticalSwing", true),
        ["swing", "vertical", "off"] => Action::Swing("verticalSwing", false),
        ["swing", "horizontal", "on"] => Action::Swing("horizontalSwing", true),
        ["swing", "horizontal", "off"] => Action::Swing("horizontalSwing", false),
        _ => return Err("未知命令或参数；运行 help 查看用法".into()),
    };
    Ok((action, transport))
}

fn device(ctrl: &Controller) -> Value {
    let d = ctrl.device();
    json!({
        "name": d.and_then(|x| x.name.as_deref()),
        "model": d.and_then(|x| x.model.as_deref()),
        "link": ctrl.link.map(|link| link.label()),
    })
}

fn property_value(value: &PropValue) -> Option<Value> {
    match value {
        PropValue::Ok(v) => Some(v.clone()),
        PropValue::Err(_) | PropValue::Missing => None,
    }
}

fn read_value(ctrl: &mut Controller, name: &'static str) -> Result<Option<Value>, String> {
    let props = ctrl.read_props(&[name]).map_err(|e| e.to_string())?;
    Ok(props.iter().find(|(key, _)| *key == name).and_then(|(_, v)| property_value(v)))
}

fn capabilities(ctrl: &Controller) -> Value {
    let profile = &ctrl.profile;
    let choices = |name: &str| {
        profile.properties.get(name).filter(|p| p.writable).map(|p| {
            p.choices.iter().map(|(value, label)| json!({"value":value,"label":label})).collect::<Vec<_>>()
        })
    };
    let temperature = profile.properties.get("targetTemp").and_then(|p| p.range);
    json!({
        "temperature_c": temperature.map(|r| json!({"min":r[0],"max":r[1],"step":r[2]})),
        "mode": choices("mode"),
        "fan": choices("fanLevel"),
        "vertical_swing": profile.writable("verticalSwing"),
        "horizontal_swing": profile.writable("horizontalSwing"),
    })
}

fn thermometer(ctrl: &mut Controller) -> Result<Value, String> {
    let t = ctrl.read_thermometer();
    let temperature = t.temperature.ok_or_else(|| {
        t.reason.unwrap_or_else(|| "温湿度计没有温度读数".into())
    })?;
    Ok(json!({
        "ok": true,
        "source": "thermometer",
        "sensor_name": t.name,
        "temperature_c": temperature,
        "humidity_percent": t.humidity,
        "battery_percent": t.battery,
    }))
}

fn air_conditioner_room(ctrl: &mut Controller) -> Result<Value, String> {
    ctrl.init_transport().map_err(|e| e.to_string())?;
    let props = ctrl.read_props(&["roomTemp", "indoorHumidity"]).map_err(|e| e.to_string())?;
    let get = |name| props.iter().find(|(key, _)| *key == name).and_then(|(_, value)| property_value(value));
    let temperature = get("roomTemp").and_then(|v| v.as_f64()).ok_or("空调没有可用的室温读数")?;
    Ok(json!({
        "ok": true,
        "source": "air_conditioner",
        "device": device(ctrl),
        "temperature_c": temperature,
        "humidity_percent": get("indoorHumidity"),
    }))
}

fn mode_expected(ctrl: &Controller, mode: &str) -> Option<Value> {
    let label = match mode {
        "cool" => "制冷", "heat" => "制热", "dry" => "除湿",
        "fan" => "送风", "auto" => "自动", _ => return None,
    };
    ctrl.profile.properties.get("mode")?.choices.iter()
        .find(|(_, name)| name == label).map(|(value, _)| json!(value))
}

fn fan_expected(ctrl: &Controller, value: &Value) -> Option<Value> {
    let choices = &ctrl.profile.properties.get("fanLevel")?.choices;
    match value.as_str() {
        Some("auto") => choices.iter().find(|(_, name)| name == "自动").map(|(v, _)| json!(v)),
        Some("max") => choices.iter().max_by_key(|(v, _)| v).map(|(v, _)| json!(v)),
        _ => Some(value.clone()),
    }
}

fn write_result(ctrl: &mut Controller, property: &'static str, requested: Value, expected: Value) -> Value {
    std::thread::sleep(std::time::Duration::from_millis(600));
    let (observed, readback_error) = match read_value(ctrl, property) {
        Ok(value) => (value, None),
        Err(error) => (None, Some(error)),
    };
    json!({
        "ok": true,
        "device": device(ctrl),
        "property": property,
        "requested": requested,
        "acknowledged": true,
        "observed": observed,
        "verified": observed.as_ref() == Some(&expected),
        "readback_error": readback_error,
    })
}

fn run(action: Action, transport_override: Option<Transport>) -> Result<Value, String> {
    if action == Action::Help {
        return Ok(json!({
            "ok": true,
            "commands": [
                "status", "capabilities", "room [auto|ac|thermometer]",
                "power on|off", "temp <celsius>", "mode cool|heat|dry|fan|auto",
                "fan auto|max|<number>", "swing vertical|horizontal on|off"
            ],
            "options": ["--local", "--cloud"],
            "note": "temp 会在关机时先开机；温度不在该型号范围或步长上时拒绝写入"
        }));
    }
    let creds = Credentials::appdata();
    let transport = transport_override.unwrap_or_else(|| Settings::load(&creds).transport());
    let mut ctrl = Controller::new(creds, transport);

    if let Action::Room(RoomSource::Thermometer) = action {
        return thermometer(&mut ctrl);
    }
    if let Action::Room(RoomSource::Auto) = action {
        let ac_result = air_conditioner_room(&mut ctrl);
        return match ac_result {
            Ok(value) => Ok(value),
            Err(ac_error) => thermometer(&mut ctrl).map_err(|thermo_error| {
                format!("空调室温不可用：{ac_error}；温湿度计不可用：{thermo_error}")
            }),
        };
    }
    if let Action::Room(RoomSource::AirConditioner) = action {
        return air_conditioner_room(&mut ctrl);
    }

    ctrl.init_transport().map_err(|e| e.to_string())?;
    match action {
        Action::Status => {
            let snapshot = ctrl.snapshot().map_err(|e| e.to_string())?;
            let mut properties = Map::new();
            let mut property_errors = Map::new();
            for (name, value) in snapshot.status {
                properties.insert(name.into(), property_value(&value).unwrap_or(Value::Null));
                if let PropValue::Err(code) = value {
                    property_errors.insert(name.into(), json!(code));
                }
            }
            Ok(json!({
                "ok": true, "device": device(&ctrl),
                "properties": properties, "property_errors": property_errors,
                "fault": snapshot.fault.text,
                "capabilities": capabilities(&ctrl),
            }))
        }
        Action::Capabilities => Ok(json!({"ok":true,"device":device(&ctrl),"capabilities":capabilities(&ctrl)})),
        Action::Power(on) => {
            ctrl.set_power(on).map_err(|e| e.to_string())?;
            Ok(write_result(&mut ctrl, "on", json!(on), json!(on)))
        }
        Action::Temp(temp) => {
            ctrl.profile.validate("targetTemp", &json!(temp)).map_err(|e| e.to_string())?;
            let (turned_on, applied) = ctrl.apply_temp_preset(temp).map_err(|e| e.to_string())?;
            let mut result = write_result(&mut ctrl, "targetTemp", json!(temp), json!(applied));
            result["turned_on"] = json!(turned_on);
            result["applied_c"] = json!(applied);
            Ok(result)
        }
        Action::Mode(mode) => {
            let expected = mode_expected(&ctrl, &mode).ok_or("该型号不支持所选模式")?;
            ctrl.set_mode(&json!(mode)).map_err(|e| e.to_string())?;
            Ok(write_result(&mut ctrl, "mode", json!(mode), expected))
        }
        Action::Fan(level) => {
            let expected = fan_expected(&ctrl, &level).ok_or("该型号不支持所选风速")?;
            ctrl.set_fan(&level).map_err(|e| e.to_string())?;
            Ok(write_result(&mut ctrl, "fanLevel", level, expected))
        }
        Action::Swing(name, on) => {
            ctrl.set_toggle(name, on).map_err(|e| e.to_string())?;
            Ok(write_result(&mut ctrl, name, json!(on), json!(on)))
        }
        Action::Help | Action::Room(_) => unreachable!(),
    }
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let result = parse(&args).and_then(|(action, transport)| run(action, transport));
    match result {
        Ok(value) => println!("{value}"),
        Err(error) => {
            println!("{}", json!({"ok":false,"error":error}));
            std::process::exit(1);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn command(input: &[&str]) -> Result<Action, String> {
        parse(&input.iter().map(|s| (*s).to_string()).collect::<Vec<_>>()).map(|(action, _)| action)
    }

    #[test]
    fn only_named_controls_are_exposed() {
        assert_eq!(command(&["power", "off"]).unwrap(), Action::Power(false));
        assert_eq!(command(&["room", "thermometer"]).unwrap(), Action::Room(RoomSource::Thermometer));
        assert!(command(&["raw", "2", "1"]).is_err());
        assert!(command(&["swing", "diagonal", "on"]).is_err());
        assert!(command(&["mode", "99"]).is_err());
        assert!(command(&["temp", "NaN"]).is_err());
    }
}
