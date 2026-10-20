use std::time::Duration;

use windows::Win32::System::WindowsProgramming::QueryUnbiasedInterruptTime;

/// Time since boot, without time spent in sleep or hibernation.
pub fn unbiased_now() -> Duration {
    let mut ticks = 0u64; // in 100 ns units
    // Cannot fail on Windows 7 and later.
    let _ = unsafe { QueryUnbiasedInterruptTime(&mut ticks) };
    Duration::from_nanos(ticks.saturating_mul(100))
}
