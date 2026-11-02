fn main() {
    match std::env::var("CARGO_CFG_TARGET_OS").as_deref() {
        Ok("linux") => println!("cargo:rustc-link-arg-bin=steam-helper=-Wl,-rpath,$ORIGIN"),
        Ok("macos") => println!("cargo:rustc-link-arg-bin=steam-helper=-Wl,-rpath,@loader_path"),
        _ => {}
    }
}
