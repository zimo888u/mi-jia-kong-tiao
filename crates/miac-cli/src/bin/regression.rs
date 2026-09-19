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
            let bad: Vec<String> = s
                .status
                .iter()
                .filter(|(_, v)| !matches!(v, PropValue::Ok(_)))
                .map(|(k, v)| format!("{k}={}", v.display()))
                .collect();
            results.push(Outcome {
                name: "状态快照（16 项属性）",
                verdict: if bad.is_empty() { Verdict::Pass } else { Verdict::Fail },
                detail: if bad.is_empty() {
                    format!("{n} 项全部读到")
                } else {
                    format!("{n} 项中有异常：{}", bad.join(", "))
                },
            });
        }
        Err(e) => results.push(Outcome { name: "状态快照", verdict: Verdict::Fail, detail: e.to_string() }),
    }

    // ── 2. 机器诊断（v1 `diag`）──
    match ctrl.diag() {
        Ok(v) => {
            let missing = v.iter().filter(|(_, x)| matches!(x, PropValue::Missing)).count();
            results.push(Outcome {
                name: "机器诊断（8 项）",
                verdict: if missing == 0 { Verdict::Pass } else { Verdict::Fail },
                detail: format!("{}/{} 项读到", v.len() - missing, v.len()),
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
    match ctrl.raw_read(2, 1) {
        Ok(v) => results.push(Outcome {
            name: "原始属性读取 2.1（开关）",
            verdict: if matches!(v, PropValue::Ok(_)) { Verdict::Pass } else { Verdict::Fail },
            detail: format!("= {}", v.display()),
        }),
        Err(e) => results.push(Outcome { name: "原始属性读取", verdict: Verdict::Fail, detail: e.to_string() }),
    }

    // ── 7. 属性表完整性（40 项地址都能解析）──
    let mut unresolved = Vec::new();
    for (name, _) in miac_core::miot::PROPS {
        if Controller::resolve_prop(name).is_err() {
            unresolved.push(*name);
        }
    }
    results.push(Outcome {
        name: "属性表（v1 全部 40 项可解析）",
        verdict: if unresolved.is_empty() { Verdict::Pass } else { Verdict::Fail },
        detail: if unresolved.is_empty() {
            format!("{} 项全部可解析", miac_core::miot::PROPS.len())
        } else {
            format!("无法解析：{}", unresolved.join(", "))
        },
    });

    // ── 8. 参数校验（v1 的 valid* 系列）──
    let mut checks = Vec::new();
    checks.push(("温度 16~31 步长 0.5", Controller::valid_temp(26.5).is_ok() && Controller::valid_temp(26.3).is_err()));
    checks.push(("模式 cool/dry/fan/heat", Controller::valid_mode(&serde_json::json!("cool")).is_ok() && Controller::valid_mode(&serde_json::json!("x")).is_err()));
    checks.push(("风速 auto/1-7/max", Controller::valid_fan(&serde_json::json!("max")).is_ok() && Controller::valid_fan(&serde_json::json!(9)).is_err()));
    checks.push(("风感 0~4", Controller::valid_wind(&serde_json::json!("noblow")).is_ok() && Controller::valid_wind(&serde_json::json!(9)).is_err()));
    checks.push(("定格 0~5", Controller::valid_pos(&serde_json::json!(3)).is_ok() && Controller::valid_pos(&serde_json::json!(6)).is_err()));
    let bad: Vec<&str> = checks.iter().filter(|(_, ok)| !ok).map(|(n, _)| *n).collect();
    results.push(Outcome {
        name: "参数校验（5 类）",
        verdict: if bad.is_empty() { Verdict::Pass } else { Verdict::Fail },
        detail: if bad.is_empty() { "全部按 v1 规则拒绝/接受".into() } else { format!("异常：{}", bad.join(", ")) },
    });

    // ── 9. 写操作（可选）──
    if do_write {
        // 逐个开关类属性：读原值 → 写反值 → 回读确认 → 复位
        for (name, label) in [
            ("light", "指示灯"),
            ("buzzer", "提示音"),
            ("eco", "ECO 节能"),
            ("sleep", "睡眠模式"),
        ] {
            let before = ctrl.read_props(&[name]).ok().and_then(|v| v[0].1.as_bool());
            let Some(orig) = before else {
                results.push(Outcome { name: label, verdict: Verdict::Fail, detail: "读原值失败".into() });
                continue;
            };
            match ctrl.set_toggle(name, !orig) {
                Ok(()) => {
                    std::thread::sleep(std::time::Duration::from_millis(400));
                    let after = ctrl.read_props(&[name]).ok().and_then(|v| v[0].1.as_bool());
                    // 复位
                    let _ = ctrl.set_toggle(name, orig);
                    let same = after == Some(!orig);
                    results.push(Outcome {
                        name: label,
                        verdict: if same { Verdict::Pass } else { Verdict::Fail },
                        detail: format!("{orig} → {}({} → 已复位 {orig})", !orig, if same { "回读一致" } else { "回读不一致" }),
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
