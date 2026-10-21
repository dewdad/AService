fn main() {
    println!("cargo:rerun-if-changed=assets/aservice.ico");

    #[cfg(windows)]
    winres::WindowsResource::new()
        .set_icon("assets/aservice.ico")
        .compile()
        .expect("failed to embed AService application icon");
}
