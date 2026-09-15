#[derive(Clone, Copy)]
pub struct Snapshot {
    user_seconds: f64,
    system_seconds: f64,
}

#[derive(Clone, Copy)]
pub struct Usage {
    pub user_seconds: f64,
    pub system_seconds: f64,
}

impl Usage {
    pub fn average_busy_cores(self, wall_seconds: f64) -> f64 {
        (self.user_seconds + self.system_seconds) / wall_seconds
    }
}

#[cfg(unix)]
pub fn snapshot() -> Option<Snapshot> {
    use std::mem::MaybeUninit;

    let mut usage = MaybeUninit::<libc::rusage>::uninit();
    // SAFETY: getrusage initializes the pointed-to rusage on success. The pointer
    // is valid and uniquely borrowed for the duration of this call.
    let status = unsafe { libc::getrusage(libc::RUSAGE_SELF, usage.as_mut_ptr()) };
    assert_eq!(status, 0, "getrusage(RUSAGE_SELF) failed");
    // SAFETY: the successful getrusage call above initialized the whole value.
    let usage = unsafe { usage.assume_init() };
    Some(Snapshot {
        user_seconds: timeval_seconds(usage.ru_utime),
        system_seconds: timeval_seconds(usage.ru_stime),
    })
}

#[cfg(unix)]
fn timeval_seconds(value: libc::timeval) -> f64 {
    value.tv_sec as f64 + value.tv_usec as f64 / 1_000_000.0
}

#[cfg(not(unix))]
pub fn snapshot() -> Option<Snapshot> {
    None
}

pub fn elapsed(start: Option<Snapshot>, end: Option<Snapshot>) -> Option<Usage> {
    let (start, end) = (start?, end?);
    Some(Usage {
        user_seconds: (end.user_seconds - start.user_seconds).max(0.0),
        system_seconds: (end.system_seconds - start.system_seconds).max(0.0),
    })
}
