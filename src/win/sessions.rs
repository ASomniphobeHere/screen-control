//! List active sessions with their user SID and lock state.

use anyhow::{Context, Result};
use windows::Win32::Foundation::{
    CloseHandle, ERROR_NO_TOKEN, ERROR_PRIVILEGE_NOT_HELD, HANDLE, HLOCAL, LocalFree,
};
use windows::Win32::Security::Authorization::ConvertSidToStringSidW;
use windows::Win32::Security::{
    GetTokenInformation, LookupAccountNameW, PSID, SID_NAME_USE, TOKEN_USER, TokenUser,
};
use windows::Win32::System::RemoteDesktop::{
    WTS_CURRENT_SERVER_HANDLE, WTS_INFO_CLASS, WTS_SESSION_INFOW, WTS_SESSIONSTATE_LOCK, WTSActive,
    WTSDomainName, WTSEnumerateSessionsW, WTSFreeMemory, WTSINFOEXW, WTSQuerySessionInformationW,
    WTSQueryUserToken, WTSSessionInfoEx, WTSUserName,
};
use windows::core::{HSTRING, PCWSTR, PWSTR};

use crate::core::engine::Session;

/// Sessions in the active state (the one on the screen) that have a user.
pub fn active_sessions() -> Result<Vec<Session>> {
    let mut sessions = Vec::new();
    for id in active_session_ids()? {
        match user_sid(id) {
            Ok(Some(sid)) => sessions.push(Session {
                id,
                sid,
                locked: is_locked(id),
            }),
            Ok(None) => tracing::debug!(session = id, "active session without a user"),
            Err(e) => tracing::warn!(
                session = id,
                error = format!("{e:#}"),
                "cannot get the user of a session"
            ),
        }
    }
    Ok(sessions)
}

/// IDs of the sessions in the active state. Cheap enough to call every second.
pub fn active_session_ids() -> Result<Vec<u32>> {
    let mut list: *mut WTS_SESSION_INFOW = std::ptr::null_mut();
    let mut count = 0u32;
    unsafe { WTSEnumerateSessionsW(Some(WTS_CURRENT_SERVER_HANDLE), 0, 1, &mut list, &mut count) }
        .context("WTSEnumerateSessionsW failed")?;
    let ids = unsafe { std::slice::from_raw_parts(list, count as usize) }
        .iter()
        .filter(|s| s.State == WTSActive)
        .map(|s| s.SessionId)
        .collect();
    unsafe { WTSFreeMemory(list.cast()) };
    Ok(ids)
}

/// The user SID as text, like `S-1-5-21-...`. `None` if nobody is signed in.
fn user_sid(session: u32) -> Result<Option<String>> {
    let mut token = HANDLE::default();
    match unsafe { WTSQueryUserToken(session, &mut token) } {
        Ok(()) => {
            let sid = token_sid(token);
            let _ = unsafe { CloseHandle(token) };
            sid.map(Some)
        }
        Err(e) if e.code() == ERROR_NO_TOKEN.to_hresult() => Ok(None),
        // Only SYSTEM may read user tokens. In console mode as an admin, look up the user name instead.
        Err(e) if e.code() == ERROR_PRIVILEGE_NOT_HELD.to_hresult() => sid_from_user_name(session),
        Err(e) => Err(e).context("WTSQueryUserToken failed"),
    }
}

fn token_sid(token: HANDLE) -> Result<String> {
    let mut len = 0u32;
    // The first call fails and gives the needed size.
    let _ = unsafe { GetTokenInformation(token, TokenUser, None, 0, &mut len) };
    let mut buf = vec![0u64; (len as usize).div_ceil(8)];
    unsafe {
        GetTokenInformation(
            token,
            TokenUser,
            Some(buf.as_mut_ptr().cast()),
            len,
            &mut len,
        )
    }
    .context("GetTokenInformation failed")?;
    let user = unsafe { &*(buf.as_ptr() as *const TOKEN_USER) };
    sid_to_string(user.User.Sid)
}

fn sid_to_string(sid: PSID) -> Result<String> {
    let mut text = PWSTR::null();
    unsafe { ConvertSidToStringSidW(sid, &mut text) }.context("ConvertSidToStringSidW failed")?;
    let result = unsafe { text.to_string() };
    unsafe { LocalFree(Some(HLOCAL(text.0.cast()))) };
    Ok(result?)
}

fn sid_from_user_name(session: u32) -> Result<Option<String>> {
    let user = query_string(session, WTSUserName)?;
    if user.is_empty() {
        return Ok(None);
    }
    let domain = query_string(session, WTSDomainName)?;
    let account = HSTRING::from(format!("{domain}\\{user}"));

    let (mut sid_len, mut domain_len) = (0u32, 0u32);
    let mut kind = SID_NAME_USE::default();
    // The first call fails and gives the needed sizes.
    let _ = unsafe {
        LookupAccountNameW(
            PCWSTR::null(),
            &account,
            None,
            &mut sid_len,
            None,
            &mut domain_len,
            &mut kind,
        )
    };
    let mut sid = vec![0u64; (sid_len as usize).div_ceil(8)];
    let mut domain_buf = vec![0u16; domain_len as usize];
    unsafe {
        LookupAccountNameW(
            PCWSTR::null(),
            &account,
            Some(PSID(sid.as_mut_ptr().cast())),
            &mut sid_len,
            Some(PWSTR(domain_buf.as_mut_ptr())),
            &mut domain_len,
            &mut kind,
        )
    }
    .with_context(|| format!("LookupAccountNameW failed for {account}"))?;
    sid_to_string(PSID(sid.as_mut_ptr().cast())).map(Some)
}

fn query_string(session: u32, class: WTS_INFO_CLASS) -> Result<String> {
    let mut buf = PWSTR::null();
    let mut bytes = 0u32;
    unsafe {
        WTSQuerySessionInformationW(
            Some(WTS_CURRENT_SERVER_HANDLE),
            session,
            class,
            &mut buf,
            &mut bytes,
        )
    }
    .context("WTSQuerySessionInformationW failed")?;
    let text = unsafe { buf.to_string() };
    unsafe { WTSFreeMemory(buf.0.cast()) };
    Ok(text?)
}

/// If the lock state cannot be read, the session counts as unlocked, so time is not lost.
fn is_locked(session: u32) -> bool {
    let mut buf = PWSTR::null();
    let mut bytes = 0u32;
    let result = unsafe {
        WTSQuerySessionInformationW(
            Some(WTS_CURRENT_SERVER_HANDLE),
            session,
            WTSSessionInfoEx,
            &mut buf,
            &mut bytes,
        )
    };
    if let Err(e) = result {
        tracing::warn!(session, error = %e, "cannot read the lock state, counting as unlocked");
        return false;
    }
    let info = unsafe { &*(buf.0 as *const WTSINFOEXW) };
    let flags = if info.Level == 1 {
        unsafe { info.Data.WTSInfoExLevel1.SessionFlags }
    } else {
        -1
    };
    unsafe { WTSFreeMemory(buf.0.cast()) };
    flags == WTS_SESSIONSTATE_LOCK as i32
}
