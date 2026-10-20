//! Show messages in a user's session and sign users out.

use anyhow::{Context, Result};
use windows::Win32::System::RemoteDesktop::{
    WTS_CURRENT_SERVER_HANDLE, WTSLogoffSession, WTSSendMessageW,
};
use windows::Win32::UI::WindowsAndMessaging::{
    MB_ICONWARNING, MB_OK, MB_SETFOREGROUND, MB_TOPMOST, MESSAGEBOX_RESULT,
};
use windows::core::HSTRING;

/// Shows a message box in the session. Does not wait for the user to close it.
pub fn send_message(session: u32, title: &str, text: &str) -> Result<()> {
    let title = HSTRING::from(title);
    let text = HSTRING::from(text);
    let mut response = MESSAGEBOX_RESULT::default();
    unsafe {
        WTSSendMessageW(
            Some(WTS_CURRENT_SERVER_HANDLE),
            session,
            &title,
            (title.len() * 2) as u32,
            &text,
            (text.len() * 2) as u32,
            MB_OK | MB_ICONWARNING | MB_SETFOREGROUND | MB_TOPMOST,
            0,
            &mut response,
            false,
        )
    }
    .context("WTSSendMessageW failed")
}

/// Signs the session out. Programs are closed without saving.
pub fn logoff(session: u32) -> Result<()> {
    unsafe { WTSLogoffSession(Some(WTS_CURRENT_SERVER_HANDLE), session, false) }
        .context("WTSLogoffSession failed")
}
