//! Rust + cc-rs compiling C. Exercises `CC_<triple>`, `TARGET_CFLAGS` and the sysroot.

use std::os::raw::c_int;

extern "C" {
    fn fixture_page_size() -> u64;
    fn fixture_sum(values: *const u32, len: usize) -> u64;
    fn fixture_pointer_width() -> c_int;
}

fn sum(values: &[u32]) -> u64 {
    // SAFETY: the pointer and length describe `values`, which outlives the call.
    unsafe { fixture_sum(values.as_ptr(), values.len()) }
}

fn main() {
    // SAFETY: no arguments, no shared state.
    println!("page size: {}", unsafe { fixture_page_size() });
    println!("sum: {}", sum(&[1, 2, 3]));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn c_code_runs() {
        assert_eq!(sum(&[1, 2, 3, 4]), 10);
        assert_eq!(sum(&[]), 0);
    }

    #[test]
    fn the_c_and_rust_halves_agree_about_the_target() {
        // SAFETY: no arguments, no shared state.
        let width = unsafe { fixture_pointer_width() };
        assert_eq!(width as usize, usize::BITS as usize);
    }

    #[test]
    fn libc_calls_work_on_device() {
        // SAFETY: no arguments, no shared state.
        let page_size = unsafe { fixture_page_size() };
        assert!(page_size >= 4096, "implausible page size {page_size}");
        assert!(page_size.is_power_of_two());
    }
}
