//! credentials.rs —— 凭据文件的定位、读取与安全写入
//!
//! 移植自 v1 `lib/credentials.js`，行为逐条对齐，保证 v1 → v2 无缝复用已有凭据：
//!
//! - 三个文件：`device.json`（did/token/localip）、`cloud-session.json`、
//!   `thermometer.json`
//! - 写入目录：默认项目目录；打包版指向 `%APPDATA%\米家空调\`
//! - **读取时先找主目录，再回退旧目录**，所以从命令行版配好之后装上图形版
//!   会自动复用，不用重新登录
//! - 写密文时先设 0600，再用 icacls 断掉继承权限，只留当前用户
//!
//! ## 与 v1 的差别：DPAPI
//!
//! v1 只靠文件权限保护明文 JSON。v2 在此基础上支持 Windows DPAPI：
//! 用 `CryptProtectData` 把内容加密成只能由**当前 Windows 用户 + 当前电脑**
//! 解开的密文。DPAPI 的密文里绑定了用户 SID 与机器密钥，拷到别的机器或
//! 别的账户下都解不开，比单纯的文件权限强一档。
//!
//! 兼容策略（重要）：**读的时候两种格式都认**——
//!   1. 先当明文 JSON 解析（v1 留下的文件）
//!   2. 失败则当 DPAPI 密文解（v2 写的文件）
//! 写的时候默认写明文（保持与 v1 互相可读），只有显式调用
//! `write_encrypted` 才写 DPAPI 密文。这样新旧两版可以并存测试，
//! 直到回归验证通过再统一切换到加密存储。

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use crate::cloud::CloudSession;

/// 凭据文件名（装敏感信息，将来用 DPAPI 加密）。
pub const FILE_DEVICE: &str = "device.json";
pub const FILE_SESSION: &str = "cloud-session.json";
pub const FILE_THERMOMETER: &str = "thermometer.json";
/// 应用设置（非敏感：主题、预设温度、通道）。与凭据同目录，便于一起迁移。
pub const FILE_SETTINGS: &str = "settings.json";

/// 三个敏感凭据文件（DPAPI 加密与 icacls 收紧权限只针对它们）。
pub const CREDENTIAL_FILES: [&str; 3] = [FILE_DEVICE, FILE_SESSION, FILE_THERMOMETER];

/// 全部允许写入的文件名。
///
/// 这是个白名单：`write_bytes` 拒绝名单外的名字，避免调用方拼错或
/// 传入用户可控的字符串时把文件写到别处。
pub const ALL_FILES: [&str; 4] = [
    FILE_DEVICE,
    FILE_SESSION,
    FILE_THERMOMETER,
    FILE_SETTINGS,
];

/// `device.json` —— 空调的 did / token / localip。
///
/// 字段名与 v1 完全一致；用 `#[serde(default)]` 容忍缺字段，
/// 这样 v1 写的文件里多余或缺少的键都不会导致解析失败。
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, Default)]
pub struct DeviceInfo {
    /// 设备名（界面显示用）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// 机型标识，用于校验是否为目标机型
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// 设备 did（云端与局域网 RPC 都要）
    #[serde(default)]
    pub did: String,
    /// 局域网 token（32 位 hex）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token: Option<String>,
    /// 局域网 IP
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub localip: Option<String>,
    /// 写入时间
    #[serde(rename = "savedAt", default, skip_serializing_if = "Option::is_none")]
    pub saved_at: Option<String>,
}

impl DeviceInfo {
    /// 是否具备局域网直连条件（缺一不可）。
    pub fn local_ready(&self) -> bool {
        self.localip.as_deref().is_some_and(|s| !s.trim().is_empty())
            && self.token.as_deref().is_some_and(|s| !s.trim().is_empty())
    }

    /// 机型是否匹配目标机型（没写 model 时按「未知但可用」处理，与 v1 一致）。
    pub fn model_matches(&self, expect: &str) -> bool {
        match self.model.as_deref() {
            None => true,
            Some(m) if m.trim().is_empty() => true,
            Some(m) => m == expect,
        }
    }
}

/// `thermometer.json` —— 温湿度计的型号与 did。
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, Default)]
pub struct ThermometerInfo {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default)]
    pub did: String,
    #[serde(rename = "savedAt", default, skip_serializing_if = "Option::is_none")]
    pub saved_at: Option<String>,
}

/// 凭据存储：负责目录选择、回退查找与写入。
#[derive(Debug, Clone)]
pub struct Credentials {
    /// 主目录（新写入的凭据落这里）
    primary: PathBuf,
    /// 回退目录（旧位置，只读）
    fallbacks: Vec<PathBuf>,
}

impl Credentials {
    /// 开发期 / 便携版：主目录 = 可执行文件所在目录。
    pub fn portable() -> Self {
        let dir = std::env::current_exe()
            .ok()
            .and_then(|p| p.parent().map(Path::to_path_buf))
            .unwrap_or_else(|| PathBuf::from("."));
        Self { primary: dir, fallbacks: Vec::new() }
    }

    /// 新建：指定主目录，并把默认位置加为回退。
    pub fn new(primary: impl Into<PathBuf>) -> Self {
        let primary = primary.into();
        let mut fallbacks = Vec::new();

        // 回退 1：%APPDATA%\米家空调\（v1 安装版/便携版用的位置）
        if let Some(appdata) = user_data_dir() {
            if appdata != primary {
                fallbacks.push(appdata);
            }
        }
        // 回退 2：可执行文件所在目录（便携版把凭据放在程序旁边）
        if let Some(exe_dir) = std::env::current_exe()
            .ok()
            .and_then(|p| p.parent().map(Path::to_path_buf))
        {
            if exe_dir != primary && !fallbacks.contains(&exe_dir) {
                fallbacks.push(exe_dir);
            }
        }
        // 回退 3：当前工作目录（开发模式）
        if let Ok(cwd) = std::env::current_dir() {
            if cwd != primary && !fallbacks.contains(&cwd) {
                fallbacks.push(cwd);
            }
        }

        Self { primary, fallbacks }
    }

    /// v2 图形版应该用这个：主目录 = `%APPDATA%\米家空调\`，与 v1 完全一致，
    /// 这样旧版留下的凭据会被直接复用。
    pub fn appdata() -> Self {
        match user_data_dir() {
            Some(d) => Self::new(d),
            None => Self::portable(),
        }
    }

    /// 测试用：只认这一个目录，**不挂任何回退目录**。
    ///
    /// 为什么必须有这个构造器：`new()` 会把 `%APPDATA%\米家空调\`、可执行文件
    /// 目录、当前工作目录都加成回退。写测试时如果用它，测试就可能静默读到
    /// **本机真实的凭据**——测试结果随开发机状态变化，非常隐蔽（本项目第一版
    /// worker 测试就踩了这个坑：临时目录是空的，却从回退目录读到了真凭据，
    /// 于是「应该初始化失败」的断言反而不成立）。
    /// 需要确定性的场景一律用这个。
    pub fn isolated(only: impl Into<PathBuf>) -> Self {
        Self { primary: only.into(), fallbacks: Vec::new() }
    }

    pub fn primary_dir(&self) -> &Path {
        &self.primary
    }

    /// 依次尝试的目录：主目录优先，然后回退目录。
    pub fn search_dirs(&self) -> impl Iterator<Item = &Path> {
        std::iter::once(self.primary.as_path()).chain(self.fallbacks.iter().map(|p| p.as_path()))
    }

    /// 实际读到该文件的位置（界面提示用），找不到返回 None。
    pub fn locate(&self, file: &str) -> Option<PathBuf> {
        self.search_dirs()
            .map(|d| d.join(file))
            .find(|p| p.is_file())
    }

    pub fn exists(&self, file: &str) -> bool {
        self.locate(file).is_some()
    }

    /// 读取并解析为 JSON；找不到或坏掉都返回 None（调用方按「未配置」处理）。
    pub fn read_json<T: serde::de::DeserializeOwned>(&self, file: &str) -> Option<T> {
        // 每个候选文件都独立尝试明文与 DPAPI。主目录里留下半写入/损坏文件时，
        // 不能因此遮住仍然有效的旧版回退凭据。
        for dir in self.search_dirs() {
            let Ok(raw) = fs::read(dir.join(file)) else { continue };
            if let Ok(value) = serde_json::from_slice(&raw) {
                return Some(value);
            }
            #[cfg(windows)]
            if let Some(plain) = crate::dpapi::unprotect(&raw) {
                if let Ok(value) = serde_json::from_slice(&plain) {
                    return Some(value);
                }
            }
        }
        None
    }

    pub fn read_device(&self) -> Option<DeviceInfo> {
        self.read_json(FILE_DEVICE)
    }

    pub fn read_session(&self) -> Option<CloudSession> {
        self.read_json(FILE_SESSION)
    }

    pub fn read_thermometer(&self) -> Option<ThermometerInfo> {
        self.read_json(FILE_THERMOMETER)
    }

    /// 写明文 JSON（与 v1 格式一致，保证旧版仍可读）。
    pub fn write_json<T: serde::Serialize>(&self, file: &str, data: &T) -> std::io::Result<PathBuf> {
        let text = serde_json::to_string_pretty(data)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
        self.write_bytes(file, text.as_bytes())
    }

    /// 写 DPAPI 加密的 JSON（v2 专属格式）。
    ///
    /// 传 `encrypt = false` 时退化为写明文，便于新旧并存测试期切换。
    pub fn write_json_maybe_encrypted<T: serde::Serialize>(
        &self,
        file: &str,
        data: &T,
        encrypt: bool,
    ) -> std::io::Result<PathBuf> {
        let text = serde_json::to_string_pretty(data)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;

        if !encrypt {
            return self.write_bytes(file, text.as_bytes());
        }

        #[cfg(windows)]
        {
            let blob = crate::dpapi::protect(text.as_bytes()).map_err(|e| {
                std::io::Error::new(std::io::ErrorKind::Other, format!("DPAPI 加密失败：{e}"))
            })?;
            return self.write_bytes(file, &blob);
        }
        #[cfg(not(windows))]
        return Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "当前平台不支持 DPAPI 凭据加密",
        ));
    }

    /// 底层写入：建目录 → 写文件 → 收紧权限。
    fn write_bytes(&self, file: &str, bytes: &[u8]) -> std::io::Result<PathBuf> {
        if !ALL_FILES.contains(&file) {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                format!("未知的凭据文件 \"{file}\""),
            ));
        }

        fs::create_dir_all(&self.primary)?;
        let path = self.primary.join(file);
        let temp = self.primary.join(format!(
            ".{file}.{}.{}.tmp",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        ));
        let write_result = (|| -> std::io::Result<()> {
            let mut out = fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&temp)?;
            out.write_all(bytes)?;
            out.sync_all()?;

            // 先收紧临时文件；失败时旧文件不受影响，临时文件会被清理。
            #[cfg(windows)]
            harden_permissions(&temp)?;
            fs::rename(&temp, &path)?;
            Ok(())
        })();
        if let Err(e) = write_result {
            let _ = fs::remove_file(&temp);
            return Err(e);
        }

        Ok(path)
    }
}

/// `%APPDATA%\米家空调\` —— 与 v1 的 `app.getPath('userData')` 同名同路径。
pub fn user_data_dir() -> Option<PathBuf> {
    let appdata = std::env::var_os("APPDATA")?;
    Some(PathBuf::from(appdata).join("米家空调"))
}

/// 用 icacls 断掉权限继承，只保留当前用户可访问。
///
/// 与 v1 `credentials.js` 的做法一致：失败直接报错，绝不留下权限过宽的凭据文件。
#[cfg(windows)]
pub(crate) fn harden_permissions(path: &Path) -> std::io::Result<()> {
    use std::process::Command;

    let user = std::env::var("USERNAME").map_err(|_| {
        std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            "无法确定当前 Windows 用户，拒绝保存凭据",
        )
    })?;

    let out = Command::new("icacls")
        .arg(path)
        .arg("/inheritance:r")
        .arg("/grant:r")
        .arg(format!("{user}:(F)"))
        .output()?;

    if !out.status.success() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            format!(
                "无法收紧凭据文件权限：{}",
                String::from_utf8_lossy(&out.stderr).trim()
            ),
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn device_json_reads_v1_format() {
        // v1 写出来的 device.json 原样可读
        let json = r#"{
            "name": "巨省电 大2匹",
            "model": "xiaomi.airc.h53h00",
            "did": "123456789",
            "token": "00112233445566778899aabbccddeeff",
            "localip": "192.168.1.50",
            "savedAt": "2026-09-01T10:00:00.000Z"
        }"#;
        let d: DeviceInfo = serde_json::from_str(json).unwrap();
        assert_eq!(d.did, "123456789");
        assert!(d.local_ready());
        assert!(d.model_matches("xiaomi.airc.h53h00"));
        assert!(!d.model_matches("xiaomi.aircondition.mh4"));
    }

    #[test]
    fn device_without_localip_is_not_local_ready() {
        let json = r#"{ "did": "1", "token": "aa" }"#;
        let d: DeviceInfo = serde_json::from_str(json).unwrap();
        assert!(!d.local_ready(), "缺 localip 时不能走局域网");
    }

    #[test]
    fn device_roundtrip_keeps_v1_field_names() {
        let d = DeviceInfo {
            name: Some("空调".into()),
            model: Some("xiaomi.airc.h53h00".into()),
            did: "42".into(),
            token: Some("ff".into()),
            localip: Some("10.0.0.2".into()),
            saved_at: Some("2026-09-19T00:00:00.000Z".into()),
        };
        let s = serde_json::to_string(&d).unwrap();
        // v1 用的是 savedAt（驼峰），必须保持一致
        assert!(s.contains("\"savedAt\""));
        assert!(s.contains("\"localip\""));
    }

    #[test]
    fn user_data_dir_matches_v1() {
        // 只验证拼出来的最后一段目录名与 v1 一致
        if let Some(d) = user_data_dir() {
            assert_eq!(d.file_name().unwrap(), "米家空调");
        }
    }

    #[test]
    fn rejects_unknown_file_names() {
        let c = Credentials::portable();
        let err = c.write_bytes("evil.json", b"{}").unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::InvalidInput);
    }
}
