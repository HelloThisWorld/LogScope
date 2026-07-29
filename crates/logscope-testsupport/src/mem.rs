//! Process peak-memory measurement for benchmark evidence.

/// Peak working-set size of the current process in bytes (Windows), or
/// `None` where unsupported.
#[cfg(windows)]
pub fn peak_working_set_bytes() -> Option<u64> {
    use windows_sys::Win32::System::ProcessStatus::{
        K32GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS,
    };
    use windows_sys::Win32::System::Threading::GetCurrentProcess;
    // SAFETY: querying the current process with a properly sized counters
    // struct is the documented use of K32GetProcessMemoryInfo.
    unsafe {
        let mut counters: PROCESS_MEMORY_COUNTERS = std::mem::zeroed();
        counters.cb = std::mem::size_of::<PROCESS_MEMORY_COUNTERS>() as u32;
        let ok = K32GetProcessMemoryInfo(GetCurrentProcess(), &mut counters, counters.cb);
        (ok != 0).then_some(counters.PeakWorkingSetSize as u64)
    }
}

#[cfg(not(windows))]
pub fn peak_working_set_bytes() -> Option<u64> {
    None
}
