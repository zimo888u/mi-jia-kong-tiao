//! miac-cli —— 用真实凭据跑一遍控制核心，打印从空调读到的实际数据
//!
//! 存在的意义：图形界面里看不到控制核心的真实往返结果（只能看提示条），
//! 排查「属性表对不对、通道通不通、加密有没有算错」时，需要一个能直接
//! 打印原始值的工具。它复用 miac-core，和界面走完全相同的代码路径。
//!
//! 用法：
//!   cargo run --release -p miac-cli -- status      读状态快照
//!   cargo run --release -p miac-cli -- diag        读机器诊断
//!   cargo run --release -p miac-cli -- thermo      读温湿度计
//!   cargo run --release -p miac-cli -- creds       只看凭据解析结果（不联网）
//!   cargo run --release -p miac-cli -- raw 2 1     读指定 siid.piid
//!   cargo run --release -p miac-cli -- login       扫码登录（出图 → 等确认 → 写凭据）

use miac_core::controller::Controller;
use miac_core::credentials::Credentials;
use miac_core::login;
use miac_core::miot;
use miac_core::settings::Settings;
use miac_core::Transport;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let cmd = args.first().map(String::as_str).unwrap_or("status");

    // 与界面用同一套凭据定位逻辑（主目录 + 回退）
    let creds = Credentials::appdata();
    println!("凭据目录：{}", creds.primary_dir().display());
    for f in miac_core::credentials::CREDENTIAL_FILES {
        match creds.locate(f) {
            Some(p) => println!("  ✓ {f}  →  {}", p.display()),
            None => println!("  ✗ {f}  未找到"),
        }
    }

    let settings = Settings::load(&creds);
    println!(
        "设置：主题={} 通道={} 预设={:?} 已迁移={}",
        if settings.dark { "深色" } else { "浅色" },
        settings.transport().label(),
        settings.presets,
        settings.migrated
    );

    // ── 扫码登录（独立分支：不依赖已有凭据）──
    if cmd == "login" {
        run_login(&creds);
        return;
    }

    if cmd == "creds" {
        // 只验证凭据能否解析（含 DPAPI 解密路径），不联网
        match creds.read_device() {
            Some(d) => println!(
                "\n设备：name={:?} model={:?}\n  did={}\n  localip={:?} token={}",
                d.name,
                d.model,
                d.did,
                d.localip,
                if d.token.is_some() { "已保存" } else { "缺失" }
            ),
            None => println!("\ndevice.json 解析失败"),
        }
        match creds.read_session() {
            Some(s) => println!(
                "云会话：ready={} userId={} country={:?}",
                s.ready(),
                s.user_id,
                s.country
            ),
            None => println!("云会话：解析失败"),
        }
        match creds.read_thermometer() {
            Some(t) => println!("温湿度计：did={} model={:?}", t.did, t.model),
            None => println!("温湿度计：解析失败"),
        }
        return;
    }

    // 通道可用 --cloud / --local 覆盖，默认自动（局域网优先，与界面一致）
    let transport = if args.iter().any(|a| a == "--cloud") {
        Transport::Cloud
    } else if args.iter().any(|a| a == "--local") {
        Transport::Local
    } else {
        Transport::Auto
    };
    let mut ctrl = Controller::new(creds, transport);
    println!("\n正在建立通道…");
    match ctrl.init_transport() {
        Ok(link) => println!("通道就绪：{}", link.label()),
        Err(e) => {
            println!("通道建立失败：{e}");
            std::process::exit(1);
        }
    }

    match cmd {
        "status" => {
            let t0 = std::time::Instant::now();
            match ctrl.snapshot() {
                Ok(s) => {
                    println!("\n读状态耗时 {:.2} 秒", t0.elapsed().as_secs_f64());
                    println!("故障：{}", s.fault.text);
                    println!("\n{:<20} {:<10} {}", "属性", "siid.piid", "值");
                    println!("{}", "-".repeat(52));
                    for (name, v) in &s.status {
                        let addr = miot::prop_addr(name)
                            .map(|(a, b)| format!("{a}.{b}"))
                            .unwrap_or_else(|| "—".into());
                        println!("{:<20} {:<10} {}", name, addr, v.display());
                    }
                }
                Err(e) => println!("读状态失败：{e}"),
            }
        }
        "diag" => {
            let t0 = std::time::Instant::now();
            match ctrl.diag() {
                Ok(v) => {
                    println!("\n读诊断耗时 {:.2} 秒", t0.elapsed().as_secs_f64());
                    for (name, val) in &v {
                        let addr = miot::prop_addr(name)
                            .map(|(a, b)| format!("{a}.{b}"))
                            .unwrap_or_else(|| "—".into());
                        println!("{:<20} {:<10} {}", name, addr, val.display());
                    }
                }
                Err(e) => println!("读诊断失败：{e}"),
            }
        }
        "thermo" => {
            let t = ctrl.read_thermometer();
            println!("\n可用={} 名称={}", t.available, t.name);
            println!(
                "温度={:?} 湿度={:?} 电量={:?}",
                t.temperature, t.humidity, t.battery
            );
            if let Some(r) = t.reason {
                println!("原因：{r}");
            }
        }
        "power" => match ctrl.power_stats() {
            Ok(p) => {
                println!(
                    "\n{} 年 {} 月：今日 {:.1} 度 / 本月 {:.1} 度 / 本年 {:.1} 度",
                    p.year, p.month, p.today_energy, p.month_energy, p.year_energy
                );
                println!("今日用时 {} 分钟", p.today_minutes);
                println!("当月首日星期 {}（0=周日），本月 {} 天", p.first_weekday, p.days_in_month);
                println!("\n每日电量：");
                for (d, e, m) in &p.daily {
                    println!("  {d:>2} 日  {e:>6.1} 度  {m:>5} 分钟");
                }
            }
            Err(e) => println!("读电量失败：{e}"),
        },
        "raw" => {
            let siid: u16 = args.get(1).and_then(|s| s.parse().ok()).unwrap_or(2);
            let piid: u16 = args.get(2).and_then(|s| s.parse().ok()).unwrap_or(1);
            match ctrl.raw_read(siid, piid) {
                Ok(v) => println!("\n{siid}.{piid} = {}", v.display()),
                Err(e) => println!("读 {siid}.{piid} 失败：{e}"),
            }
        }
        other => {
            println!("未知子命令：{other}");
            println!("可用：status / diag / thermo / power / raw <siid> <piid> / creds / login");
        }
    }
}

/// 扫码登录：出二维码图片 → 等手机确认 → 拉设备列表 → 写凭据。
///
/// 命令行版把二维码写到文件并自动打开图片查看器；图形版则直接显示在窗口里
/// （两边共用 `miac_core::login`，协议实现只有一份）。
fn run_login(creds: &Credentials) {
    println!("\n=== 扫码登录 ===");

    let mut session = login::LoginSession::new(None);
    println!("正在向小米申请二维码…");
    let challenge = match session.create_challenge() {
        Ok(c) => c,
        Err(e) => {
            println!("生成二维码失败：{e}");
            return;
        }
    };

    let ext = if challenge.mime == "image/png" { "png" } else { "jpg" };
    let qr_path = std::env::current_dir()
        .unwrap_or_default()
        .join(format!("login-qr.{ext}"));
    if let Err(e) = std::fs::write(&qr_path, &challenge.image) {
        println!("写二维码图片失败：{e}");
        return;
    }
    println!("二维码已保存：{}", qr_path.display());
    println!("请用已登录该空调账号的米家 App 扫码，并在手机上确认。");
    println!("（等确认中，无需回车…）");

    // 用系统默认图片查看器打开二维码
    #[cfg(windows)]
    {
        let _ = std::process::Command::new("rundll32.exe")
            .arg("url.dll,FileProtocolHandler")
            .arg(&qr_path)
            .spawn();
    }

    let mut tries = 0u32;
    let result = match session.wait_for_approval(&challenge, || {
        tries += 1;
        if tries % 10 == 1 {
            print!(".");
            use std::io::Write;
            let _ = std::io::stdout().flush();
        }
        true
    }) {
        Ok(r) => r,
        Err(e) => {
            println!("\n登录失败：{e}");
            let _ = std::fs::remove_file(&qr_path);
            return;
        }
    };
    println!("\n扫码确认成功，userId={}", result.user_id);

    // 1) 写云会话（只存 RPC 需要的四项，不存密码）
    let session_data = result.to_session(miot::COUNTRY);
    match creds.write_json(miac_core::credentials::FILE_SESSION, &session_data) {
        Ok(p) => println!("已写入云会话：{}", p.display()),
        Err(e) => {
            println!("写 cloud-session.json 失败：{e}");
            return;
        }
    }

    // 2) 拉设备列表，找出空调与温湿度计
    println!("正在读取设备列表…");
    let devices = match login::fetch_devices(&session_data, None) {
        Ok(d) => d,
        Err(e) => {
            println!("读取设备列表失败：{e}");
            return;
        }
    };
    println!("账号下共 {} 个设备。", devices.len());

    let acs: Vec<_> = devices.iter().filter(|d| d.looks_like_ac()).collect();
    if acs.is_empty() {
        println!("没有找到空调设备（型号/名称里都没有空调特征）");
        return;
    }
    println!("\n找到的空调：");
    for (i, d) in acs.iter().enumerate() {
        println!(
            "  [{}] {}  model={}  did={}  {}  token={}",
            i + 1,
            d.name,
            d.model,
            d.did,
            d.localip.as_deref().unwrap_or("无局域网IP"),
            if d.token.is_some() { "有" } else { "无" }
        );
    }

    // 命令行版默认取第一个；图形版会让用户选
    let ac = acs[0];
    println!("\n选用第一个：{}（{}）", ac.name, ac.model);

    let device_json = serde_json::json!({
        "name": ac.name,
        "model": ac.model,
        "did": ac.did,
        "localip": ac.localip,
        "token": ac.token,
        "savedAt": login::now_iso8601(),
    });
    match creds.write_json(miac_core::credentials::FILE_DEVICE, &device_json) {
        Ok(p) => println!("已写入设备信息：{}", p.display()),
        Err(e) => {
            println!("写 device.json 失败：{e}");
            return;
        }
    }

    // 3) 顺手记下温湿度计（有才写）
    if let Some(t) = devices.iter().find(|d| d.is_thermometer()) {
        let tj = serde_json::json!({
            "name": t.name,
            "model": t.model,
            "did": t.did,
            "savedAt": login::now_iso8601(),
        });
        match creds.write_json(miac_core::credentials::FILE_THERMOMETER, &tj) {
            Ok(p) => println!("已写入温湿度计：{}", p.display()),
            Err(e) => println!("写 thermometer.json 失败：{e}"),
        }
    } else {
        println!("未发现米家智能温湿度计 3，保留原有配置。");
    }

    let _ = std::fs::remove_file(&qr_path);
    println!("\n登录完成。可以执行 status 读一次设备状态。");
}
