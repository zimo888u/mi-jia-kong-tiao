//! migration.rs —— 首次启动的自动迁移
//!
//! v1（Electron 版）把状态分散在三处，v2 首次启动要把它们接过来：
//!
//! | 内容 | v1 位置 | 说明 |
//! |---|---|---|
//! | `device.json` | `%APPDATA%\米家空调\` 或项目目录 | 空调 did/token/localip |
//! | `cloud-session.json` | 同上 | 云端会话 |
//! | `thermometer.json` | 同上 | 温湿度计 |
//! | 明暗主题 | `%APPDATA%\米家空调\Local Storage\leveldb\*.ldb` | ⚠ LevelDB，非 JSON |
//! | 预设温度 | v1 写死在 `lib/miot.js` 的 `TEMP_PRESETS` | 27.5 / 27.0 / 26.5 |
//! | 通信通道 | v1 存在渲染进程内存里，不落盘 | 默认「自动」 |
//!
//! ## 关于主题（唯一的难点）
//!
//! v1 的主题存在渲染进程的 `localStorage` 里，而 Chromium 的 localStorage
//! 落盘是 **LevelDB** 格式，不是 JSON。要完整解析 LevelDB 得引入一整个
//! 存储引擎，为读一个布尔值不值得。
//!
//! 这里的做法：把 `.ldb` / `.log` 文件按字节扫一遍，找 `theme` 键名后面
//! 紧跟的 `dark` / `light` ASCII 值。LevelDB 会把键和值以明文存在数据块里，
//! 所以这种「朴素扫描」对这么短的字符串是有效的。
//!
//! 扫不到就退回默认（深色）——主题本来就是用户随手能切的，迁移不到不致命，
//! 但能不声不响地继承过来体验更好。

use std::path::{Path, PathBuf};

use crate::credentials::{Credentials, CREDENTIAL_FILES};
use crate::settings::{Settings, FILE_SETTINGS};

/// 迁移过程中做完的每一件事，供界面展示「已从旧版继承了哪些东西」。
#[derive(Debug, Clone, PartialEq)]
pub enum Migrated {
    /// 复制了一个凭据文件（文件名, 来源目录）
    Credential { file: String, from: PathBuf },
    /// 继承到旧版的明暗主题
    Theme { dark: bool },
    /// 应用了温度预设
    Presets { values: Vec<f64> },
    /// 从旧版设置文件继承了通信通道
    Transport { index: i32 },
}

impl Migrated {
    /// 给界面看的一句话说明。
    pub fn describe(&self) -> String {
        match self {
            Migrated::Credential { file, from } => {
                format!("已继承旧版凭据 {file}（来自 {}）", from.display())
            }
            Migrated::Theme { dark } => {
                format!("已继承旧版主题：{}", if *dark { "深色" } else { "浅色" })
            }
            Migrated::Presets { values } => {
                let list: Vec<String> = values.iter().map(|v| format!("{v:.1}")).collect();
                format!("已应用温度预设 {}", list.join(" / "))
            }
            Migrated::Transport { index } => {
                let name = match index {
                    1 => "强制局域网",
                    2 => "强制云端",
                    _ => "自动（局域网优先）",
                };
                format!("已继承通信通道：{name}")
            }
        }
    }
}

/// 迁移结果。
#[derive(Debug, Default)]
pub struct MigrationReport {
    /// 做了哪些事
    pub actions: Vec<Migrated>,
    /// 最终可用的凭据目录（主目录）
    pub target_dir: PathBuf,
    /// 是否真的执行了迁移流程（false = 之前已经迁过，直接跳过）
    pub ran: bool,
}

impl MigrationReport {
    pub fn summary(&self) -> String {
        if !self.ran {
            return "无需迁移（此前已完成）".into();
        }
        if self.actions.is_empty() {
            return "首次运行，未发现旧版数据".into();
        }
        self.actions
            .iter()
            .map(|a| a.describe())
            .collect::<Vec<_>>()
            .join("；")
    }
}

/// 执行首次迁移。
///
/// `settings` 会被就地修改（写入继承到的主题/预设/通道），
/// 由调用方决定何时 `save`。
pub fn run(creds: &Credentials, settings: &mut Settings) -> MigrationReport {
    let mut report = MigrationReport {
        target_dir: creds.primary_dir().to_path_buf(),
        ..Default::default()
    };

    // 已经迁过就不再重复（但凭据本身仍会被 credentials 层正常读到）
    if settings.migrated && creds.exists(FILE_SETTINGS) {
        settings.normalize();
        return report;
    }

    report.ran = true;

    // ── 1. 凭据：从旧位置复制到主目录 ──────────────────────────
    // credentials 的读取本来就会回退查找旧目录，所以即使不复制也能用；
    // 这里显式复制一份，是为了让 v2 之后能独立写入（例如 DPAPI 加密版）。
    for file in CREDENTIAL_FILES {
        let Some(src) = creds.locate(file) else { continue };
        let dst = creds.primary_dir().join(file);

        // 已经在主目录里就不用动
        if src == dst {
            continue;
        }

        let Some(from_dir) = src.parent().map(Path::to_path_buf) else { continue };
        if std::fs::create_dir_all(creds.primary_dir()).is_err() {
            continue;
        }
        // 复制后的凭据必须立即收紧权限。失败时移除这份新副本，继续从旧位置只读，
        // 绝不能为了迁移而在新目录留下权限未知的敏感文件。
        match std::fs::copy(&src, &dst) {
            Ok(_) => {
                #[cfg(windows)]
                if let Err(e) = crate::credentials::harden_permissions(&dst) {
                    let _ = std::fs::remove_file(&dst);
                    eprintln!("[迁移] 收紧 {file} 权限失败，已移除新副本：{e}");
                    continue;
                }
                report.actions.push(Migrated::Credential { file: file.to_string(), from: from_dir });
            }
            Err(e) => {
                eprintln!("[迁移] 复制 {file} 失败（不影响读取）：{e}");
            }
        }
    }

    // ── 2. 明暗主题：从 Chromium localStorage 里扫 ──────────────
    if let Some(dark) = read_v1_theme() {
        settings.dark = dark;
        report.actions.push(Migrated::Theme { dark });
    }

    // ── 3. 温度预设：v1 是写死的，直接沿用同一组 ────────────────
    // 只有当用户没改过（还是默认值）时才「继承」，否则会覆盖用户的新设置
    let defaults = Settings::default().presets;
    if settings.presets == defaults {
        report.actions.push(Migrated::Presets { values: settings.presets.clone() });
    }

    // ── 4. 通信通道：v1 不落盘，若有旧 settings.json 则读 ───────
    if let Some(old) = read_v1_settings_json(creds) {
        if (0..=2).contains(&old.transport) {
            settings.transport = old.transport;
            report.actions.push(Migrated::Transport { index: old.transport });
        }
    }

    settings.migrated = true;
    settings.normalize();

    if let Err(e) = settings.save(creds) {
        eprintln!("[迁移] 保存 settings.json 失败：{e}");
    }

    report
}

/// v1 的 userData 目录（也就是 v2 的主目录）。
fn v1_user_data_dir() -> Option<PathBuf> {
    crate::credentials::user_data_dir()
}

/// 从 Chromium 的 localStorage（LevelDB）里扫出 v1 记的主题。
///
/// 朴素扫描的理由见模块头注释：为读一个布尔值引入完整 LevelDB 解析不划算，
/// 而 LevelDB 的键值在数据块里是明文，短字符串扫描足够可靠。
///
/// 返回值：`Some(true)` = 深色，`Some(false)` = 浅色，`None` = 没找到。
pub fn read_v1_theme() -> Option<bool> {
    let base = v1_user_data_dir()?.join("Local Storage").join("leveldb");
    read_theme_from_leveldb(&base)
}

/// 在给定目录里扫 LevelDB 文件找主题值（抽出来便于测试）。
pub fn read_theme_from_leveldb(dir: &Path) -> Option<bool> {
    let entries = std::fs::read_dir(dir).ok()?;

    // v1 存的主题键形如 `ac-ctl:theme`（也兼容单纯 `theme`）
    const KEYS: [&str; 2] = ["ac-ctl:theme", "theme"];
    const VALUES: [(&str, bool); 2] = [("light", false), ("dark", true)];

    for entry in entries.flatten() {
        let path = entry.path();
        let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
        if !matches!(ext, "ldb" | "log") {
            continue;
        }
        let Ok(bytes) = std::fs::read(&path) else { continue };

        for key in KEYS {
            let kb = key.as_bytes();
            let mut start = 0usize;
            while let Some(pos) = find(&bytes[start..], kb) {
                let after = start + pos + kb.len();
                // 键后面若干字节内找值（LevelDB 的块结构会插入少量元数据）
                let window_end = (after + 24).min(bytes.len());
                let window = &bytes[after..window_end];
                for (needle, dark) in VALUES {
                    if find(window, needle.as_bytes()).is_some() {
                        return Some(dark);
                    }
                }
                start = after;
            }
        }
    }
    None
}

/// 在 haystack 里找 needle 的首次出现位置。
fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || haystack.len() < needle.len() {
        return None;
    }
    haystack.windows(needle.len()).position(|w| w == needle)
}

/// 读一个「旧版 settings.json」——v2 自己之前写的也算。
///
/// 存在的意义：迁移函数需要知道用户上次选的通道，而 v1 并不落盘通道，
/// 所以这里读到的一般是 v2 自己的文件；保留这层是为了将来键名变化时
/// 仍能把旧值搬过来。
fn read_v1_settings_json(creds: &Credentials) -> Option<Settings> {
    creds.read_json::<Settings>(FILE_SETTINGS)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn find_works() {
        assert_eq!(find(b"hello world", b"world"), Some(6));
        assert_eq!(find(b"hello", b"zzz"), None);
        assert_eq!(find(b"", b"a"), None);
        assert_eq!(find(b"abc", b""), None);
    }

    #[test]
    fn theme_scanner_finds_dark() {
        let dir = std::env::temp_dir().join("miac-test-theme-dark");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        // 造一个像 LevelDB 数据块的假文件：键名后跟明文值
        let mut f = std::fs::File::create(dir.join("000003.ldb")).unwrap();
        f.write_all(b"\x00\x01\x02ac-ctl:theme\x01\x06dark\x00\x00junkjunk").unwrap();
        drop(f);

        assert_eq!(read_theme_from_leveldb(&dir), Some(true));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn theme_scanner_finds_light() {
        let dir = std::env::temp_dir().join("miac-test-theme-light");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        let mut f = std::fs::File::create(dir.join("000005.log")).unwrap();
        f.write_all(b"randomtheme\x00\x01light\xff\xff").unwrap();
        drop(f);

        assert_eq!(read_theme_from_leveldb(&dir), Some(false));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn theme_scanner_returns_none_when_absent() {
        let dir = std::env::temp_dir().join("miac-test-theme-none");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("000004.ldb"), b"nothing interesting here").unwrap();

        assert_eq!(read_theme_from_leveldb(&dir), None);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn migration_is_idempotent() {
        // 已经迁过的情况下第二次调用应该直接跳过：ran=false，不做事。
        // 注意用 Credentials::isolated：普通构造器会挂上 %APPDATA% 等回退目录，
        // 测试会读到本机真实凭据，断言就随开发机状态飘了。
        let mut s = Settings::default();
        s.migrated = true;

        let dir = std::env::temp_dir().join("miac-test-mig-idempotent");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        // 写一个 settings.json，让「已迁移」这个判断有据可依
        let creds = Credentials::isolated(&dir);
        creds.write_json(FILE_SETTINGS, &s).unwrap();

        let report = run(&creds, &mut s);
        assert!(!report.ran, "已迁过就不该再跑一遍");
        assert!(report.actions.is_empty(), "不该重复做事");
        assert!(report.summary().contains("已完成"));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn report_summary_is_readable() {
        let mut r = MigrationReport { ran: true, ..Default::default() };
        assert_eq!(r.summary(), "首次运行，未发现旧版数据");
        r.actions.push(Migrated::Theme { dark: true });
        assert!(r.summary().contains("深色"));
        r.actions.push(Migrated::Presets { values: vec![27.5, 27.0, 26.5] });
        assert!(r.summary().contains("27.5"));
    }
}
