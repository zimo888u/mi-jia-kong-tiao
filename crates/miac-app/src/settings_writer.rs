//! Serialize settings writes independently of both rendering and device I/O.
use std::cell::RefCell;
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread::{self, JoinHandle};

use miac_core::{credentials::Credentials, settings::Settings};

enum Request {
    Save(Settings),
    Shutdown,
}

pub struct SettingsWriter {
    tx: Sender<Request>,
    errors: Receiver<String>,
    handle: RefCell<Option<JoinHandle<()>>>,
}

impl SettingsWriter {
    pub fn spawn(creds: Credentials) -> Self {
        Self::with_save(move |settings| {
            settings.save(&creds).map(|_| ()).map_err(|e| e.to_string())
        })
    }

    fn with_save(mut save: impl FnMut(Settings) -> Result<(), String> + Send + 'static) -> Self {
        let (tx, rx) = mpsc::channel();
        let (errors_tx, errors) = mpsc::channel();
        let handle = thread::Builder::new()
            .name("miac-settings".into())
            .stack_size(256 * 1024)
            .spawn(move || {
                while let Ok(request) = rx.recv() {
                    let Request::Save(mut latest) = request else {
                        break;
                    };
                    let mut shutdown = false;
                    // A slow disk/ACL operation may outlive many clicks. Persist only
                    // the newest complete settings snapshot, preserving write order.
                    while let Ok(request) = rx.try_recv() {
                        match request {
                            Request::Save(settings) => latest = settings,
                            Request::Shutdown => {
                                shutdown = true;
                                break;
                            }
                        }
                    }
                    if let Err(error) = save(latest) {
                        let _ = errors_tx.send(error);
                    }
                    if shutdown {
                        break;
                    }
                }
            })
            .expect("无法启动设置保存线程");
        Self {
            tx,
            errors,
            handle: RefCell::new(Some(handle)),
        }
    }

    pub fn save(&self, settings: &Settings) -> Result<(), String> {
        self.tx
            .send(Request::Save(settings.clone()))
            .map_err(|_| "设置保存线程已退出".into())
    }

    pub fn try_error(&self) -> Option<String> {
        self.errors.try_recv().ok()
    }

    pub fn shutdown(&self) {
        if let Some(handle) = self.handle.borrow_mut().take() {
            let _ = self.tx.send(Request::Shutdown);
            let _ = handle.join();
        }
    }
}

impl Drop for SettingsWriter {
    fn drop(&mut self) {
        self.shutdown();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn slow_save_does_not_block_input_and_shutdown_flushes_latest_settings() {
        let (started_tx, started_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let (saved_tx, saved_rx) = mpsc::channel();
        let mut first = true;
        let writer = SettingsWriter::with_save(move |settings| {
            if first {
                first = false;
                started_tx.send(()).unwrap();
                release_rx.recv_timeout(Duration::from_secs(5)).unwrap();
            }
            saved_tx.send(settings).unwrap();
            Ok(())
        });
        let mut settings = Settings::default();
        writer.save(&settings).unwrap();
        started_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        for i in 0..100 {
            settings.auto_refresh = i % 2 == 0;
            settings.pending_temp = Some(16.0 + (i % 31) as f64 * 0.5);
            writer.save(&settings).unwrap();
        }
        // Reaching this send proves input did not wait for the blocked disk.
        release_tx.send(()).unwrap();
        writer.shutdown();
        let saved: Vec<_> = saved_rx.try_iter().collect();
        assert_eq!(saved.len(), 2);
        assert_eq!(saved[1].auto_refresh, settings.auto_refresh);
        assert_eq!(saved[1].pending_temp, settings.pending_temp);
    }

    #[test]
    fn save_errors_are_reported() {
        let writer = SettingsWriter::with_save(|_| Err("disk full".into()));
        writer.save(&Settings::default()).unwrap();
        writer.shutdown();
        assert_eq!(writer.try_error().as_deref(), Some("disk full"));
    }
}
