//! Links SpiderMonkey, built from source, into an executable, together with the C++ runtime.

fn main() {
    let _engine = mozjs::rust::JSEngine::init().expect("could not initialize SpiderMonkey");
}
