//! Turns an object file into a native executable with the platform linker.
//! Only x86_64 Windows (MSVC) is supported in v0.1; other hosts fail loudly.

use std::path::{Path, PathBuf};
use std::process::Command;

/// The C runtime, compiled by build.rs and embedded so the compiler is one self-contained binary.
pub const RUNTIME_LIB: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/anvil_rt.lib"));

pub const TARGET: &str = "x86_64-pc-windows-msvc";

pub fn link_exe(object: &[u8], out: &Path, work_dir: &Path) -> Result<(), String> {
    if !cfg!(all(windows, target_arch = "x86_64")) {
        return Err(format!("anvil build only supports {TARGET} hosts in v0.1"));
    }
    std::fs::create_dir_all(work_dir).map_err(|e| format!("cannot create {}: {e}", work_dir.display()))?;
    let obj_path = work_dir.join("program.obj");
    let rt_path = work_dir.join("anvil_rt.lib");
    std::fs::write(&obj_path, object).map_err(|e| format!("cannot write {}: {e}", obj_path.display()))?;
    std::fs::write(&rt_path, RUNTIME_LIB).map_err(|e| format!("cannot write {}: {e}", rt_path.display()))?;

    let mut cmd: Command = cc::windows_registry::find(TARGET, "link.exe")
        .ok_or_else(|| "link.exe not found: install the Visual Studio C++ build tools".to_string())?;
    let out_abs: PathBuf =
        if out.is_absolute() { out.to_path_buf() } else { std::env::current_dir().unwrap().join(out) };
    cmd.arg("/NOLOGO")
        .arg(format!("/OUT:{}", out_abs.display()))
        .arg("/SUBSYSTEM:CONSOLE")
        .arg("/INCREMENTAL:NO")
        .arg("/DEBUG:NONE")
        // Reproducible output: no timestamps, so the same source gives byte-identical executables.
        .arg("/Brepro")
        // Deep recursion: 64 MiB of stack.
        .arg("/STACK:67108864")
        .arg(&obj_path)
        .arg(&rt_path)
        .args(["libcmt.lib", "libucrt.lib", "libvcruntime.lib", "kernel32.lib"]);
    let output = cmd.output().map_err(|e| format!("cannot run link.exe: {e}"))?;
    if !output.status.success() {
        return Err(format!(
            "link.exe failed ({}):\n{}{}",
            output.status,
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        ));
    }
    Ok(())
}
