//! The service process's own metrics: `process.cpu.time`, `process.memory.usage` and
//! `process.unix.file_descriptor.count`, observed at each collection (a `/metrics` scrape, an OTLP export).
//!
//! Kind, unit and attributes are the OpenTelemetry semantic conventions' (semconv v1.44.0), so
//! a standard dashboard reads them:
//! `process.cpu.time` is a counter in `s` with `cpu.mode` = `user` or `system`; the other two are up-down
//! counters in `By` and `{file_descriptor}`.
//!
//! Sources: CPU time from `getrusage` on every Unix; the resident set size from `/proc/self/statm` on Linux
//! and from Mach `task_info` on macOS; open descriptors by listing `/proc/self/fd` on Linux and `/dev/fd`
//! on macOS. Where a source does not exist, that instrument reports nothing rather than a wrong value.

use opentelemetry::KeyValue;
use opentelemetry::metrics::{Meter, ObservableCounter, ObservableUpDownCounter};

#[cfg(unix)]
use crate::num::ToF64;

/// The registered instruments; their callbacks run for as long as the meter provider lives.
pub struct ProcessMetrics {
    _cpu_time: ObservableCounter<f64>,
    _memory_usage: ObservableUpDownCounter<i64>,
    _open_file_descriptors: ObservableUpDownCounter<i64>,
}

/// Registers the process instruments on `meter`.
pub fn register(meter: &Meter) -> ProcessMetrics {
    let cpu_time = meter
        .f64_observable_counter("process.cpu.time")
        .with_unit("s")
        .with_description("CPU seconds the service process has used, by CPU mode")
        .with_callback(|observer| {
            if let Some(cpu) = cpu_time() {
                observer.observe(cpu.user, &[KeyValue::new("cpu.mode", "user")]);
                observer.observe(cpu.system, &[KeyValue::new("cpu.mode", "system")]);
            }
        })
        .build();
    let memory_usage = meter
        .i64_observable_up_down_counter("process.memory.usage")
        .with_unit("By")
        .with_description("The service process's resident set size")
        .with_callback(|observer| {
            if let Some(bytes) = resident_bytes() {
                observer.observe(saturating_i64(bytes), &[]);
            }
        })
        .build();
    let open_file_descriptors = meter
        .i64_observable_up_down_counter("process.unix.file_descriptor.count")
        .with_unit("{file_descriptor}")
        .with_description("File descriptors the service process holds open")
        .with_callback(|observer| {
            if let Some(count) = open_file_descriptors() {
                observer.observe(saturating_i64(count), &[]);
            }
        })
        .build();
    ProcessMetrics {
        _cpu_time: cpu_time,
        _memory_usage: memory_usage,
        _open_file_descriptors: open_file_descriptors,
    }
}

fn saturating_i64(n: u64) -> i64 {
    i64::try_from(n).unwrap_or(i64::MAX)
}

/// CPU seconds used so far.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CpuTime {
    /// In user mode.
    pub user: f64,
    /// In the kernel on the process's behalf.
    pub system: f64,
}

/// The process's CPU time so far, from `getrusage(RUSAGE_SELF)`; every thread counts.
///
/// On Linux the total is exact but its split into user and system follows the mode each timer tick
/// sampled, so a process younger than a tick or two can show all of its time in one mode and 0 in the
/// other. The kernel keeps both readings monotonic; this returns them as they are.
#[cfg(unix)]
#[allow(
    unsafe_code,
    reason = "the process's own counters (getrusage, sysconf, the Mach and proc calls), which std has no form of"
)]
pub fn cpu_time() -> Option<CpuTime> {
    // SAFETY: `getrusage` only writes the `rusage` it is given, which is plain data and valid zeroed.
    let usage = unsafe {
        let mut usage: libc::rusage = std::mem::zeroed();
        if libc::getrusage(libc::RUSAGE_SELF, &raw mut usage) != 0 {
            return None;
        }
        usage
    };
    // timeval's fields are i32 on macOS and i64 on Linux; `ToF64` serves both.
    let seconds = |t: libc::timeval| t.tv_sec.to_f64() + t.tv_usec.to_f64() / 1e6;
    Some(CpuTime {
        user: seconds(usage.ru_utime),
        system: seconds(usage.ru_stime),
    })
}

/// No source off Unix.
#[cfg(not(unix))]
pub fn cpu_time() -> Option<CpuTime> {
    None
}

/// The resident set size in bytes: resident pages in `/proc/self/statm` times the page size.
#[cfg(target_os = "linux")]
#[allow(
    unsafe_code,
    reason = "the process's own counters (getrusage, sysconf, the Mach and proc calls), which std has no form of"
)]
pub fn resident_bytes() -> Option<u64> {
    let statm = std::fs::read_to_string("/proc/self/statm").ok()?;
    let pages: u64 = statm.split_whitespace().nth(1)?.parse().ok()?;
    // SAFETY: `sysconf` reads a configuration value and has no memory effects.
    let page_size = unsafe { libc::sysconf(libc::_SC_PAGESIZE) };
    let page_size = u64::try_from(page_size).ok().filter(|&p| p > 0)?;
    pages.checked_mul(page_size)
}

/// The resident set size in bytes, from Mach `task_info(MACH_TASK_BASIC_INFO)`.
///
/// `libc` deprecates its Mach bindings in favour of the `mach2` crate; macOS is only a development
/// platform here, so the two calls are not worth a dependency.
#[cfg(target_os = "macos")]
#[allow(deprecated)]
#[allow(
    unsafe_code,
    reason = "the process's own counters (getrusage, sysconf, the Mach and proc calls), which std has no form of"
)]
pub fn resident_bytes() -> Option<u64> {
    // SAFETY: `task_info` writes at most `count` integers into the zeroed `info` it is given, and
    // `count` is that struct's size in integers.
    unsafe {
        let mut info: libc::mach_task_basic_info = std::mem::zeroed();
        let mut count = libc::MACH_TASK_BASIC_INFO_COUNT;
        let status = libc::task_info(
            libc::mach_task_self(),
            libc::MACH_TASK_BASIC_INFO,
            (&raw mut info).cast(),
            &raw mut count,
        );
        (status == libc::KERN_SUCCESS).then_some(info.resident_size)
    }
}

/// No source on this platform.
#[cfg(not(any(target_os = "linux", target_os = "macos")))]
pub fn resident_bytes() -> Option<u64> {
    None
}

/// Another process's resident set size in bytes (an instance's, for the memory ceiling and
/// `docx_to_pdf.instance.memory.usage`): `/proc/<pid>/statm` on Linux, `proc_pidinfo` on
/// macOS. `None` when the process is gone or the platform has no source.
///
/// On Linux the reading lags: since 6.2 the kernel counts resident pages per CPU and `statm` shows only
/// what has been folded into the total, in steps of `max(32, 2 × CPUs)` pages, so a process that has
/// just exec'd and faulted fewer pages than that reads 0 although its pages are resident.
#[cfg(target_os = "linux")]
#[allow(
    unsafe_code,
    reason = "the process's own counters (getrusage, sysconf, the Mach and proc calls), which std has no form of"
)]
pub fn resident_bytes_of(pid: u32) -> Option<u64> {
    let statm = std::fs::read_to_string(format!("/proc/{pid}/statm")).ok()?;
    let pages: u64 = statm.split_whitespace().nth(1)?.parse().ok()?;
    // SAFETY: `sysconf` reads a configuration value and has no memory effects.
    let page_size = unsafe { libc::sysconf(libc::_SC_PAGESIZE) };
    let page_size = u64::try_from(page_size).ok().filter(|&p| p > 0)?;
    pages.checked_mul(page_size)
}

/// Resets another process's resident high-water mark (`VmHWM`) to its current resident size, by writing `5`
/// to `/proc/<pid>/clear_refs` (proc(5)); whether it took is not reported, a later [`peak_resident_bytes_of`]
/// then reads a mark from before.
#[cfg(target_os = "linux")]
pub fn reset_peak_of(pid: u32) {
    let _ = std::fs::write(format!("/proc/{pid}/clear_refs"), "5");
}

/// No high-water mark to reset off Linux.
#[cfg(not(target_os = "linux"))]
pub fn reset_peak_of(_pid: u32) {}

/// Another process's peak resident set size in bytes since it started or since [`reset_peak_of`]: `VmHWM` in
/// `/proc/<pid>/status`. None for a process that is gone (a zombie has no `VmHWM` line).
#[cfg(target_os = "linux")]
pub fn peak_resident_bytes_of(pid: u32) -> Option<u64> {
    let status = std::fs::read_to_string(format!("/proc/{pid}/status")).ok()?;
    let line = status.lines().find_map(|l| l.strip_prefix("VmHWM:"))?;
    let kib: u64 = line.trim().strip_suffix("kB")?.trim().parse().ok()?;
    kib.checked_mul(1024)
}

/// No resettable peak off Linux; macOS is only a development platform here.
#[cfg(not(target_os = "linux"))]
pub fn peak_resident_bytes_of(_pid: u32) -> Option<u64> {
    None
}

/// Another process's resident set size in bytes, from `proc_pidinfo(PROC_PIDTASKINFO)`.
#[cfg(target_os = "macos")]
#[allow(
    unsafe_code,
    reason = "the process's own counters (getrusage, sysconf, the Mach and proc calls), which std has no form of"
)]
pub fn resident_bytes_of(pid: u32) -> Option<u64> {
    let pid = i32::try_from(pid).ok()?;
    let size = i32::try_from(std::mem::size_of::<libc::proc_taskinfo>()).ok()?;
    // SAFETY: `proc_pidinfo` writes at most `size` bytes into the zeroed `info` it is given, and `size` is
    // that struct's size.
    unsafe {
        let mut info: libc::proc_taskinfo = std::mem::zeroed();
        let written =
            libc::proc_pidinfo(pid, libc::PROC_PIDTASKINFO, 0, (&raw mut info).cast(), size);
        (written == size).then_some(info.pti_resident_size)
    }
}

/// No source on this platform.
#[cfg(not(any(target_os = "linux", target_os = "macos")))]
pub fn resident_bytes_of(_pid: u32) -> Option<u64> {
    None
}

/// The directory whose entries are this process's open descriptors.
#[cfg(target_os = "linux")]
const FD_DIR: Option<&str> = Some("/proc/self/fd");
#[cfg(target_os = "macos")]
const FD_DIR: Option<&str> = Some("/dev/fd");
#[cfg(not(any(target_os = "linux", target_os = "macos")))]
const FD_DIR: Option<&str> = None;

/// The number of open file descriptors, not counting the one used to list them.
pub fn open_file_descriptors() -> Option<u64> {
    let entries = std::fs::read_dir(FD_DIR?).ok()?;
    let listed = entries.flatten().count() as u64;
    // The listing itself holds one descriptor open, and it shows among the entries.
    Some(listed.saturating_sub(1))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_source_exists_where_the_service_runs() {
        if cfg!(any(target_os = "linux", target_os = "macos")) {
            assert!(cpu_time().is_some());
            assert!(resident_bytes().is_some());
            assert!(open_file_descriptors().is_some());
        }
    }

    #[test]
    fn another_process_has_a_resident_size_while_it_lives() {
        if !cfg!(any(target_os = "linux", target_os = "macos")) {
            return;
        }
        // A child that reads 0 right after `spawn` is no fault of the reading: on Linux its first
        // pages are still in per-CPU counters (see `resident_bytes_of`). So the child
        // first makes its resident size larger than any per-CPU slack, holding 8 MB in a shell
        // variable, and says so; then it waits on its stdin, which stays open until it is killed.
        let mut child = std::process::Command::new("sh")
            .args([
                "-c",
                "x=$(head -c 8000000 /dev/zero | tr '\\0' a); echo ready; read -r _",
            ])
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        let mut ready = String::new();
        std::io::BufRead::read_line(
            &mut std::io::BufReader::new(child.stdout.take().unwrap()),
            &mut ready,
        )
        .unwrap();
        assert_eq!(ready, "ready\n");
        let pid = child.id();
        assert!(resident_bytes_of(pid).is_some_and(|n| n > 0));
        child.kill().unwrap();
        child.wait().unwrap();
        assert_eq!(resident_bytes_of(pid), None, "a reaped process has none");
    }

    #[test]
    fn cpu_time_only_grows() {
        let Some(before) = cpu_time() else { return };
        let mut x = 0u64;
        for i in 0..5_000_000u64 {
            x = x.wrapping_mul(31).wrapping_add(i);
        }
        std::hint::black_box(x);
        let after = cpu_time().unwrap();
        assert!(after.user + after.system >= before.user + before.system);
    }
}
