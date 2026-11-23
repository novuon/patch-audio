//! The UI's heap: bytes in use and unused bytes held by the active allocator.

#[repr(C)]
#[derive(Default)]
#[cfg(all(target_env = "gnu", not(feature = "jemalloc")))]
struct MallInfo2 {
    arena: usize,
    ordblks: usize,
    smblks: usize,
    hblks: usize,
    hblkhd: usize,
    usmblks: usize,
    fsmblks: usize,
    uordblks: usize,
    fordblks: usize,
    keepcost: usize,
}

#[cfg(all(target_env = "gnu", not(feature = "jemalloc")))]
unsafe extern "C" {
    fn mallinfo2() -> MallInfo2;
}

#[cfg(feature = "jemalloc")]
fn bytes() -> Option<(usize, usize)> {
    fn read<T: Default>(name: &std::ffi::CStr, refresh: bool) -> Option<T> {
        let mut value = T::default();
        let mut len = std::mem::size_of::<T>();
        let mut epoch = 1_u64;
        // SAFETY: callers use the documented mallctl types (u64 for epoch,
        // size_t for stats). All pointers are valid for their specified sizes.
        let rc = unsafe {
            tikv_jemalloc_sys::mallctl(
                name.as_ptr(),
                (&mut value as *mut T).cast(),
                &mut len,
                if refresh {
                    (&mut epoch as *mut u64).cast()
                } else {
                    std::ptr::null_mut()
                },
                if refresh {
                    std::mem::size_of::<u64>()
                } else {
                    0
                },
            )
        };
        (rc == 0).then_some(value)
    }
    read::<u64>(c"epoch", true)?;
    let allocated = read::<usize>(c"stats.allocated", false)?;
    let resident = read::<usize>(c"stats.resident", false)?;
    // Resident includes allocator metadata as well as unused resident pages.
    Some((allocated, resident.saturating_sub(allocated)))
}

#[cfg(not(feature = "jemalloc"))]
fn bytes() -> Option<(usize, usize)> {
    #[cfg(target_env = "gnu")]
    {
        // SAFETY: a plain query of the allocator's counters.
        let m = unsafe { mallinfo2() };
        Some((m.uordblks + m.hblkhd, m.fordblks))
    }
    #[cfg(not(target_env = "gnu"))]
    None
}

/// (in use, unused resident memory including allocator overhead) in MB.
pub fn stats() -> Option<(u64, u64)> {
    bytes().map(|(used, held)| ((used / (1024 * 1024)) as u64, (held / (1024 * 1024)) as u64))
}

/// Heap in use (MB, fractional), for startup logs.
pub fn in_use_mb() -> f64 {
    bytes().map_or(0.0, |(used, _)| used as f64 / (1024.0 * 1024.0))
}
