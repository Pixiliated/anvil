//! Turns an object file into a native executable with the platform's linker: MSVC's link.exe on
//! Windows, the C compiler driver (`cc`, or `$CC`) on Linux and macOS. Code is generated for
//! the host; other hosts fail loudly.

use std::path::{Path, PathBuf};
use std::process::Command;

/// The C runtime, compiled by build.rs and embedded so the compiler is one self-contained binary.
#[cfg(windows)]
pub const RUNTIME_LIB: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/anvil_rt.lib"));
#[cfg(not(windows))]
pub const RUNTIME_LIB: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/libanvil_rt.a"));

/// The target triple native code is generated for: the host's.
#[cfg(all(windows, target_arch = "x86_64"))]
pub const TARGET: &str = "x86_64-pc-windows-msvc";
#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
pub const TARGET: &str = "x86_64-unknown-linux-gnu";
#[cfg(all(target_os = "linux", target_arch = "aarch64"))]
pub const TARGET: &str = "aarch64-unknown-linux-gnu";
#[cfg(all(target_os = "macos", target_arch = "x86_64"))]
pub const TARGET: &str = "x86_64-apple-darwin";
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
pub const TARGET: &str = "aarch64-apple-darwin";
#[cfg(not(any(
    all(windows, target_arch = "x86_64"),
    all(any(target_os = "linux", target_os = "macos"), any(target_arch = "x86_64", target_arch = "aarch64"))
)))]
pub const TARGET: &str = "unsupported";

/// Whether this host can build native executables.
pub fn supported() -> Result<(), String> {
    if TARGET == "unsupported" {
        return Err(format!(
            "native builds need x86-64 Windows, or x86-64 or ARM64 Linux or macOS; this host is {} {}",
            std::env::consts::OS,
            std::env::consts::ARCH
        ));
    }
    Ok(())
}

pub fn link_exe(object: &[u8], out: &Path, work_dir: &Path) -> Result<(), String> {
    supported()?;
    std::fs::create_dir_all(work_dir).map_err(|e| format!("cannot create {}: {e}", work_dir.display()))?;
    let (obj_name, rt_name) =
        if cfg!(windows) { ("program.obj", "anvil_rt.lib") } else { ("program.o", "libanvil_rt.a") };
    let obj_path = work_dir.join(obj_name);
    let rt_path = work_dir.join(rt_name);
    std::fs::write(&obj_path, object).map_err(|e| format!("cannot write {}: {e}", obj_path.display()))?;
    std::fs::write(&rt_path, RUNTIME_LIB).map_err(|e| format!("cannot write {}: {e}", rt_path.display()))?;
    let out_abs: PathBuf =
        if out.is_absolute() { out.to_path_buf() } else { std::env::current_dir().unwrap().join(out) };
    let (mut cmd, name) =
        if cfg!(windows) { msvc(&obj_path, &rt_path, &out_abs)? } else { unix(&obj_path, &rt_path, &out_abs) };
    let output = cmd.output().map_err(|e| format!("cannot run {name}: {e}"))?;
    if !output.status.success() {
        return Err(format!(
            "{name} failed ({}):\n{}{}",
            output.status,
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        ));
    }
    Ok(())
}

fn msvc(obj: &Path, rt: &Path, out: &Path) -> Result<(Command, String), String> {
    let mut cmd: Command = cc::windows_registry::find(TARGET, "link.exe")
        .ok_or_else(|| "link.exe not found: install the Visual Studio C++ build tools".to_string())?;
    cmd.arg("/NOLOGO")
        .arg(format!("/OUT:{}", out.display()))
        .arg("/SUBSYSTEM:CONSOLE")
        .arg("/INCREMENTAL:NO")
        .arg("/DEBUG:NONE")
        // Reproducible output: no timestamps, so the same source gives byte-identical executables.
        .arg("/Brepro")
        // Deep recursion: 64 MiB of stack (on Linux and macOS the runtime starts a thread with as much).
        .arg("/STACK:67108864")
        .arg(obj)
        .arg(rt)
        .args(["libcmt.lib", "libucrt.lib", "libvcruntime.lib", "kernel32.lib"]);
    Ok((cmd, "link.exe".into()))
}

fn unix(obj: &Path, rt: &Path, out: &Path) -> (Command, String) {
    let cc = std::env::var("CC").unwrap_or_else(|_| "cc".into());
    let mut cmd = Command::new(&cc);
    cmd.arg("-o").arg(out).arg(obj).arg(rt).args(["-lm", "-lpthread"]);
    (cmd, cc)
}
