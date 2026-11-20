//! Allocator tuning: glibc's mmap threshold, and returning freed pages.
//!
//! Each dictation allocates multi-megabyte transient buffers — the captured
//! 16 kHz PCM (~64 KB per second, held twice: once for transcription, once
//! for the history WAV) plus the transcription engine's per-run mel/FFT
//! scratch (~80 KB per second of audio) — all freed within seconds.
//!
//! glibc's malloc serves allocations above its "mmap threshold" with a
//! private mmap that is returned to the OS on free. But the threshold is
//! *dynamic*: freeing an mmapped block raises it to that block's size (up to
//! 32 MB), so after the first dictation every later large buffer is served
//! from malloc arenas instead. Arena memory freed by the app is cached for
//! reuse, and interleaved small live allocations pin those pages, so the OS
//! never gets them back: RSS grows by roughly the transient-buffer volume of
//! each dictation, is never touched again, and slowly migrates to swap
//! (issue #1792 — measured at ~15 MB retained per 2-minute dictation;
//! pinning the threshold reduced that to ~0.5 MB).
//!
//! `init_allocator` is a no-op off glibc: this failure mode is specific to
//! glibc's dynamic-threshold heuristic. `trim_freed_memory` also works on
//! macOS, whose zones keep freed pages too.

/// Pin glibc's mmap threshold so large transient buffers keep taking the
/// mmap path and are returned to the OS as soon as they are freed.
///
/// Must run before the workload allocates (called at the top of `run()`);
/// the cost is an mmap/munmap round-trip per multi-MB buffer, which is
/// negligible at dictation frequency.
#[cfg(all(target_os = "linux", target_env = "gnu"))]
pub fn init_allocator() {
    // SAFETY: mallopt has no pointer arguments; this process-wide glibc setting is valid before dictation allocates.
    unsafe {
        libc::mallopt(libc::M_MMAP_THRESHOLD, 128 * 1024);
    }
}

#[cfg(not(all(target_os = "linux", target_env = "gnu")))]
pub fn init_allocator() {}

/// Return freed-but-cached malloc memory to the OS.
///
/// Called once per finished transcription pipeline (see FinishGuard) and
/// after a model loads, unloads or drops its run buffers. On Linux it sweeps
/// whatever smaller-than-threshold churn still accumulates in the arenas. On
/// macOS the default zones keep freed pages dirty: after loading
/// whisper-small.en and transcribing once, 70 MB of the engine's footprint
/// was freed memory still held by malloc, and it stayed after the model
/// unloaded. Takes on the order of a millisecond, off the main thread.
#[cfg(all(target_os = "linux", target_env = "gnu"))]
pub fn trim_freed_memory() {
    // SAFETY: malloc_trim has no pointer arguments and is thread-safe; it returns only whole free pages to the OS.
    unsafe {
        libc::malloc_trim(0);
    }
}

#[cfg(target_os = "macos")]
pub fn trim_freed_memory() {
    extern "C" {
        fn malloc_zone_pressure_relief(zone: *mut libc::c_void, goal: libc::size_t)
            -> libc::size_t;
    }
    // SAFETY: a null zone releases free pages across all zones with goal 0; thread-safe and dereferences nothing.
    unsafe {
        malloc_zone_pressure_relief(std::ptr::null_mut(), 0);
    }
}

#[cfg(not(any(all(target_os = "linux", target_env = "gnu"), target_os = "macos")))]
pub fn trim_freed_memory() {}
