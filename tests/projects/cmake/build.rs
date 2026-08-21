fn main() {
    println!("cargo::rerun-if-changed=lib");
    let destination = cmake::build("lib");
    println!(
        "cargo::rustc-link-search=native={}",
        destination.join("lib").display()
    );
    println!("cargo::rustc-link-lib=static=fixture");
}
