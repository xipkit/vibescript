/// Pauses a diagnostic process so its parent can collect resident mappings.
pub fn pause(stage: &str) {
    #[cfg(unix)]
    {
        // SAFETY: The environment key is NUL terminated. The returned pointer
        // is only tested, never retained or dereferenced.
        if unsafe { libc::getenv(c"VIBESCRIPT_FOOTPRINT_SNAPSHOT".as_ptr()).is_null() } {
            return;
        }
        report(stage, false, 0);
        if command() == b't' {
            let now = std::time::Instant::now();
            release();
            report(stage, true, now.elapsed().as_nanos());
            command();
        }
    }
    #[cfg(not(unix))]
    let _ = stage;
}

#[cfg(unix)]
fn command() -> u8 {
    let mut byte = 0_u8;
    assert_eq!(
        // SAFETY: The writable buffer holds the one byte read from stdin.
        unsafe { libc::read(0, (&mut byte as *mut u8).cast(), 1) },
        1
    );
    byte
}

#[cfg(unix)]
fn report(stage: &str, trimmed: bool, trim_ns: u128) {
    let (used, reserved) = allocator();
    let rss = super::rss::current().unwrap_or(0);
    eprintln!(
        "{{\"stage\":\"{stage}\",\"trimmed\":{trimmed},\"trim_ns\":{trim_ns},\"rss_bytes\":{rss},\"allocator_used_bytes\":{used},\"allocator_reserved_bytes\":{reserved}}}"
    );
}

#[cfg(target_os = "macos")]
fn release() {
    unsafe extern "C" {
        fn malloc_zone_pressure_relief(zone: *mut libc::malloc_zone_t, goal: usize) -> usize;
    }
    // SAFETY: A null zone and zero goal ask all zones to release unused pages.
    unsafe { malloc_zone_pressure_relief(std::ptr::null_mut(), 0) };
}

#[cfg(all(target_os = "linux", target_env = "gnu"))]
fn release() {
    // SAFETY: malloc_trim releases only allocator-owned unused pages.
    unsafe { libc::malloc_trim(0) };
}

#[cfg(all(
    unix,
    not(any(target_os = "macos", all(target_os = "linux", target_env = "gnu")))
))]
fn release() {}

#[cfg(target_os = "macos")]
fn allocator() -> (usize, usize) {
    let mut stats = std::mem::MaybeUninit::<libc::malloc_statistics_t>::uninit();
    // SAFETY: A null zone requests aggregate statistics, written to a correctly
    // sized output structure before it is read.
    unsafe {
        libc::malloc_zone_statistics(std::ptr::null_mut(), stats.as_mut_ptr());
        let stats = stats.assume_init();
        (stats.size_in_use, stats.size_allocated)
    }
}

#[cfg(all(target_os = "linux", target_env = "gnu"))]
fn allocator() -> (usize, usize) {
    // SAFETY: mallinfo2 has no arguments and returns aggregate allocator data.
    let info = unsafe { libc::mallinfo2() };
    (info.uordblks + info.hblkhd, info.arena + info.hblkhd)
}

#[cfg(all(
    unix,
    not(any(target_os = "macos", all(target_os = "linux", target_env = "gnu")))
))]
fn allocator() -> (usize, usize) {
    (0, 0)
}
