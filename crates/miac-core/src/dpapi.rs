//! dpapi.rs —— 用 Windows DPAPI 加密凭据
//!
//! 默认使用当前 Windows 用户的 DPAPI 上下文，不启用 LOCAL_MACHINE 范围。
//! 同一用户的其他进程仍可调用 DPAPI；域账户漫游、备份密钥等场景也可能
//! 允许异机恢复，不能宣称密文“无人可解”。
//!
//! 官方文档：<https://learn.microsoft.com/windows/win32/api/dpapi/nf-dpapi-cryptprotectdata>
//!
//! ## 兼容性
//!
//! 新格式带版本头，并把文件名纳入额外熵，避免三个凭据文件的密文互换。
//! 旧版没有版本头的 DPAPI 密文仍可读，写入时一律升级到新格式。

const LEGACY_ENTROPY: &[u8] = b"miac-app-v2-credentials";
const FORMAT_MAGIC: &[u8] = b"MIAC-DPAPI-3\0";

/// 额外熵只是公开的用途隔离值，不是隐藏在 EXE 里的密码。
fn entropy_for(file: &str) -> Vec<u8> {
    let mut entropy = b"miac-app-v3-credentials\0".to_vec();
    entropy.extend_from_slice(file.as_bytes());
    entropy
}

/// DPAPI 操作失败。
#[derive(Debug)]
pub struct DpapiError(pub String);

impl std::fmt::Display for DpapiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "DPAPI 失败：{}", self.0)
    }
}

impl std::error::Error for DpapiError {}

#[cfg(windows)]
mod imp {
    use super::DpapiError;
    use std::ptr;
    use windows_sys::Win32::Foundation::LocalFree;
    use windows_sys::Win32::Security::Cryptography::{
        CryptProtectData, CryptUnprotectData, CRYPT_INTEGER_BLOB, CRYPTPROTECT_UI_FORBIDDEN,
    };

    /// 把 `data` 交给 `f` 处理，自动管理 DPAPI 分配的输出缓冲。
    ///
    /// `CryptProtectData` / `CryptUnprotectData` 用 `LocalAlloc` 分配输出，
    /// 必须用 `LocalFree` 释放，否则每调用一次就漏一块内存。
    unsafe fn with_blob<F>(data: &[u8], entropy_bytes: &[u8], f: F) -> Result<Vec<u8>, DpapiError>
    where
        F: FnOnce(*const CRYPT_INTEGER_BLOB, *const CRYPT_INTEGER_BLOB, *mut CRYPT_INTEGER_BLOB) -> i32,
    {
        let mut input = CRYPT_INTEGER_BLOB {
            cbData: data.len() as u32,
            pbData: data.as_ptr() as *mut u8,
        };
        let mut entropy = CRYPT_INTEGER_BLOB {
            cbData: entropy_bytes.len() as u32,
            pbData: entropy_bytes.as_ptr() as *mut u8,
        };
        let mut output = CRYPT_INTEGER_BLOB { cbData: 0, pbData: ptr::null_mut() };

        let ok = f(&input, &entropy, &mut output);
        if ok == 0 {
            let e = std::io::Error::last_os_error();
            return Err(DpapiError(format!("{e}")));
        }

        // 拷贝出来后清除 DPAPI 分配的缓冲，再释放。解密时其中含有明文。
        let out = if output.cbData == 0 {
            Vec::new()
        } else {
            std::slice::from_raw_parts(output.pbData, output.cbData as usize).to_vec()
        };
        for i in 0..output.cbData as usize {
            std::ptr::write_volatile(output.pbData.add(i), 0);
        }
        LocalFree(output.pbData as *mut core::ffi::c_void);
        let _ = &mut input; // 保持 input 存活到调用结束
        let _ = &mut entropy;
        Ok(out)
    }

    pub fn protect(data: &[u8], entropy: &[u8]) -> Result<Vec<u8>, DpapiError> {
        unsafe {
            with_blob(data, entropy, |input, entropy, output| {
                CryptProtectData(
                    input,
                    ptr::null(),   // 描述文本（不写进密文，避免泄露内容）
                    entropy,
                    ptr::null_mut(), // 保留
                    ptr::null_mut(), // 保留
                    CRYPTPROTECT_UI_FORBIDDEN, // 当前用户范围，禁止意外弹出系统 UI
                    output,
                )
            })
        }
    }

    pub fn unprotect(data: &[u8], entropy: &[u8]) -> Result<Vec<u8>, DpapiError> {
        unsafe {
            with_blob(data, entropy, |input, entropy, output| {
                CryptUnprotectData(
                    input,
                    ptr::null_mut(), // 不需要返回描述
                    entropy,
                    ptr::null_mut(),
                    ptr::null_mut(),
                    CRYPTPROTECT_UI_FORBIDDEN,
                    output,
                )
            })
        }
    }
}

/// 加密某个凭据文件，新写入的密文会标记格式版本。
#[cfg(windows)]
pub fn protect_for(file: &str, data: &[u8]) -> Result<Vec<u8>, DpapiError> {
    let blob = imp::protect(data, &entropy_for(file))?;
    let mut encoded = Vec::with_capacity(FORMAT_MAGIC.len() + blob.len());
    encoded.extend_from_slice(FORMAT_MAGIC);
    encoded.extend_from_slice(&blob);
    Ok(encoded)
}

/// 解密新格式或旧版没有版本头的 DPAPI 密文。
#[cfg(windows)]
pub fn unprotect_for(file: &str, data: &[u8]) -> Option<Vec<u8>> {
    if let Some(blob) = data.strip_prefix(FORMAT_MAGIC) {
        imp::unprotect(blob, &entropy_for(file)).ok()
    } else {
        imp::unprotect(data, LEGACY_ENTROPY).ok()
    }
}

/// 非 Windows 平台不得把敏感凭据静默降级为明文写入。
#[cfg(not(windows))]
pub fn protect_for(_file: &str, _data: &[u8]) -> Result<Vec<u8>, DpapiError> {
    Err(DpapiError("本平台不支持 DPAPI".into()))
}

#[cfg(not(windows))]
pub fn unprotect_for(_file: &str, _data: &[u8]) -> Option<Vec<u8>> {
    None
}

pub fn is_current_format(data: &[u8]) -> bool {
    data.starts_with(FORMAT_MAGIC)
}

/// 判断一坨字节看起来像不像 DPAPI 密文。
///
/// 明文 JSON 一定以 `{` 或 `[` 开头（可能带 BOM），所以「不是 JSON 开头」
/// 且长度足够，就当作密文尝试解密。
pub fn looks_encrypted(data: &[u8]) -> bool {
    match data.first() {
        None => false,
        Some(b'{') | Some(b'[') => false,
        // UTF-8 BOM
        Some(0xEF) if data.len() > 3 && &data[1..3] == b"\xBB\xBF" => false,
        _ => data.len() > 32,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn looks_encrypted_classifies_correctly() {
        assert!(!looks_encrypted(b"{}"));
        assert!(!looks_encrypted(b"{\"did\":\"1\"}"));
        assert!(!looks_encrypted(b"[1,2,3]"));
        assert!(!looks_encrypted(b""));
        assert!(!looks_encrypted(b"\xEF\xBB\xBF{}"));
        // 一段够长的二进制 → 判为密文
        assert!(looks_encrypted(&[0x01u8; 64]));
    }

    #[cfg(windows)]
    #[test]
    fn dpapi_roundtrip() {
        // 同一用户同一机器上，加密后必须能原样解回来
        let plain = br#"{"did":"123","token":"aabb"}"#;
        let blob = protect_for("device.json", plain).expect("DPAPI 加密应成功");
        assert_ne!(&blob[..], &plain[..], "密文不应等于明文");
        assert!(is_current_format(&blob));
        let back = unprotect_for("device.json", &blob).expect("DPAPI 解密应成功");
        assert_eq!(&back[..], &plain[..]);
        assert!(unprotect_for("cloud-session.json", &blob).is_none());
        let legacy = imp::protect(plain, LEGACY_ENTROPY).unwrap();
        assert_eq!(unprotect_for("device.json", &legacy).unwrap(), plain);
    }

    #[cfg(windows)]
    #[test]
    fn dpapi_rejects_garbage() {
        // 随便一段数据不应该被「解」成有效内容
        assert!(unprotect_for("device.json", &[0u8; 64]).is_none());
    }
}
