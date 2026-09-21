//! A session-local instance guard and a latched, non-blocking activation signal.
#![cfg(windows)]

use windows_sys::Win32::Foundation::{CloseHandle, GetLastError, ERROR_ALREADY_EXISTS, HANDLE};
use windows_sys::Win32::System::Threading::{CreateEventW, CreateMutexW, SetEvent, WaitForSingleObject};

struct Handle(HANDLE);
impl Drop for Handle {
    fn drop(&mut self) {
        unsafe { CloseHandle(self.0); }
    }
}

pub struct SingleInstance {
    _mutex: Handle,
    activate: Handle,
}

impl SingleInstance {
    pub fn acquire() -> std::io::Result<Option<Self>> {
        Self::acquire_named("Local\\Miac.Desktop.v2")
    }

    fn acquire_named(name: &str) -> std::io::Result<Option<Self>> {
        let wide = |suffix: &str| format!("{name}.{suffix}").encode_utf16().chain([0]).collect::<Vec<_>>();
        unsafe {
            // Create the event first: even a duplicate launched during startup
            // can leave a signal for the first instance's future UI timer.
            let activate = CreateEventW(std::ptr::null(), 0, 0, wide("activate").as_ptr());
            if activate.is_null() { return Err(std::io::Error::last_os_error()); }
            let activate = Handle(activate);
            let mutex = CreateMutexW(std::ptr::null(), 0, wide("instance").as_ptr());
            if mutex.is_null() { return Err(std::io::Error::last_os_error()); }
            let exists = GetLastError() == ERROR_ALREADY_EXISTS;
            let instance = Self { _mutex: Handle(mutex), activate };
            if exists {
                if SetEvent(instance.activate.0) == 0 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(None)
            } else {
                Ok(Some(instance))
            }
        }
    }

    pub fn take_activation(&self) -> bool {
        // WAIT_OBJECT_0 = 0. Auto-reset consumes the signal exactly once.
        unsafe { WaitForSingleObject(self.activate.0, 0) == 0 }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn duplicates_signal_first_instance_and_guard_releases_on_exit() {
        let name = format!("Local\\Miac.Test.{}", std::process::id());
        let first = SingleInstance::acquire_named(&name).unwrap().unwrap();
        assert!(!first.take_activation());
        // The signal survives even though the original UI is not listening yet.
        for _ in 0..10 {
            assert!(SingleInstance::acquire_named(&name).unwrap().is_none());
        }
        assert!(first.take_activation());
        assert!(!first.take_activation());
        drop(first);
        assert!(SingleInstance::acquire_named(&name).unwrap().is_some());
    }
}
