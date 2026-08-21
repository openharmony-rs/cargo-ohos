//! Rust + cc-rs compiling C++. Exercises `CXX_<triple>`, `TARGET_CXXFLAGS`, `CXXSTDLIB_<triple>`
//! and - on device - which libc++ actually gets loaded.

use std::ffi::CString;
use std::os::raw::{c_char, c_int};
use std::path::Path;

extern "C" {
    fn fixture_count_vowels(text: *const c_char) -> u64;
    fn fixture_throws_and_catches(value: c_int) -> c_int;
}

fn count_vowels(text: &str) -> u64 {
    let text = CString::new(text).expect("no interior nul");
    // SAFETY: the C string is nul-terminated and outlives the call.
    unsafe { fixture_count_vowels(text.as_ptr()) }
}

fn throws_and_catches(value: i32) -> i32 {
    // SAFETY: no shared state.
    unsafe { fixture_throws_and_catches(value) }
}

fn main() {
    println!("vowels: {}", count_vowels("openharmony"));
    println!("caught: {}", throws_and_catches(-1));
}

/// The C++ runtime this process actually mapped, from `/proc/self/maps`.
fn loaded_cxx_runtime() -> Option<String> {
    let maps = std::fs::read_to_string("/proc/self/maps").ok()?;
    maps.lines()
        .filter_map(|line| line.split_whitespace().next_back())
        .find(|path| path.contains("/libc++"))
        .map(str::to_owned)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A binary run through `ohos-test-runner` lives in `/data/local/tmp`, whose loader
    /// namespace can see the platform `libc++.so` in `/system/lib64/chipset-sdk-sp`. An
    /// application cannot: its namespace only holds what its `.hap` bundles plus the NDK
    /// namespace. So the C++ runtime the toolchain reported and the runner sent along has to
    /// be the one that is loaded here, not whatever the device happens to provide - otherwise
    /// this test would pass while the same binary in a `.hap` would not start.
    #[test]
    fn the_bundled_cxx_runtime_is_the_one_that_is_loaded() {
        let loaded = loaded_cxx_runtime().expect("a C++ runtime has to be mapped");
        println!("loaded C++ runtime: {loaded}");

        let sent_along: Vec<std::path::PathBuf> = std::env::var_os("LD_LIBRARY_PATH")
            .map(|paths| std::env::split_paths(&paths).collect())
            .unwrap_or_default();
        if sent_along.is_empty() {
            // Nothing was bundled, so the device's own runtime is the right one.
            return;
        }
        assert!(
            sent_along
                .iter()
                .any(|dir| !dir.as_os_str().is_empty() && Path::new(&loaded).starts_with(dir)),
            "{loaded} was loaded instead of the runtime sent along in {sent_along:?}"
        );
    }

    #[test]
    fn cxx_code_runs() {
        assert_eq!(count_vowels("openharmony"), 4);
        assert_eq!(count_vowels(""), 0);
    }

    #[test]
    fn cxx_exceptions_unwind_on_device() {
        assert_eq!(throws_and_catches(21), 42);
        assert_eq!(throws_and_catches(-1), -1);
    }
}
