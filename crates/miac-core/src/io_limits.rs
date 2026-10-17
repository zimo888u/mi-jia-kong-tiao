//! Bounded reads for untrusted local files and decompressed HTTP bodies.
use std::{fs::File, io::{self, Read}, path::Path};

pub const CREDENTIAL_BYTES: usize = 1024 * 1024;
pub const HTTP_BYTES: usize = 4 * 1024 * 1024;
pub const SPEC_BYTES: usize = 32 * 1024 * 1024;

pub fn read(mut reader: impl Read, limit: usize) -> io::Result<Vec<u8>> {
    let mut bytes = Vec::new();
    reader.by_ref().take(limit as u64 + 1).read_to_end(&mut bytes)?;
    if bytes.len() > limit {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "数据超过允许的大小"));
    }
    Ok(bytes)
}

pub fn read_file(path: impl AsRef<Path>, limit: usize) -> io::Result<Vec<u8>> {
    read(File::open(path)?, limit)
}

#[cfg(windows)]
pub fn system_executable(name: &str) -> io::Result<std::path::PathBuf> {
    use windows_sys::Win32::System::SystemInformation::GetSystemDirectoryW;
    if name.is_empty() || !name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'.') {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "无效的系统程序名"));
    }
    let mut buffer = vec![0u16; 32768];
    let n = unsafe { GetSystemDirectoryW(buffer.as_mut_ptr(), buffer.len() as u32) } as usize;
    if n == 0 || n >= buffer.len() { return Err(io::Error::last_os_error()); }
    use std::os::windows::ffi::OsStringExt;
    Ok(std::path::PathBuf::from(std::ffi::OsString::from_wide(&buffer[..n])).join(name))
}

#[cfg(test)]
mod tests {
    #[test]
    fn reads_to_eof_and_rejects_oversized_streams() {
        assert_eq!(super::read(&b"1234"[..], 4).unwrap(), b"1234");
        assert!(super::read(&b"12345"[..], 4).is_err());
    }
}
