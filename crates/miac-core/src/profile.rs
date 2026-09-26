//! Model-specific MIoT capabilities. Never reuse property addresses across models.
use std::{collections::{BTreeMap, BTreeSet}, io::Read, path::Path, time::Duration};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use crate::miot;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Property {
    pub siid: u16,
    pub piid: u16,
    pub readable: bool,
    pub writable: bool,
    pub format: String,
    pub range: Option<[f64; 3]>,
    pub choices: Vec<(i64, String)>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Profile {
    pub model: String,
    pub description: String,
    pub spec_status: String,
    pub properties: BTreeMap<String, Property>,
}

fn kind(value: &Value) -> &str {
    value["type"].as_str().unwrap_or("").split(':').nth(3).unwrap_or("")
}

fn integer_format(format: &str) -> Option<(bool, u32)> {
    match format {
        "uint8" => Some((false, 8)),
        "uint16" => Some((false, 16)),
        "uint32" => Some((false, 32)),
        "uint64" => Some((false, 64)),
        "int8" => Some((true, 8)),
        "int16" => Some((true, 16)),
        "int32" => Some((true, 32)),
        "int64" => Some((true, 64)),
        _ => None,
    }
}

fn exact_integral_float(value: &Value) -> Option<f64> {
    // JSON decimals such as 1.0 are accepted only while their integer value
    // can still be represented exactly by f64.
    let number = value.as_f64()?;
    (number.is_finite() && number.fract() == 0.0 && number.abs() <= 9_007_199_254_740_992.0)
        .then_some(number)
}

impl Profile {
    fn without_unverified_mode(mut self, ambiguous_catalog_revisions: bool) -> Self {
        // Public revisions of these models assign different numbers to Cool,
        // Dry and Heat. A model name alone cannot identify the device revision.
        if ambiguous_catalog_revisions || matches!(self.model.as_str(),
            "xiaomi.aircondition.ma1" | "xiaomi.aircondition.ma2" | "xiaomi.aircondition.ma4") {
            if let Some(mode) = self.properties.get_mut("mode") { mode.writable = false; }
        }
        self
    }

    pub fn legacy() -> Self {
        let mut profile = Self { model: miot::EXPECT_MODEL.into(), description: "米家空调".into(), spec_status: "legacy".into(), ..Self::default() };
        for &(name, (siid, piid)) in miot::PROPS {
            let boolean = ["on", "eco", "heater", "dryer", "sleep", "favoriteOn", "horizontalSwing", "verticalSwing", "windSensation", "buzzer", "light"].contains(&name);
            let choices = match name { "mode" => miot::MODE_NAME, "fanLevel" => miot::FAN_NAME, _ => &[] };
            profile.properties.insert(name.into(), Property {
                siid, piid, readable: true, writable: ["on", "mode", "targetTemp", "eco", "heater", "dryer", "sleep", "favoriteOn", "fanLevel", "horizontalSwing", "verticalSwing", "windDirection", "windSensation", "verticalPos", "horizontalPos", "buzzer", "light", "lightBright", "coolingEffect", "heatingEffect", "fanPercent"].contains(&name),
                format: if boolean { "bool" } else { "float" }.into(),
                range: (name == "targetTemp").then_some([16.0,31.0,0.5]),
                choices: choices.iter().map(|(v,n)| (*v, (*n).into())).collect(),
            });
        }
        profile
    }

    pub fn parse(model: &str, spec: &Value) -> Result<Self, String> {
        if kind(spec) != "air-conditioner" { return Err("该型号不是 MIoT 空调设备".into()); }
        let mut profile = Self { model: model.into(), description: spec["description"].as_str().unwrap_or("米家空调").into(), spec_status: spec["status"].as_str().unwrap_or("unknown").into(), ..Self::default() };
        for service in spec["services"].as_array().into_iter().flatten() {
            for prop in service["properties"].as_array().into_iter().flatten() {
                let name = match (kind(service), kind(prop)) {
                    ("air-conditioner", "on") => "on",
                    ("air-conditioner", "mode") => "mode",
                    ("air-conditioner", "target-temperature") => "targetTemp",
                    ("air-conditioner", "fault") => "fault",
                    ("air-conditioner", "eco") => "eco",
                    ("air-conditioner", "heater") => "heater",
                    ("air-conditioner", "dryer") => "dryer",
                    ("air-conditioner", "sleep-mode") => "sleep",
                    ("air-conditioner" | "fan-control", "fan-level") => "fanLevel",
                    ("air-conditioner" | "fan-control", "horizontal-swing") => "horizontalSwing",
                    ("air-conditioner" | "fan-control", "vertical-swing") => "verticalSwing",
                    ("environment", "temperature") => "roomTemp",
                    ("environment", "relative-humidity") => "indoorHumidity",
                    ("indicator-light", "on") => "light",
                    ("alarm", "alarm") => "buzzer",
                    // Vendor extensions deliberately limited to the verified family.
                    ("electricity", "electricity") if model.starts_with("xiaomi.") => "electricity",
                    ("maintenance", "running-duration") if model.starts_with("xiaomi.") => "runDuration",
                    _ => continue,
                };
                let Some(siid) = service["iid"].as_u64().and_then(|v| u16::try_from(v).ok()).filter(|v| *v > 0) else { continue };
                let Some(piid) = prop["iid"].as_u64().and_then(|v| u16::try_from(v).ok()).filter(|v| *v > 0) else { continue };
                let access = |mode: &str| prop["access"].as_array().is_some_and(|a| a.iter().any(|v| v.as_str() == Some(mode)));
                let format = prop["format"].as_str().unwrap_or("").to_string();
                if ["on", "eco", "heater", "dryer", "sleep", "horizontalSwing", "verticalSwing", "light", "buzzer"].contains(&name) && format != "bool" { continue; }
                let range = prop["value-range"].as_array().and_then(|a| {
                    let r = [a.first()?.as_f64()?, a.get(1)?.as_f64()?, a.get(2)?.as_f64()?];
                    (r.iter().all(|n| n.is_finite()) && r[0] < r[1] && r[2] > 0.0).then_some(r)
                });
                let mut choices: Vec<_> = prop["value-list"].as_array().into_iter().flatten().filter_map(|c| {
                    Some((c["value"].as_i64()?, label(c["description"].as_str().unwrap_or(""))))
                }).collect();
                if choices.is_empty() && name == "fanLevel" {
                    if let Some([min, max, step]) = range {
                        if (max-min)/step <= 20.0 && step.fract() == 0.0 && min.fract() == 0.0 {
                            choices = (0..=((max-min)/step) as i64).map(|i| { let v = min as i64 + i * step as i64; (v, v.to_string()) }).collect();
                        }
                    }
                }
                if name == "fanLevel" && profile.properties.contains_key(name) {
                    if range.is_some() && choices.is_empty() {
                        profile.properties.entry("fanPercent".into()).or_insert(Property { siid, piid, readable: access("read"), writable: access("write"), format, range, choices });
                    }
                    continue;
                }
                profile.properties.entry(name.into()).or_insert(Property { siid, piid, readable: access("read"), writable: access("write"), format, range, choices });
            }
        }
        if !profile.properties.get("on").is_some_and(|p| p.readable && p.writable)
            || !profile.properties.get("targetTemp").is_some_and(|p| p.readable && p.writable && p.range.is_some()
                && (p.format == "float" || integer_format(&p.format).is_some())) {
            return Err("该型号未公开可安全读写的开关和目标温度规格".into());
        }
        if let Some(mode) = profile.properties.get_mut("mode") {
            if mode.choices.is_empty() { mode.writable = false; }
        }
        Ok(profile)
    }

    pub fn writable(&self, name: &str) -> bool { self.properties.get(name).is_some_and(|p| p.writable) }
    pub fn addr(&self, name: &str) -> Option<(u16,u16)> { self.properties.get(name).map(|p| (p.siid,p.piid)) }
    pub fn temperature_range(&self) -> [f64;3] { self.properties.get("targetTemp").and_then(|p| p.range).unwrap_or([16.0,31.0,0.5]) }
    pub fn snap_temperature(&self, value: f64) -> f64 {
        if !value.is_finite() { return value; }
        let [min,max,step] = self.temperature_range();
        let max_steps = (((max - min) / step) + 1e-8).floor();
        let steps = ((value.clamp(min, max) - min) / step).round().clamp(0.0, max_steps);
        (min + steps * step).min(max)
    }
    pub fn validate(&self, name: &str, value: &Value) -> Result<Value,String> {
        let p = self.properties.get(name).filter(|p| p.writable).ok_or_else(|| format!("该型号不支持写入 {name}"))?;
        if p.format == "bool" { return value.as_bool().map(Value::Bool).ok_or_else(|| "需要布尔值".into()); }
        if p.format == "string" { return value.as_str().map(|s| json!(s)).ok_or_else(|| "需要字符串".into()); }
        let (normalized, n) = match integer_format(&p.format) {
            Some((false, bits)) => {
                let integer = value.as_u64()
                    .or_else(|| exact_integral_float(value).filter(|n| *n >= 0.0).map(|n| n as u64))
                    .ok_or("需要无符号整数")?;
                if bits < 64 && integer >= (1u64 << bits) { return Err("无符号整数超出属性格式范围".into()); }
                (json!(integer), integer as f64)
            }
            Some((true, bits)) => {
                let integer = value.as_i64()
                    .or_else(|| exact_integral_float(value).map(|n| n as i64))
                    .ok_or("需要整数")?;
                if bits < 64 {
                    let limit = 1i64 << (bits - 1);
                    if integer < -limit || integer >= limit { return Err("整数超出属性格式范围".into()); }
                }
                (json!(integer), integer as f64)
            }
            None if p.format == "float" => {
                let number = value.as_f64().filter(|n| n.is_finite()).ok_or("需要有效数值")?;
                (value.clone(), number)
            }
            None => return Err(format!("不支持的属性格式 {}", p.format)),
        };
        if !p.choices.is_empty() && !p.choices.iter().any(|(v,_)| normalized.as_i64() == Some(*v) || (p.format == "float" && *v as f64 == n)) {
            return Err(format!("该型号不支持 {name}={n}"));
        }
        if let Some([min,max,step]) = p.range {
            if n.abs() > 9_007_199_254_740_992.0 || n < min || n > max || ((n-min)/step - ((n-min)/step).round()).abs() > 1e-5 {
                return Err(format!("{name} 需为 {min}~{max}，步长 {step}"));
            }
        }
        Ok(normalized)
    }
}

fn label(name: &str) -> String {
    match name.to_lowercase().as_str() {
        "cool" | "cooling" => "制冷", "heat" | "heating" => "制热", "dry" => "除湿", "fan" => "送风", "auto" | "automatic" => "自动", "low" => "低速", "medium" | "mid" => "中速", "high" => "高速", "silent" | "quiet" => "静音", "turbo" => "强劲", _ => name,
    }.to_string()
}

fn fetch(agent: &ureq::Agent, url: &str) -> Result<Value,String> {
    let response = agent.get(url).call().map_err(|e| e.to_string())?;
    let mut data = Vec::new();
    response.into_reader().take(32*1024*1024).read_to_end(&mut data).map_err(|e| e.to_string())?;
    serde_json::from_slice(&data).map_err(|e| e.to_string())
}

fn select_catalog_spec<'a>(index: &'a Value, model: &str) -> Option<(&'a Value, bool)> {
    let entries = index["instances"].as_array()?;
    let candidates = || entries.iter()
        .filter(|entry| entry["model"].as_str() == Some(model))
        .filter(|entry| entry["type"].as_str().is_some_and(|urn| urn.contains(":device:air-conditioner:")));
    let priority = |entry: &Value| match entry["status"].as_str() {
        Some("released") => 3,
        Some("preview") => 2,
        Some("debug") => 1,
        _ => 0,
    };
    let selected = candidates().max_by_key(|entry| {
        let version = entry["type"].as_str().and_then(|urn| urn.rsplit(':').next())
            .and_then(|s| s.parse::<u32>().ok()).unwrap_or(0);
        (priority(entry), version)
    })?;
    let selected_priority = priority(selected);
    let versions: BTreeSet<_> = candidates()
        .filter(|entry| priority(entry) == selected_priority)
        .filter_map(|entry| entry["type"].as_str())
        .collect();
    Some((selected, versions.len() > 1))
}

pub fn load(model: &str, directory: &Path, proxy: Option<&str>) -> Result<Profile,String> {
    if model == miot::EXPECT_MODEL { return Ok(Profile::legacy()); }
    if model.is_empty() || model.len() > 128 || !model.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'.' || c == b'_') { return Err("设备型号缺失或无效，请重新扫码选择空调".into()); }
    if let Some(spec) = bundled_specs().get(model) {
        if let Ok(profile) = Profile::parse(model, spec) { return Ok(profile.without_unverified_mode(false)); }
    }
    let cache = directory.join("miot-spec-cache").join(format!("{model}.json"));
    if let Ok(data) = std::fs::read(&cache) {
        if let Ok(spec) = serde_json::from_slice::<Value>(&data) {
            if spec["model"].as_str() == Some(model) {
                if let Ok(profile) = Profile::parse(model, &spec["spec"]) {
                    // Old cache files did not record whether revisions conflict.
                    // Keep mode read-only until this can be verified again.
                    let ambiguous = spec["mode_revisions_ambiguous"].as_bool().unwrap_or(true);
                    return Ok(profile.without_unverified_mode(ambiguous));
                }
            }
        }
    }
    let mut builder = ureq::AgentBuilder::new().tls_config(crate::tls::connector()).timeout(Duration::from_secs(30));
    let configured_proxy = proxy.map(str::to_owned).or_else(|| std::env::var("MIAC_PROXY").ok());
    if let Some(proxy) = configured_proxy.as_deref() { builder = builder.proxy(ureq::Proxy::new(proxy).map_err(|e| e.to_string())?); }
    let agent = builder.build();
    let index = fetch(&agent,"https://miot-spec.org/miot-spec-v2/instances?status=all")?;
    let (selected, ambiguous_revisions) = select_catalog_spec(&index, model)
        .ok_or_else(|| format!("官方规格库未找到空调型号 {model}"))?;
    let urn = selected["type"].as_str().ok_or("官方规格缺少类型")?;
    let encoded: String = urn.bytes().map(|b| if b.is_ascii_alphanumeric() || b"-._~".contains(&b) { (b as char).to_string() } else { format!("%{b:02X}") }).collect();
    let url = format!("https://miot-spec.org/miot-spec-v2/instance?type={encoded}");
    let mut spec = fetch(&agent, &url)?;
    if spec["type"].as_str() != Some(urn) { return Err("官方返回的规格型号与请求不一致".into()); }
    spec["status"] = selected["status"].clone();
    let profile = Profile::parse(model,&spec)?;
    // Cache only public model specifications; never account credentials.
    if std::fs::create_dir_all(cache.parent().unwrap()).is_ok() {
        let _ = std::fs::write(cache, json!({"model":model,"spec":spec,"mode_revisions_ambiguous":ambiguous_revisions}).to_string());
    }
    Ok(profile.without_unverified_mode(ambiguous_revisions))
}

pub fn bundled_specs() -> &'static Value {
    static SPECS: std::sync::OnceLock<Value> = std::sync::OnceLock::new();
    SPECS.get_or_init(|| serde_json::from_str(include_str!("../specs/air-conditioners.json")).expect("invalid bundled MIoT specs"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn official_model_fixtures_have_independent_addresses_and_enums() {
        let ma1 = Profile::parse("xiaomi.aircondition.ma1", &bundled_specs()["xiaomi.aircondition.ma1"]).unwrap();
        let mh4 = Profile::parse("xiaomi.aircondition.mh4", &bundled_specs()["xiaomi.aircondition.mh4"]).unwrap();
        assert_eq!(ma1.addr("targetTemp"), Some((2,3)));
        assert_eq!(mh4.addr("targetTemp"), Some((2,4)));
        assert_eq!(mh4.addr("electricity"), Some((8,1)));
        assert_eq!(Profile::legacy().addr("electricity"), Some((20,1)));
        assert_eq!(ma1.properties["mode"].choices.iter().find(|(_,n)| n == "制冷").unwrap().0, 2);
        assert_eq!(mh4.properties["mode"].choices.iter().find(|(_,n)| n == "制冷").unwrap().0, 2);
        assert!(!mh4.writable("horizontalSwing"));
        assert!(mh4.validate("horizontalSwing", &json!(true)).is_err());
        assert!(mh4.validate("fanLevel", &json!(8)).is_err());
        assert!(mh4.validate("roomTemp", &json!(26)).is_err());
    }

    #[test]
    fn mode_values_come_from_the_selected_spec_not_legacy_constants() {
        let mut spec = bundled_specs()["xiaomi.aircondition.ma1"].clone();
        for service in spec["services"].as_array_mut().unwrap() {
            for property in service["properties"].as_array_mut().into_iter().flatten() {
                if kind(property) == "mode" {
                    property["value-list"] = json!([{"value":1,"description":"Cool"},{"value":9,"description":"Heat"}]);
                }
            }
        }
        let profile = Profile::parse("xiaomi.aircondition.ma1", &spec).unwrap();
        assert_eq!(profile.properties["mode"].choices, vec![(1,"制冷".into()),(9,"制热".into())]);
        assert!(profile.validate("mode", &json!(1)).is_ok());
        assert!(profile.validate("mode", &json!(2)).is_err());
    }

    #[test]
    fn unversioned_models_with_conflicting_mode_enums_do_not_write_mode() {
        let dir = std::env::temp_dir();
        for model in ["xiaomi.aircondition.ma1", "xiaomi.aircondition.ma2", "xiaomi.aircondition.ma4"] {
            let profile = load(model, &dir, None).unwrap();
            assert!(!profile.writable("mode"), "{model}");
            assert!(profile.validate("mode", &json!(2)).is_err());
            assert!(profile.writable("on"));
            assert!(profile.writable("targetTemp"));
        }
    }

    #[test]
    fn catalog_revisions_guard_mode_without_fetching_any_specs() {
        let index = json!({"instances": [
            {"model":"example.airc.multi", "status":"released", "type":"urn:miot-spec-v2:device:air-conditioner:0000A004:multi:1"},
            {"model":"example.airc.multi", "status":"released", "type":"urn:miot-spec-v2:device:air-conditioner:0000A004:multi:2"},
            {"model":"example.airc.multi", "status":"preview", "type":"urn:miot-spec-v2:device:air-conditioner:0000A004:multi:3"},
            {"model":"example.airc.single", "status":"released", "type":"urn:miot-spec-v2:device:air-conditioner:0000A004:single:1"},
            {"model":"example.airc.single", "status":"preview", "type":"urn:miot-spec-v2:device:air-conditioner:0000A004:single:2"},
            {"model":"example.airc.single", "status":"preview", "type":"urn:miot-spec-v2:device:air-conditioner:0000A004:single:3"},
            {"model":"example.airc.preview", "status":"preview", "type":"urn:miot-spec-v2:device:air-conditioner:0000A004:preview:1"},
            {"model":"example.airc.preview", "status":"preview", "type":"urn:miot-spec-v2:device:air-conditioner:0000A004:preview:2"},
            {"model":"example.airc.debug", "status":"debug", "type":"urn:miot-spec-v2:device:air-conditioner:0000A004:debug:1"},
            {"model":"example.airc.debug", "status":"debug", "type":"urn:miot-spec-v2:device:air-conditioner:0000A004:debug:2"},
            {"model":"example.airc.duplicate", "status":"released", "type":"urn:miot-spec-v2:device:air-conditioner:0000A004:duplicate:1"},
            {"model":"example.airc.duplicate", "status":"released", "type":"urn:miot-spec-v2:device:air-conditioner:0000A004:duplicate:1"}
        ]});
        for (model, expected_version, ambiguous) in [
            ("example.airc.multi", ":2", true),
            ("example.airc.single", ":1", false),
            ("example.airc.preview", ":2", true),
            ("example.airc.debug", ":2", true),
            ("example.airc.duplicate", ":1", false),
        ] {
            let (entry, actual_ambiguity) = select_catalog_spec(&index, model).unwrap();
            assert!(entry["type"].as_str().unwrap().ends_with(expected_version), "{model}");
            assert_eq!(actual_ambiguity, ambiguous, "{model}");
            let mut profile = Profile::legacy();
            profile.model = model.into();
            assert_eq!(profile.without_unverified_mode(actual_ambiguity).writable("mode"), !ambiguous, "{model}");
        }
    }

    #[test]
    fn unsigned_values_respect_the_published_width() {
        let mut profile = Profile::legacy();
        profile.properties.get_mut("fanLevel").unwrap().choices.clear();
        profile.properties.get_mut("fanLevel").unwrap().format = "uint8".into();
        for invalid in [json!(-1), json!(256), json!(1.5)] {
            assert!(profile.validate("fanLevel", &invalid).is_err());
        }
        assert_eq!(profile.validate("fanLevel", &json!(255)).unwrap(), json!(255));
    }

    #[test]
    fn target_temperature_rejects_unrecognized_numeric_formats() {
        for format in ["uint", "uint08", "uintgarbage", "uint0", "uint128", "integer", "string"] {
            let mut spec = bundled_specs()["xiaomi.aircondition.ma3"].clone();
            for service in spec["services"].as_array_mut().unwrap() {
                for property in service["properties"].as_array_mut().into_iter().flatten() {
                    if kind(property) == "target-temperature" { property["format"] = json!(format); }
                }
            }
            assert!(Profile::parse("xiaomi.aircondition.ma3", &spec).is_err(), "{format}");
        }
    }

    #[test]
    fn large_integer_writes_keep_the_exact_json_value() {
        let mut profile = Profile::legacy();
        let property = profile.properties.get_mut("fanPercent").unwrap();
        property.format = "uint64".into();
        let exact = json!(9_007_199_254_740_993u64);
        assert_eq!(profile.validate("fanPercent", &exact).unwrap(), exact);
        assert_eq!(profile.validate("fanPercent", &json!(u64::MAX)).unwrap(), json!(u64::MAX));
        assert!(profile.validate("fanPercent", &json!(9_007_199_254_740_994.0)).is_err());

        profile.properties.get_mut("fanPercent").unwrap().format = "int64".into();
        assert_eq!(profile.validate("fanPercent", &json!(i64::MAX)).unwrap(), json!(i64::MAX));
        assert_eq!(profile.validate("fanPercent", &json!(i64::MIN)).unwrap(), json!(i64::MIN));
    }

    #[test]
    fn temperature_snap_stays_on_grid_when_upper_bound_is_unaligned() {
        let mut profile = Profile::legacy();
        profile.properties.get_mut("targetTemp").unwrap().range = Some([16.0, 31.8, 0.5]);
        let snapped = profile.snap_temperature(31.8);
        assert_eq!(snapped, 31.5);
        assert!(profile.validate("targetTemp", &json!(snapped)).is_ok());
        assert!(profile.snap_temperature(f64::NAN).is_nan());
    }

    #[test]
    fn all_bundled_specs_are_parsed_without_model_guessing() {
        let mut supported = 0;
        for (model, spec) in bundled_specs().as_object().unwrap() {
            match Profile::parse(model,spec) {
                Ok(p) => {
                    supported += 1;
                    let [min,max,step] = p.temperature_range();
                    assert!(min < max && step > 0.0);
                    assert!(p.validate("on", &json!(true)).is_ok());
                    let value = p.snap_temperature(26.5);
                    assert!(p.validate("targetTemp", &json!(value)).is_ok(), "{model}: {value}");
                    assert!(p.validate("targetTemp", &json!(max+step)).is_err());
                    println!("supported: {model}; temperature {min}..{max}/{step}");
                }
                Err(error) => println!("unsupported: {model}: {error}"),
            }
        }
        assert_eq!(supported, bundled_specs().as_object().unwrap().len());
    }

    #[test]
    fn reject_non_ac_and_missing_core_capabilities() {
        assert!(Profile::parse("fake.model", &json!({"type":"urn:miot-spec-v2:device:fan:x:y:1"})).is_err());
        assert!(Profile::parse("fake.model", &json!({"type":"urn:miot-spec-v2:device:air-conditioner:x:y:1","services":[]})).is_err());
    }

    #[test]
    fn model_specific_temperature_steps_and_invalid_values() {
        let mut p = Profile::legacy();
        p.properties.get_mut("targetTemp").unwrap().range = Some([16.0, 30.0, 1.0]);
        assert_eq!(p.snap_temperature(26.5), 27.0);
        assert!(p.validate("targetTemp", &json!(26.5)).is_err());
        assert!(p.validate("targetTemp", &json!(31)).is_err());
        assert!(p.validate("on", &json!(1)).is_err());
        assert!(p.validate("mode", &json!(99)).is_err());
        assert!(p.validate("targetTemp", &Value::Null).is_err());
    }
}
