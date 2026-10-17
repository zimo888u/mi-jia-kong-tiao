//! Command-line diagnostics. Parse arguments before reading credentials or connecting.
use miac_core::{controller::Controller, credentials::Credentials, login, miot, Transport};
use std::io::Write;

#[derive(Debug, PartialEq)]
enum Action { Help, Status, Diag, Thermo, Power, Raw(u16, u16), Creds, Login }

fn parse(args: &[String]) -> Result<(Action, Transport), String> {
    let cloud = args.iter().any(|a| a == "--cloud");
    let local = args.iter().any(|a| a == "--local");
    if cloud && local { return Err("不能同时指定 --cloud 和 --local".into()); }
    let words: Vec<_> = args.iter().filter(|a| *a != "--cloud" && *a != "--local").map(String::as_str).collect();
    let action = match words.as_slice() {
        [] | ["status"] => Action::Status,
        ["help" | "--help" | "-h"] => Action::Help,
        ["diag"] => Action::Diag,
        ["thermo"] => Action::Thermo,
        ["power"] => Action::Power,
        ["creds"] => Action::Creds,
        ["login"] => Action::Login,
        ["raw", siid, piid] => {
            let parse_id = |value: &str| value.parse::<u16>().ok().filter(|n| *n != 0)
                .ok_or_else(|| "siid 和 piid 必须是 1~65535 的整数".to_string());
            Action::Raw(parse_id(siid)?, parse_id(piid)?)
        }
        _ => return Err("未知命令或参数；运行 help 查看用法".into()),
    };
    Ok((action, if cloud { Transport::Cloud } else if local { Transport::Local } else { Transport::Auto }))
}

fn run(action: Action, transport: Transport) -> Result<(), String> {
    if action == Action::Help {
        println!("status / diag / thermo / power / raw <siid> <piid> / creds / login；可选 --cloud 或 --local");
        return Ok(());
    }
    let creds = Credentials::appdata();
    println!("凭据目录：{}", creds.primary_dir().display());
    if action == Action::Login { return run_login(&creds); }
    if action == Action::Creds {
        let state = creds.read_login().ok_or("设备凭据无法读取，请扫码登录")?;
        println!("设备：{}  型号：{}  did={}  局域网条件={}  云会话={}",
            state.device.name.as_deref().unwrap_or(""), state.device.model.as_deref().unwrap_or(""),
            state.device.did, state.device.local_ready(), state.session.ready());
        println!("温湿度计：{}", if state.thermometer.did.is_empty() { "未关联" } else { "已关联" });
        return Ok(());
    }
    let mut ctrl = Controller::new(creds, transport);
    if action == Action::Thermo {
        let t = ctrl.read_thermometer();
        if !t.available { return Err(t.reason.unwrap_or_else(|| "温湿度计没有有效读数".into())); }
        println!("名称={} 温度={:?} 湿度={:?} 电量={:?}", t.name, t.temperature, t.humidity, t.battery);
        return Ok(());
    }
    let link = ctrl.init_transport().map_err(|e| e.to_string())?;
    println!("通道就绪：{}", link.label());
    match action {
        Action::Status | Action::Diag => {
            let values = if action == Action::Status {
                let snapshot = ctrl.snapshot().map_err(|e| e.to_string())?;
                println!("故障：{}", snapshot.fault.text);
                snapshot.status
            } else { ctrl.diag().map_err(|e| e.to_string())? };
            for (name, value) in values {
                let address = ctrl.profile.addr(name).map(|(s,p)| format!("{s}.{p}")).unwrap_or_else(|| "—".into());
                println!("{name:<20} {address:<10} {}", value.display());
            }
        }
        Action::Power => {
            let p = ctrl.power_stats().map_err(|e| e.to_string())?;
            println!("{} 年 {} 月：今日 {:.1} 度 / 本月 {:.1} 度 / 本年 {:.1} 度", p.year, p.month, p.today_energy, p.month_energy, p.year_energy);
            println!("今日用时 {} 分钟", p.today_minutes);
            for (day, energy, minutes) in p.daily { println!("{day:>2} 日  {energy:>6.1} 度  {minutes:>5} 分钟"); }
        }
        Action::Raw(siid, piid) => {
            let value = ctrl.raw_read(siid, piid).map_err(|e| e.to_string())?;
            if !matches!(value, miac_core::controller::PropValue::Ok(_)) { return Err(format!("读 {siid}.{piid} 失败：{}", value.display())); }
            println!("{siid}.{piid} = {}", value.display());
        }
        _ => unreachable!("handled before connection"),
    }
    Ok(())
}

fn run_login(creds: &Credentials) -> Result<(), String> {
    let mut session = login::LoginSession::new(None);
    println!("正在向小米申请二维码…");
    let challenge = session.create_challenge().map_err(|e| e.to_string())?;
    let suffix = if challenge.mime == "image/png" { ".png" } else { ".jpg" };
    // Exclusive random temporary file; Drop cleans every return path.
    let mut qr = tempfile::Builder::new().prefix("miac-login-qr-").suffix(suffix).tempfile().map_err(|e| e.to_string())?;
    qr.write_all(&challenge.image).and_then(|_| qr.flush()).map_err(|e| e.to_string())?;
    println!("二维码：{}；请用米家 App 扫码并确认。", qr.path().display());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        let executable = miac_core::io_limits::system_executable("rundll32.exe").map_err(|e| e.to_string())?;
        std::process::Command::new(executable).arg("url.dll,FileProtocolHandler").arg(qr.path())
            .creation_flags(0x08000000).spawn().map_err(|e| format!("打开二维码失败：{e}"))?;
    }
    let result = session.wait_for_approval(&challenge, || true).map_err(|e| e.to_string())?;
    let session_data = result.to_session(miot::COUNTRY);
    let devices = login::fetch_devices(&session_data, None).map_err(|e| e.to_string())?;
    let index = devices.iter().position(|d| d.looks_like_ac()).ok_or("账号下没有可控制的空调")?;
    let (device, sensor) = login::save_credentials(creds, &session_data, &devices, index)?;
    println!("登录完成：{device}；温湿度计：{}", sensor.as_deref().unwrap_or("未关联，已清除旧账号关联"));
    Ok(())
}

fn main() {
    let args: Vec<_> = std::env::args().skip(1).collect();
    if let Err(error) = parse(&args).and_then(|(action, transport)| run(action, transport)) {
        eprintln!("{error}"); std::process::exit(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn args(values: &[&str]) -> Vec<String> { values.iter().map(|s| (*s).into()).collect() }
    #[test]
    fn rejects_invalid_commands_before_credential_or_network_access() {
        for words in [&["typo"][..], &["raw"], &["raw", "x", "1"], &["raw", "0", "1"], &["status", "extra"], &["--cloud", "--local", "status"]] {
            assert!(parse(&args(words)).is_err());
        }
        assert_eq!(parse(&args(&["--local", "raw", "3", "5"])).unwrap(), (Action::Raw(3,5), Transport::Local));
    }
}
