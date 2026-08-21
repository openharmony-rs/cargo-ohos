fn main() {
    println!("cargo::rerun-if-changed=src/native.c");
    cc::Build::new().file("src/native.c").compile("native");
}
