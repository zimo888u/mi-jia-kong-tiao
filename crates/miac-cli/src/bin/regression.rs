//! regression —— 新旧功能回归：把 v1 命令行支持的全部操作在 v2 上真机跑一遍
//!
//! 用途：第 6 阶段要求的「所有现有控制功能通过回归测试」。
//! 这里逐项调用 miac-core 的真实读写路径（局域网优先），打印前后状态对照，
//! 并在结束时把被改动的项复位，避免留下副作用。
//!
//! 用法：
//!   cargo run --release -p miac-cli --bin regression            # 只读项
//!   cargo run --release -p miac-cli --bin regression -- --write # 连写操作一起（会真发指令）
//!
//! 设计取舍：默认**不**执行写操作。设备是真实家电，误改状态会被用户立刻感知；
//! 加 --write 才做，并且每项都先记原值、最后复位。

use miac_core::controller::{Controller, PropValue};
use miac_core::credentials::Credentials;
use miac_core::Transport;

/// 一项测试的结果。
struct Outcome {
    name: &'static str,
    /// 通过 / 未通过 / 跳过
    verdict: Verdict,
    detail: String,
}

#[derive(PartialEq)]
enum Verdict {
    Pass,
    Fail,
    /// 设备按设计拒绝（例如关机时不能调模式）
    Skip,
}

impl Verdict {
    fn mark(&self) -> &'static str {
        match self {
            Verdict::Pass => "✅",
            Verdict::Fail => "❌",
            Verdict::Skip => "⏭",
        }
    }
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let do_write = args.iter().any(|a| a == "--write");

    let creds = Credentials::appdata();
    let mut ctrl = Controller::new(creds, Transport::Auto);

    println!("=== 米家空调 v2 功能回归 ===");
    println!("写操作：{}\n", if do_write { "启用（会真发指令，结束后复位）" } else { "未启用（只读；加 --write 开启）" });

    if let Err(e) = ctrl.init_transport() {
        println!("通道建立失败：{e}");
        std::process::exit(1);
    }
    println!("通道：{}\n", ctrl.link.map(|l| l.label()).unwrap_or("—"));

    let mut results: Vec<Outcome> = Vec::new();

    // ── 1. 状态快照（v1 `status`）──
    match ctrl.snapshot() {
        Ok(s) => {
            let n = s.status.len();
            let expected = ["on", "mode", "targetTemp", "fanLevel", "verticalSwing", "horizontalSwing", "windDirection", "eco", "sleep", "heater", "dryer", "favoriteOn", "roomTemp", "indoorHumidity", "electricity", "faultValue"];
            let complete = expected.iter().filter(|name| ctrl.profile.properties.get(**name).is_some_and(|p| p.readable)).all(|name| s.status.iter().any(|(key, _)| key == name));
            let bad: Vec<String> = s
                .status
                .iter()
                .filter(|(name, v)| ctrl.profile.properties.get(*name).is_some_and(|p| p.readable) && !matches!(v, PropValue::Ok(_)))
                .map(|(k, v)| format!("{k}={}", v.display()))
                .collect();
            results.push(Outcome {
                name: "状态快照（16 项属性）",
                verdict: if complete && n >= 2 && bad.is_empty() { Verdict::Pass } else { Verdict::Fail },
                detail: format!("{n} 项返回，必要属性齐全={complete}；异常：{}", bad.join(", ")),
            });
        }
        Err(e) => results.push(Outcome { name: "状态快照", verdict: Verdict::Fail, detail: e.to_string() }),
    }

    // ── 2. 机器诊断（v1 `diag`）──
    match ctrl.diag() {
        Ok(v) => {
            let expected = ["indoorPipeTemp", "indoorFanSpeed", "outdoorTemp", "outdoorPipeTemp", "compressorFreq", "outdoorCurrent", "outdoorVoltage", "runDuration"];
            let required: Vec<_> = expected.iter().filter(|name| ctrl.profile.properties.get(**name).is_some_and(|p| p.readable)).collect();
            let missing = required.iter().filter(|name| !v.iter().any(|(key, value)| key == **name && matches!(value, PropValue::Ok(_)))).count();
            results.push(Outcome {
                name: "机器诊断（8 项）",
                verdict: if required.is_empty() { Verdict::Skip } else if missing == 0 { Verdict::Pass } else { Verdict::Fail },
                detail: format!("{}/{} 项该型号支持的诊断读到", required.len() - missing, required.len()),
            });
        }
        Err(e) => results.push(Outcome { name: "机器诊断", verdict: Verdict::Fail, detail: e.to_string() }),
    }

    // ── 3. 维护状态（clean / examine / runDuration）──
    match ctrl.maintenance() {
        Ok((v, cleaning)) => {
            let missing = v.iter().filter(|(_, x)| matches!(x, PropValue::Missing)).count();
            results.push(Outcome {
                name: "维护状态（自清洁/体检/累计运行）",
                verdict: if missing == 0 { Verdict::Pass } else { Verdict::Fail },
                detail: format!("自清洁={}，{} 项读到", if cleaning { "运行中" } else { "未运行" }, v.len() - missing),
            });
        }
        Err(e) => results.push(Outcome { name: "维护状态", verdict: Verdict::Fail, detail: e.to_string() }),
    }

    // ── 4. 温湿度计 ──
    let t = ctrl.read_thermometer();
    results.push(Outcome {
        name: "温湿度计",
        verdict: if t.available { Verdict::Pass } else { Verdict::Skip },
        detail: if t.available {
            format!(
                "{:.1} ℃ / {:.0} %{}",
                t.temperature.unwrap_or(0.0),
                t.humidity.unwrap_or(0.0),
                t.battery.map(|b| format!(" / 电量 {b:.0}%")).unwrap_or_default()
            )
        } else {
            t.reason.unwrap_or_else(|| "不可用".into())
        },
    });

    // ── 5. 耗电统计（日/月）──
    match ctrl.power_stats() {
        Ok(p) => results.push(Outcome {
            name: "耗电统计（日/月/年）",
            verdict: if !p.daily.is_empty() { Verdict::Pass } else { Verdict::Fail },
            detail: format!(
                "{} 年 {} 月，今日 {:.1} 度，本月 {:.1} 度，本年 {:.1} 度，{} 天明细",
                p.year, p.month, p.today_energy, p.month_energy, p.year_energy, p.daily.len()
            ),
        }),
        Err(e) => results.push(Outcome { name: "耗电统计", verdict: Verdict::Fail, detail: e.to_string() }),
    }

    // ── 6. 原始属性读写（v1 `raw`）──
    let (on_siid, on_piid) = ctrl.profile.addr("on").expect("initialized AC profile has power");
    match ctrl.raw_read(on_siid, on_piid) {
        Ok(v) => results.push(Outcome {
            name: "原始属性读取（该型号开关）",
            verdict: if matches!(v, PropValue::Ok(_)) { Verdict::Pass } else { Verdict::Fail },
            detail: format!("= {}", v.display()),
        }),
        Err(e) => results.push(Outcome { name: "原始属性读取", verdict: Verdict::Fail, detail: e.to_string() }),
    }

    // Compare addresses against the bundled official specification, independently
    // of the legacy PROPS lookup. Check structural invariants as well.
    let mut issues = Vec::new();
    let mut names = std::collections::BTreeSet::new();
    let mut addresses = std::collections::BTreeSet::new();
    for (name, prop) in &ctrl.profile.properties {
        if !names.insert(name) || !addresses.insert((prop.siid, prop.piid)) || prop.siid == 0 || prop.piid == 0 {
            issues.push(name.clone());
        }
    }
    if let Some(spec) = miac_core::profile::bundled_specs().get(&ctrl.profile.model) {
        match miac_core::profile::Profile::parse(&ctrl.profile.model, spec) {
            Ok(reference) => for (name, prop) in reference.properties {
                if ctrl.profile.addr(&name) != Some((prop.siid, prop.piid)) { issues.push(name); }
            },
            Err(error) => issues.push(error),
        }
    }
    results.push(Outcome { name: "Model specification integrity", verdict: if issues.is_empty() && names.len() >= 3 { Verdict::Pass } else { Verdict::Fail }, detail: format!("{} properties; issues: {:?}", names.len(), issues) });

    // Exercise the same model-specific validator used by production writes.
    let mut validation_issues = Vec::new();
    let [min, max, step] = ctrl.profile.temperature_range();
    for (value, accepted) in [(min, true), (max, true), (min-step, false), (max+step, false), (min+step/2.0, false)] {
        if ctrl.profile.validate("targetTemp", &serde_json::json!(value)).is_ok() != accepted { validation_issues.push(format!("temperature {value}")); }
    }
    if ctrl.profile.validate("on", &serde_json::json!(true)).is_err() || ctrl.profile.validate("on", &serde_json::json!("on")).is_ok() { validation_issues.push("power type".into()); }
    for name in ["mode", "fanLevel"] {
        if let Some(prop) = ctrl.profile.properties.get(name).filter(|p| p.writable) {
            for (value, _) in &prop.choices {
                if ctrl.profile.validate(name, &serde_json::json!(value)).is_err() { validation_issues.push(format!("{name} {value}")); }
            }
            if ctrl.profile.validate(name, &serde_json::json!(-999999)).is_ok() { validation_issues.push(format!("{name} invalid enum")); }
        }
    }
    results.push(Outcome { name: "Production model validation", verdict: if validation_issues.is_empty() { Verdict::Pass } else { Verdict::Fail }, detail: format!("issues: {:?}", validation_issues) });

    // ── 9. 写操作（可选）──
    if do_write {
        // 逐个开关类属性：读原值 → 写反值 → 回读确认 → 复位
        for (name, label) in [
            ("light", "指示灯"),
            ("buzzer", "提示音"),
            ("eco", "ECO 节能"),
            ("sleep", "睡眠模式"),
        ] {
            let before = ctrl.read_props(&[name]).ok().and_then(|v| v.first().and_then(|(_,p)| p.as_bool()));
            let Some(orig) = before else {
                results.push(Outcome { name: label, verdict: Verdict::Fail, detail: "读原值失败".into() });
                continue;
            };
            match ctrl.set_toggle(name, !orig) {
                Ok(()) => {
                    std::thread::sleep(std::time::Duration::from_millis(400));
                    let after = ctrl.read_props(&[name]).ok().and_then(|v| v.first().and_then(|(_,p)| p.as_bool()));
                    // 复位
                    let restored = ctrl.set_toggle(name, orig);
                    let restored_value = if restored.is_ok() {
                        std::thread::sleep(std::time::Duration::from_millis(400));
                        ctrl.read_props(&[name]).ok().and_then(|v| v.first().and_then(|(_,p)| p.as_bool()))
                    } else { None };
                    let reset_ok = restored_value == Some(orig);
                    let same = after == Some(!orig);
                    results.push(Outcome {
                        name: label,
                        verdict: if same && reset_ok { Verdict::Pass } else { Verdict::Fail },
                        detail: format!("{orig} → {}（{}）；{}", !orig,
                            if same { "回读一致" } else { "回读不一致" },
                            if reset_ok { format!("已验证复位 {orig}") } else { format!("复位失败或未确认：{restored:?}，读回 {restored_value:?}") }),
                    });
                }
                Err(e) => {
                    // 设备拒绝（-5000 = 当前状态下不可写，例如关机时不能开 ECO）不是我们的 bug，
                    // v1 的 README 也明确写了这条限制，所以记为「跳过」而不是失败。
                    let refused = e.0.contains("-5000");
                    results.push(Outcome {
                        name: label,
                        verdict: if refused { Verdict::Skip } else { Verdict::Fail },
                        detail: if refused {
                            format!("设备拒绝（{}）：当前状态不可写，与 v1 行为一致", e.0)
                        } else {
                            e.to_string()
                        },
                    });
                }
            }
        }
    }

    // ── 汇总 ──
    println!("{:<28} {:<6} {}", "项目", "结论", "详情");
    println!("{}", "-".repeat(90));
    for r in &results {
        println!("{:<28} {:<6} {}", r.name, r.verdict.mark(), r.detail);
    }
    let failed = results.iter().filter(|r| r.verdict == Verdict::Fail).count();
    let skipped = results.iter().filter(|r| r.verdict == Verdict::Skip).count();
    println!(
        "\n共 {} 项：通过 {}，跳过 {}（设备按设计拒绝），失败 {}。",
        results.len(),
        results.len() - failed - skipped,
        skipped,
        failed
    );
    if !do_write {
        println!("（写操作未测试；加 --write 可连同写入回归一起跑）");
    }
    if failed > 0 {
        std::process::exit(1);
    }
}
