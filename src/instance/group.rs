//! Test support, no dependency on the crate: compiled into the library's tests
//! (`src/instance/mod.rs`) and included by path into the integration tests that kill instance groups
//! (`tests/instance_lok.rs`, `tests/pool_lok.rs`), so the rule exists once.

/// For tests: whether process group `pgid` has a live member. A zombie is not one: an orphan of
/// a killed group is reparented to PID 1 and stays a zombie in the group until PID 1 reaps it, and a CI job
/// container's PID 1 never does, so `kill(-pgid, 0)`, which still succeeds then, cannot tell "gone".
#[allow(
    unsafe_code,
    reason = "signal 0 to a process group, which std has no form of"
)]
pub fn group_alive(pgid: u32) -> bool {
    #[cfg(target_os = "linux")]
    {
        let Ok(entries) = std::fs::read_dir("/proc") else {
            return false;
        };
        entries.flatten().any(|entry| {
            let Ok(stat) = std::fs::read_to_string(entry.path().join("stat")) else {
                return false;
            };
            // `pid (comm) state ppid pgrp …`; comm may hold spaces and parentheses, so split after the last `)`.
            let Some((_, rest)) = stat.rsplit_once(')') else {
                return false;
            };
            let mut fields = rest.split_whitespace();
            let state = fields.next();
            let pgrp = fields.nth(1).and_then(|f| f.parse::<u32>().ok());
            pgrp == Some(pgid) && !matches!(state, Some("Z" | "X"))
        })
    }
    #[cfg(not(target_os = "linux"))]
    {
        // SAFETY: signal 0 only checks that the group exists. No /proc here; macOS's launchd reaps orphans.
        unsafe { libc::kill(-pgid.cast_signed(), 0) == 0 }
    }
}
