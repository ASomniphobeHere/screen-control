//! Starts the helper program in a user's session, as that user.

use std::os::windows::ffi::OsStrExt;
use std::path::Path;

use anyhow::{Context, Result};
use windows::Win32::Foundation::{CloseHandle, HANDLE};
use windows::Win32::Security::{
    DuplicateTokenEx, SecurityImpersonation, TOKEN_ALL_ACCESS, TokenPrimary,
};
use windows::Win32::System::Environment::{CreateEnvironmentBlock, DestroyEnvironmentBlock};
use windows::Win32::System::RemoteDesktop::WTSQueryUserToken;
use windows::Win32::System::Threading::{
    CREATE_NO_WINDOW, CREATE_UNICODE_ENVIRONMENT, CreateProcessAsUserW, PROCESS_INFORMATION,
    STARTUPINFOW,
};
use windows::core::{PCWSTR, PWSTR};

/// The desktop the user sees.
const DESKTOP: &str = r"winsta0\default";

/// Starts `<this exe> helper` in the session as its user, with no console window.
/// Returns the new process ID. Only works when running as SYSTEM.
pub fn start_helper(session: u32) -> Result<u32> {
    let exe = std::env::current_exe().context("cannot find this program's path")?;
    let user = Handle::user_token(session)?;
    let token = user.primary()?;
    let env = Environment::new(&token)?;

    let exe_w = wide(exe.as_os_str());
    let mut command_line = wide(command_line(&exe).as_ref());
    let mut desktop = wide(DESKTOP.as_ref());
    let startup = STARTUPINFOW {
        cb: size_of::<STARTUPINFOW>() as u32,
        lpDesktop: PWSTR(desktop.as_mut_ptr()),
        ..Default::default()
    };
    let mut info = PROCESS_INFORMATION::default();
    unsafe {
        CreateProcessAsUserW(
            Some(token.0),
            PCWSTR(exe_w.as_ptr()),
            Some(PWSTR(command_line.as_mut_ptr())),
            None,
            None,
            false,
            CREATE_NO_WINDOW | CREATE_UNICODE_ENVIRONMENT,
            Some(env.0),
            PCWSTR::null(),
            &startup,
            &mut info,
        )
    }
    .context("CreateProcessAsUserW failed")?;
    // Not needed: the helper runs on its own.
    drop(Handle(info.hThread));
    drop(Handle(info.hProcess));
    Ok(info.dwProcessId)
}

/// The path in quotes, as it may have spaces.
fn command_line(exe: &Path) -> String {
    format!("\"{}\" helper", exe.display())
}

/// Text for Windows: UTF-16 that ends with a zero.
fn wide(text: &std::ffi::OsStr) -> Vec<u16> {
    text.encode_wide().chain(Some(0)).collect()
}

/// A handle that is closed on drop.
struct Handle(HANDLE);

impl Handle {
    fn user_token(session: u32) -> Result<Self> {
        let mut token = HANDLE::default();
        unsafe { WTSQueryUserToken(session, &mut token) }.context("WTSQueryUserToken failed")?;
        Ok(Handle(token))
    }

    /// A primary token copy, as `CreateProcessAsUserW` needs.
    fn primary(&self) -> Result<Self> {
        let mut token = HANDLE::default();
        unsafe {
            DuplicateTokenEx(
                self.0,
                TOKEN_ALL_ACCESS,
                None,
                SecurityImpersonation,
                TokenPrimary,
                &mut token,
            )
        }
        .context("DuplicateTokenEx failed")?;
        Ok(Handle(token))
    }
}

impl Drop for Handle {
    fn drop(&mut self) {
        if !self.0.is_invalid() {
            let _ = unsafe { CloseHandle(self.0) };
        }
    }
}

/// The user's environment variables, like `LOCALAPPDATA`. Destroyed on drop.
struct Environment(*mut core::ffi::c_void);

impl Environment {
    fn new(token: &Handle) -> Result<Self> {
        let mut block = std::ptr::null_mut();
        unsafe { CreateEnvironmentBlock(&mut block, Some(token.0), false) }
            .context("CreateEnvironmentBlock failed")?;
        Ok(Environment(block))
    }
}

impl Drop for Environment {
    fn drop(&mut self) {
        let _ = unsafe { DestroyEnvironmentBlock(self.0) };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn command_line_quotes_the_path() {
        assert_eq!(
            command_line(Path::new(
                r"C:\Program Files\ScreenControl\screen-control.exe"
            )),
            r#""C:\Program Files\ScreenControl\screen-control.exe" helper"#
        );
    }
}
