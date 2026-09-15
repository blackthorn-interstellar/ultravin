//! Process-local instruction/cycle counters for complete timed benchmark passes.

#[derive(Clone, Copy, serde::Serialize)]
pub struct Snapshot {
    pub instructions: u64,
    pub cycles: u64,
}

#[cfg(target_os = "macos")]
pub fn snapshot() -> Option<Snapshot> {
    let mut usage = std::mem::MaybeUninit::<libc::rusage_info_v4>::uninit();
    // SAFETY: V4 specifies the initialized buffer layout; the buffer is valid
    // and uniquely borrowed. proc_pid_rusage's historical API takes a void**.
    let status = unsafe {
        libc::proc_pid_rusage(
            libc::getpid(),
            libc::RUSAGE_INFO_V4,
            usage.as_mut_ptr().cast(),
        )
    };
    if status != 0 {
        return None;
    }
    // SAFETY: the successful call initialized the V4 structure.
    let usage = unsafe { usage.assume_init() };
    (usage.ri_instructions > 0 && usage.ri_cycles > 0).then_some(Snapshot {
        instructions: usage.ri_instructions,
        cycles: usage.ri_cycles,
    })
}

#[cfg(not(target_os = "macos"))]
pub fn snapshot() -> Option<Snapshot> {
    None
}

pub fn elapsed(start: Option<Snapshot>, end: Option<Snapshot>) -> Option<Snapshot> {
    let (start, end) = (start?, end?);
    Some(Snapshot {
        instructions: end.instructions.checked_sub(start.instructions)?,
        cycles: end.cycles.checked_sub(start.cycles)?,
    })
}
