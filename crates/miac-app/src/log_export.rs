//! 将界面里保留的本次运行日志导出为 UTF-8 文本，不读取凭据或磁盘日志。

use std::{ffi::OsString, io, os::windows::ffi::OsStringExt, path::{Path, PathBuf}};
use windows_sys::Win32::{
    Foundation::SYSTEMTIME,
    System::SystemInformation::GetLocalTime,
    UI::{
        Controls::Dialogs::{
            CommDlgExtendedError, GetSaveFileNameW, OPENFILENAMEW, OFN_EXPLORER,
            OFN_HIDEREADONLY, OFN_NOCHANGEDIR, OFN_NOREADONLYRETURN, OFN_OVERWRITEPROMPT,
            OFN_PATHMUSTEXIST,
        },
        Input::KeyboardAndMouse::GetActiveWindow,
    },
};

fn local_time() -> SYSTEMTIME {
    let mut time = unsafe { std::mem::zeroed() };
    unsafe { GetLocalTime(&mut time) };
    time
}

pub fn choose_path() -> Result<Option<PathBuf>, String> {
    let time = local_time();
    let suggested = format!(
        "miac-log-{:04}{:02}{:02}-{:02}{:02}{:02}.txt",
        time.wYear, time.wMonth, time.wDay, time.wHour, time.wMinute, time.wSecond
    );
    // Common Dialog 要求可写且以 NUL 结尾的 UTF-16 缓冲区。
    let mut file = vec![0u16; 32_768];
    let filename: Vec<u16> = suggested.encode_utf16().collect();
    file[..filename.len()].copy_from_slice(&filename);
    let filter: Vec<u16> = "文本文件 (*.txt)\0*.txt\0所有文件 (*.*)\0*.*\0\0"
        .encode_utf16()
        .collect();
    let title: Vec<u16> = "导出运行日志\0".encode_utf16().collect();
    let extension: Vec<u16> = "txt\0".encode_utf16().collect();

    let mut dialog: OPENFILENAMEW = unsafe { std::mem::zeroed() };
    dialog.lStructSize = std::mem::size_of::<OPENFILENAMEW>() as u32;
    dialog.hwndOwner = unsafe { GetActiveWindow() };
    dialog.lpstrFilter = filter.as_ptr();
    dialog.nFilterIndex = 1;
    dialog.lpstrFile = file.as_mut_ptr();
    dialog.nMaxFile = file.len() as u32;
    dialog.lpstrTitle = title.as_ptr();
    dialog.lpstrDefExt = extension.as_ptr();
    dialog.Flags = OFN_EXPLORER
        | OFN_HIDEREADONLY
        | OFN_NOCHANGEDIR
        | OFN_NOREADONLYRETURN
        | OFN_OVERWRITEPROMPT
        | OFN_PATHMUSTEXIST;

    if unsafe { GetSaveFileNameW(&mut dialog) } == 0 {
        let code = unsafe { CommDlgExtendedError() };
        return if code == 0 {
            Ok(None)
        } else {
            Err(format!("Windows 对话框错误 0x{code:04X}"))
        };
    }

    let end = file.iter().position(|&unit| unit == 0).ok_or("文件路径过长")?;
    Ok(Some(PathBuf::from(OsString::from_wide(&file[..end]))))
}

fn format_logs(lines: &[String], exported_at: &str) -> String {
    let mut out = format!(
        "米家空调运行日志\r\n导出时间：{exported_at}\r\n范围：本次运行，最近 200 条\r\n提示：日志可能包含设备名称和本机路径，分享前请检查。\r\n\r\n"
    );
    for line in lines {
        out.push_str(line);
        out.push_str("\r\n");
    }
    out
}

pub fn save(path: &Path, lines: &[String]) -> io::Result<()> {
    if !path
        .extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| ext.eq_ignore_ascii_case("txt"))
    {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "请保存为 .txt 文件"));
    }
    let time = local_time();
    let stamp = format!(
        "{:04}-{:02}-{:02} {:02}:{:02}:{:02}",
        time.wYear, time.wMonth, time.wDay, time.wHour, time.wMinute, time.wSecond
    );
    std::fs::write(path, format_logs(lines, &stamp))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn export_is_utf8_and_keeps_the_current_log_order() {
        let path = std::env::temp_dir().join(format!(
            "miac-export-test-{}-{}.txt",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let lines = vec!["[  1s] 已连接".to_owned(), "[  2s] 状态更新".to_owned()];
        save(&path, &lines).unwrap();
        let content = std::fs::read_to_string(&path).unwrap();
        std::fs::remove_file(&path).unwrap();
        assert!(content.contains("米家空调运行日志\r\n"));
        assert!(content.ends_with("[  1s] 已连接\r\n[  2s] 状态更新\r\n"));
    }

    #[test]
    fn refuses_to_overwrite_a_non_txt_file() {
        let path = std::env::temp_dir().join("device.json");
        let err = save(&path, &["example".to_owned()]).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
    }
}
