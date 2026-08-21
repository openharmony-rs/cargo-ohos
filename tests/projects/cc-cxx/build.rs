fn main() {
    println!("cargo::rerun-if-changed=src/native.cpp");
    cc::Build::new()
        .cpp(true)
        .file("src/native.cpp")
        .compile("native");
}
