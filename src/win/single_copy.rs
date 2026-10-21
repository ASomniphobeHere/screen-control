//! Lets only one copy of a program run, with a named mutex.

use anyhow::{Context, Result};
use windows::Win32::Foundation::{CloseHandle, ERROR_ALREADY_EXISTS, GetLastError, HANDLE};
use windows::Win32::System::Threading::CreateMutexW;
use windows::core::HSTRING;

/// Held while this copy runs. Windows removes the mutex when the last copy that has it ends,
/// even if it is killed.
pub struct SingleCopy(HANDLE);

impl SingleCopy {
    /// `None` if another copy already holds the name. A `Local\` name is per session.
    pub fn claim(name: &str) -> Result<Option<Self>> {
        let wide = HSTRING::from(name);
        let handle = unsafe { CreateMutexW(None, false, &wide) }
            .with_context(|| format!("CreateMutexW failed for {name}"))?;
        // Must be read at once, before another call changes it.
        if unsafe { GetLastError() } == ERROR_ALREADY_EXISTS {
            let _ = unsafe { CloseHandle(handle) };
            return Ok(None);
        }
        Ok(Some(SingleCopy(handle)))
    }
}

impl Drop for SingleCopy {
    fn drop(&mut self) {
        let _ = unsafe { CloseHandle(self.0) };
    }
}
