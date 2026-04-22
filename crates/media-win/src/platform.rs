//! Windows MMCSS (Multimedia Class Scheduler Service) helpers.
//!
//! MMCSS boosts thread scheduling priority for multimedia tasks (video,
//! audio, gaming). We use the "Games" task for capture and render threads
//! per spec §3.1.

use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::HANDLE;
use windows::Win32::System::Threading::{
    AvRevertMmThreadCharacteristics, AvSetMmThreadCharacteristicsW,
};

use crate::error::{MediaError, Result};

/// RAII handle for an MMCSS task registration. Drops restore default
/// scheduling for the thread.
pub struct MmcssScope {
    handle: HANDLE,
}

impl MmcssScope {
    /// Register the current thread with the MMCSS "Games" task.
    pub fn games() -> Result<Self> {
        Self::with_task(w!("Games"))
    }

    /// Register with an arbitrary MMCSS task name. See
    /// https://learn.microsoft.com/en-us/windows/win32/procthread/multimedia-class-scheduler-service
    /// for the standard names ("Audio", "Capture", "Games", "Playback",
    /// "Pro Audio").
    pub fn with_task(task: PCWSTR) -> Result<Self> {
        let mut task_index: u32 = 0;
        let handle = unsafe {
            AvSetMmThreadCharacteristicsW(task, &mut task_index).map_err(|e| {
                MediaError::MmcssFailed {
                    reason: format!("AvSetMmThreadCharacteristicsW: {e}"),
                }
            })?
        };
        tracing::debug!(task_index, "MMCSS task attached");
        Ok(Self { handle })
    }
}

impl Drop for MmcssScope {
    fn drop(&mut self) {
        unsafe {
            let _ = AvRevertMmThreadCharacteristics(self.handle);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn games_task_attaches_and_detaches() {
        // On most dev machines MMCSS is available. On headless CI without
        // the audiosrv/MMCSS service it may fail; we treat that as a warn
        // and pass.
        match MmcssScope::games() {
            Ok(scope) => {
                drop(scope);
            }
            Err(MediaError::MmcssFailed { reason }) => {
                eprintln!("MMCSS unavailable on this machine: {reason}");
            }
            Err(e) => panic!("unexpected error: {e}"),
        }
    }
}
