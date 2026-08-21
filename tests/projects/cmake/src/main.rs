//! Rust + cmake-rs. Exercises `CMAKE`, `CMAKE_TOOLCHAIN_FILE_<triple>` and the toolchain file
//! cargo-ohos generates around the SDK's `ohos.toolchain.cmake`.

extern "C" {
    fn fixture_cmake_answer() -> u64;
    fn fixture_cmake_page_size() -> u64;
}

fn answer() -> u64 {
    // SAFETY: no arguments, no shared state.
    unsafe { fixture_cmake_answer() }
}

fn main() {
    println!("answer: {}", answer());
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_cmake_library_runs() {
        assert_eq!(answer(), 42);
    }

    #[test]
    fn the_cmake_library_uses_the_ohos_sysroot() {
        // SAFETY: no arguments, no shared state.
        let page_size = unsafe { fixture_cmake_page_size() };
        assert!(page_size >= 4096, "implausible page size {page_size}");
    }
}
