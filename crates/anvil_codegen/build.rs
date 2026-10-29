// Compiles the C runtime once; the compiler embeds the resulting library and hands it to the linker.
fn main() {
    println!("cargo:rerun-if-changed=runtime/anvil_rt.c");
    cc::Build::new()
        .file("runtime/anvil_rt.c")
        .static_crt(true)
        .opt_level(2)
        .warnings_into_errors(true)
        .cargo_metadata(false)
        .compile("anvil_rt");
}
