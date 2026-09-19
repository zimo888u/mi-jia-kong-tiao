//! dpapi.rs —— 用 Windows DPAPI 加密凭据
//!
//! DPAPI（Data Protection API）把数据加密成只能由**特定上下文**解开的形式：
//!
//! - `CryptProtectData` 默认（不传 `CRYPTPROTECT_LOCAL_MACHINE`）时，
//!   密钥由**当前用户的登录凭据**派生。换个 Windows 账户就解不开。
//! - 密文里还混入了**机器密钥**，所以把文件拷到另一台电脑也解不开。
//!
//! 这正是需求里说的「将数据绑定到当前 Windows 用户和电脑」。相比 v1 只用
//! 文件权限（icacls）保护明文，DPAPI 多了一层真正的加密。
//!
//! 官方文档：<https://learn.microsoft.com/windows/win32/api/dpapi/nf-dpapi-cryptprotectdata>
//!
//! ## 兼容性
//!
//! 本模块只负责「加密/解密字节」。格式判断（明文 JSON 还是 DPAPI 密文）
//! 由 `credentials.rs` 负责——先按明文试解析，失败再当密文解。这样 v1 留下
//! 的明文凭据和 v2 写的密文凭据可以同时被正确读取。

/// DPAPI 密文的额外熵。
///
/// 加上固定的额外熵后，即使别的程序也调用 DPAPI，也解不开我们的密文
/// （它必须知道这串熵）。作用类似「命名空间」。
const ENTROPY: &[u8] = b"miac-app-v2-credentials";

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
    use super::{DpapiError, ENTROPY};
    use std::ptr;
    use windows_sys::Win32::Foundation::LocalFree;
    use windows_sys::Win32::Security::Cryptography::{
        CryptProtectData, CryptUnprotectData, CRYPT_INTEGER_BLOB,
    };

    /// 把 `data` 交给 `f` 处理，自动管理 DPAPI 分配的输出缓冲。
    ///
    /// `CryptProtectData` / `CryptUnprotectData` 用 `LocalAlloc` 分配输出，
    /// 必须用 `LocalFree` 释放，否则每调用一次就漏一块内存。
    unsafe fn with_blob<F>(data: &[u8], f: F) -> Result<Vec<u8>, DpapiError>
    where
        F: FnOnce(*const CRYPT_INTEGER_BLOB, *const CRYPT_INTEGER_BLOB, *mut CRYPT_INTEGER_BLOB) -> i32,
    {
        let mut input = CRYPT_INTEGER_BLOB {
            cbData: data.len() as u32,
            pbData: data.as_ptr() as *mut u8,
        };
        let mut entropy = CRYPT_INTEGER_BLOB {
            cbData: ENTROPY.len() as u32,
            pbData: ENTROPY.as_ptr() as *mut u8,
        };
        let mut output = CRYPT_INTEGER_BLOB { cbData: 0, pbData: ptr::null_mut() };

        let ok = f(&input, &entropy, &mut output);
        if ok == 0 {
            let e = std::io::Error::last_os_error();
            return Err(DpapiError(format!("{e}")));
        }

        // 拷贝出来再释放
        let out = std::slice::from_raw_parts(output.pbData, output.cbData as usize).to_vec();
        LocalFree(output.pbData as *mut core::ffi::c_void);
        let _ = &mut input; // 保持 input 存活到调用结束
        let _ = &mut entropy;
        Ok(out)
    }

    /// 加密：绑定当前 Windows 用户 + 本机。
    pub fn protect(data: &[u8]) -> Result<Vec<u8>, DpapiError> {
        unsafe {
            with_blob(data, |input, entropy, output| {
                CryptProtectData(
                    input,
                    ptr::null(),   // 描述文本（不写进密文，避免泄露内容）
                    entropy,
                    ptr::null_mut(), // 保留
                    ptr::null_mut(), // 保留
                    0,               // 0 = 绑定当前用户（不是 LOCAL_MACHINE）
                    output,
                )
            })
        }
    }

    /// 解密：只有同一用户 + 同一机器能成功。
    pub fn unprotect(data: &[u8]) -> Result<Vec<u8>, DpapiError> {
        unsafe {
            with_blob(data, |input, entropy, output| {
                CryptUnprotectData(
                    input,
                    ptr::null_mut(), // 不需要返回描述
                    entropy,
                    ptr::null_mut(),
                    ptr::null_mut(),
                    0,
                    output,
                )
            })
        }
    }
}

/// 加密字节。非 Windows 平台直接报错（本项目只针对 Windows）。
#[cfg(windows)]
pub fn protect(data: &[u8]) -> Result<Vec<u8>, DpapiError> {
    imp::protect(data)
}

/// 解密字节。
#[cfg(windows)]
pub fn unprotect(data: &[u8]) -> Option<Vec<u8>> {
    imp::unprotect(data).ok()
}

/// 平台不可用时的实现：让调用方走明文回退路径。
#[cfg(not(windows))]
pub fn protect(_data: &[u8]) -> Result<Vec<u8>, DpapiError> {
    Err(DpapiError("本平台不支持 DPAPI".into()))
}

#[cfg(not(windows))]
pub fn unprotect(_data: &[u8]) -> Option<Vec<u8>> {
    None
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
        let blob = protect(plain).expect("DPAPI 加密应成功");
        assert_ne!(&blob[..], &plain[..], "密文不应等于明文");
        let back = unprotect(&blob).expect("DPAPI 解密应成功");
        assert_eq!(&back[..], &plain[..]);
    }

    #[cfg(windows)]
    #[test]
    fn dpapi_rejects_garbage() {
        // 随便一段数据不应该被「解」成有效内容
        assert!(unprotect(&[0u8; 64]).is_none());
    }
}
