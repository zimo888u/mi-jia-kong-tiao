//! settings.rs —— 应用设置（明暗主题、预设温度、通道策略等）
//!
//! 与凭据分开存放：凭据是敏感的（DPAPI 加密），设置是普通偏好。
//! 文件名 `settings.json`，同样放在 `%APPDATA%\米家空调\`。
//!
//! 与 v1 的关系：v1 把主题存在渲染进程的 localStorage 里（在
//! `%APPDATA%\米家空调\Local Storage\` 的 LevelDB 中），不是 JSON，
//! 所以 v2 无法用解析 JSON 的方式读它。首启迁移时由 `migration.rs`
//! 尽力从 LevelDB 里抠出主题键，抠不到就用默认值（浅色）。

use std::path::PathBuf;

use crate::credentials::{user_data_dir, Credentials};

/// 设置文件名。
pub const FILE_SETTINGS: &str = "settings.json";

/// 温度预设的档位数（与 v1 界面上的三个按钮一致）。
pub const PRESET_SLOTS: usize = 3;

/// 应用设置。
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Settings {
    /// 明暗主题：true = 深色
    #[serde(default)]
    pub dark: bool,
    /// 三个温度预设（℃）
    #[serde(default = "default_presets")]
    pub presets: Vec<f64>,
    /// 空调关机时由用户预先选择的温度。开机成功后才下发给设备。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pending_temp: Option<f64>,
    /// The device that owns the pending temperature, when saved by this version.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pending_temp_device: Option<String>,
    /// 通信通道：0 自动 / 1 强制局域网 / 2 强制云端
    #[serde(default)]
    pub transport: i32,
    /// 自动刷新开关
    #[serde(default = "default_true")]
    pub auto_refresh: bool,
    /// 自动刷新间隔（秒）。v1 是 6 秒。
    #[serde(default = "default_interval")]
    pub refresh_secs: u64,
    /// 关闭窗口时收进托盘（而不是退出）
    #[serde(default = "default_true")]
    pub close_to_tray: bool,
    /// 用户明确记住关闭方式后才跳过询问；旧配置也默认先询问。
    #[serde(default)]
    pub remember_close_action: bool,
    /// 是否已经完成过首次迁移（避免每次启动都跑一遍）
    #[serde(default)]
    pub migrated: bool,
    /// 旧版本的显示标记；真实状态必须检查凭据文件，不能信任此字段。
    #[serde(default)]
    pub encrypt_credentials: bool,
}

fn default_true() -> bool {
    true
}

fn default_interval() -> u64 {
    6
}

fn default_presets() -> Vec<f64> {
    vec![27.5, 27.0, 26.5]
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            dark: false,
            presets: default_presets(),
            pending_temp: None,
            pending_temp_device: None,
            transport: 0,
            auto_refresh: true,
            refresh_secs: 6,
            close_to_tray: true,
            remember_close_action: false,
            migrated: false,
            encrypt_credentials: false,
        }
    }
}

impl Settings {
    /// 0 每次询问，1 收到托盘，2 直接退出。
    pub fn close_behavior(&self) -> i32 {
        if !self.remember_close_action { 0 } else if self.close_to_tray { 1 } else { 2 }
    }

    pub fn set_close_behavior(&mut self, behavior: i32) {
        match behavior {
            0 => self.remember_close_action = false,
            1 | 2 => {
                self.remember_close_action = true;
                self.close_to_tray = behavior == 1;
            }
            _ => {}
        }
    }

    /// 读设置；文件不存在或坏掉时返回默认值（不报错，界面照常起）。
    pub fn load(creds: &Credentials) -> Self {
        let mut s: Settings = creds.read_json(FILE_SETTINGS).unwrap_or_default();
        s.normalize();
        s
    }

    /// 写设置。失败只告警，不影响使用。
    pub fn save(&self, creds: &Credentials) -> std::io::Result<PathBuf> {
        creds.write_json(FILE_SETTINGS, self)
    }

    /// 把不合法的值拉回合理范围，避免坏文件把界面搞崩。
    pub fn normalize(&mut self) {
        if self.presets.len() != PRESET_SLOTS {
            self.presets = default_presets();
        }
        for p in &mut self.presets {
            if !p.is_finite() || !(5.0..=40.0).contains(p) {
                *p = 26.5;
            }
        }
        if let Some(pending) = self.pending_temp {
            if !pending.is_finite() || !(5.0..=40.0).contains(&pending) {
                self.pending_temp = None;
                self.pending_temp_device = None;
            } else {
                self.pending_temp = Some(pending); // 型号范围和步长由 Profile 在下发前校验
            }
        }
        if !(0..=2).contains(&self.transport) {
            self.transport = 0;
        }
        // 刷新间隔给个下限，防止有人写 0 把设备刷爆
        self.refresh_secs = self.refresh_secs.clamp(2, 300);
    }

    /// 取第 idx 个预设（越界返回 None）。
    pub fn preset(&self, idx: usize) -> Option<f64> {
        self.presets.get(idx).copied()
    }

    /// 通道索引 → `Transport` 枚举。
    pub fn transport(&self) -> crate::Transport {
        crate::Transport::from_index(self.transport)
    }

    /// 改第 idx 个预设并返回是否真的改了。
    pub fn set_preset(&mut self, idx: usize, value: f64) -> bool {
        if idx >= self.presets.len() || !value.is_finite() || !(5.0..=40.0).contains(&value) {
            return false;
        }
        self.presets[idx] = value; // 保留型号支持的精度
        true
    }
}

/// 设置文件的实际路径（界面「关于」里展示用）。
pub fn settings_path() -> Option<PathBuf> {
    user_data_dir().map(|d| d.join(FILE_SETTINGS))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn close_behavior_asks_until_user_explicitly_remembers_a_choice() {
        assert_eq!(Settings::default().close_behavior(), 0);
        for legacy in [r#"{"close_to_tray":true}"#, r#"{"close_to_tray":false}"#] {
            let settings: Settings = serde_json::from_str(legacy).unwrap();
            assert_eq!(settings.close_behavior(), 0);
        }
    }

    #[test]
    fn remembered_close_choices_survive_restart_and_can_be_reset() {
        let mut settings = Settings::default();
        for choice in [1, 2, 0] {
            settings.set_close_behavior(choice);
            let saved = serde_json::to_string(&settings).unwrap();
            settings = serde_json::from_str(&saved).unwrap();
            assert_eq!(settings.close_behavior(), choice);
        }
        settings.set_close_behavior(99);
        assert_eq!(settings.close_behavior(), 0);
    }

    #[test]
    fn defaults_are_sane() {
        let s = Settings::default();
        assert!(!s.dark, "new installations should use the sage light theme");
        assert_eq!(s.presets.len(), PRESET_SLOTS);
        assert_eq!(s.presets, vec![27.5, 27.0, 26.5]);
        assert_eq!(s.transport, 0);
        assert!(s.auto_refresh);
        assert_eq!(s.refresh_secs, 6);
        assert!(!s.migrated);
    }

    #[test]
    fn missing_theme_uses_light_default() {
        let settings: Settings = serde_json::from_str("{}").unwrap();
        assert!(!settings.dark);
    }

    #[test]
    fn saved_theme_preference_is_preserved() {
        for dark in [false, true] {
            let settings: Settings = serde_json::from_str(
                &format!(r#"{{"dark":{dark}}}"#),
            ).unwrap();
            assert_eq!(settings.dark, dark);
        }
    }

    #[test]
    fn normalize_repairs_bad_values() {
        let mut s = Settings {
            presets: vec![100.0, -5.0], // 长度不对 + 越界
            transport: 99,
            refresh_secs: 0,
            ..Default::default()
        };
        s.normalize();
        assert_eq!(s.presets, vec![27.5, 27.0, 26.5], "长度不对时整体回默认");
        assert_eq!(s.transport, 0);
        assert_eq!(s.refresh_secs, 2, "刷新间隔有下限 2 秒");
    }

    #[test]
    fn normalize_keeps_valid_presets_but_fixes_out_of_range() {
        let mut s = Settings { presets: vec![26.0, 999.0, 27.5], ..Default::default() };
        s.normalize();
        assert_eq!(s.presets[0], 26.0);
        assert_eq!(s.presets[1], 26.5, "越界的单项回默认");
        assert_eq!(s.presets[2], 27.5);
    }

    #[test]
    fn set_preset_preserves_model_specific_precision() {
        let mut s = Settings::default();
        assert!(s.set_preset(0, 26.3));
        assert_eq!(s.presets[0], 26.3);
        s.pending_temp = Some(32.0);
        s.normalize();
        assert_eq!(s.pending_temp, Some(32.0));
        assert!(!s.set_preset(9, 26.0), "越界索引应失败");
        assert!(!s.set_preset(0, 99.0), "越界温度应失败");
    }

    #[test]
    fn json_roundtrip() {
        let s = Settings::default();
        let text = serde_json::to_string(&s).unwrap();
        let back: Settings = serde_json::from_str(&text).unwrap();
        assert_eq!(back.presets, s.presets);
        assert_eq!(back.dark, s.dark);
    }

    #[test]
    fn missing_fields_fall_back_to_defaults() {
        // 手写的残缺 settings.json 不应导致解析失败
        let s: Settings = serde_json::from_str(r#"{ "dark": false }"#).unwrap();
        assert!(!s.dark);
        assert_eq!(s.presets.len(), PRESET_SLOTS);
        assert_eq!(s.refresh_secs, 6);
    }
}
