//! Rust only: no build script, no native code. Checks that the produced binary is a working
//! OpenHarmony binary at all.

fn main() {
    println!("{}", greeting());
}

fn greeting() -> String {
    format!("hello from {}", std::env::consts::ARCH)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn built_for_openharmony() {
        assert!(cfg!(target_env = "ohos"), "not built for the ohos target env");
        assert!(cfg!(target_os = "linux"));
    }

    #[test]
    fn the_standard_library_works_on_device() {
        // A thread and a channel: pthreads and the allocator are wired up.
        let (sender, receiver) = std::sync::mpsc::channel();
        let handle = std::thread::spawn(move || sender.send(greeting()).unwrap());
        let greeting = receiver.recv().unwrap();
        handle.join().unwrap();
        assert!(greeting.starts_with("hello from"));

        // The runner leaves us in a writable working directory on the device.
        let path = std::path::Path::new("rust-only-fixture.txt");
        std::fs::write(path, greeting.as_bytes()).unwrap();
        assert_eq!(std::fs::read_to_string(path).unwrap(), greeting);
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn the_system_clock_is_readable() {
        assert!(std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs()
            > 0);
    }
}
