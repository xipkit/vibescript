#[cfg(target_os = "macos")]
/// Returns the process's current resident memory in bytes.
pub fn current() -> Option<usize> {
    let mut info = std::mem::MaybeUninit::<libc::mach_task_basic_info_data_t>::uninit();
    let mut count = libc::MACH_TASK_BASIC_INFO_COUNT;
    // SAFETY: task_info writes at most `count` integer words to the correctly
    // sized information structure; it is read only after a successful call.
    #[allow(deprecated)]
    let result = unsafe {
        libc::task_info(
            libc::mach_task_self(),
            libc::MACH_TASK_BASIC_INFO,
            info.as_mut_ptr().cast(),
            &mut count,
        )
    };
    if result != libc::KERN_SUCCESS {
        return None;
    }
    // SAFETY: A successful task_info initialized the structure above.
    Some(unsafe { info.assume_init() }.resident_size as usize)
}

#[cfg(target_os = "linux")]
/// Returns the process's current resident memory in bytes.
pub fn current() -> Option<usize> {
    use std::{fs::File, io::Read};

    let mut buffer = [0_u8; 256];
    let length = File::open("/proc/self/statm")
        .ok()?
        .read(&mut buffer)
        .ok()?;
    // SAFETY: sysconf takes a valid configuration name and no pointers.
    let page = unsafe { libc::sysconf(libc::_SC_PAGESIZE) };
    if length == 0 || page <= 0 {
        return None;
    }
    std::str::from_utf8(&buffer[..length])
        .ok()?
        .split_whitespace()
        .nth(1)?
        .parse::<usize>()
        .ok()?
        .checked_mul(page as usize)
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
/// Returns no measurement on platforms without an RSS implementation.
pub fn current() -> Option<usize> {
    None
}
