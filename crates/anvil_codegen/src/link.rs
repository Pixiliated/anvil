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

/// `objects` are the compiled C files of extern functions (see `compile_c`). With `map`, the
/// linker also writes the addresses of the executable's symbols there (MSVC only).
pub fn link_exe(
    object: &[u8],
    objects: &[PathBuf],
    out: &Path,
    work_dir: &Path,
    map: Option<&Path>,
) -> Result<(), String> {
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
    cmd.args(objects);
    if let (Some(map), true) = (map, cfg!(windows)) {
        cmd.arg(format!("/MAP:{}", map.display()));
    }
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

/// The executable's symbols, by address: from the linker's map on Windows, from `nm` elsewhere.
pub fn symbols(exe: &Path, map: &Path) -> Result<Vec<(u64, String)>, String> {
    let mut out = Vec::new();
    if cfg!(windows) {
        let text = std::fs::read(map).map_err(|e| format!("cannot read {}: {e}", map.display()))?;
        for line in String::from_utf8_lossy(&text).lines() {
            // ` 0001:00000010  name  0000000140001010 f   program.obj`
            let mut words = line.split_whitespace();
            let (Some(place), Some(name), Some(address)) = (words.next(), words.next(), words.next()) else { continue };
            if !place.contains(':') || address.len() != 16 {
                continue;
            }
            if let Ok(a) = u64::from_str_radix(address, 16) {
                out.push((a, name.to_string()));
            }
        }
    } else {
        let listed = Command::new("nm").arg("-n").arg(exe).output().map_err(|e| format!("cannot run nm: {e}"))?;
        if !listed.status.success() {
            return Err(format!("nm failed on {}: {}", exe.display(), String::from_utf8_lossy(&listed.stderr)));
        }
        for line in String::from_utf8_lossy(&listed.stdout).lines() {
            let words: Vec<&str> = line.split_whitespace().collect();
            if let [address, _, name] = words[..] {
                if let Ok(a) = u64::from_str_radix(address, 16) {
                    out.push((a, name.to_string()));
                }
            }
        }
    }
    out.sort();
    Ok(out)
}

fn msvc(obj: &Path, rt: &Path, out: &Path) -> Result<(Command, String), String> {
    let mut cmd: Command = cc::windows_registry::find(TARGET, "link.exe")
        .ok_or_else(|| "link.exe not found: install the Visual Studio C++ build tools".to_string())?;
    cmd.arg("/NOLOGO")
        .arg(format!("/OUT:{}", out.display()))
        .arg("/SUBSYSTEM:CONSOLE")
        // The runtime's wmain takes the arguments as UTF-16; the linker only looks for main itself.
        .arg("/ENTRY:wmainCRTStartup")
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

/// How C files are compiled, for executables and for the interpreter's library alike, so a C
/// function computes the same thing in both.
#[cfg(windows)]
const C_FLAGS: &[&str] = &["/nologo", "/O2", "/MT", "/W3", "/Brepro"];
#[cfg(not(windows))]
const C_FLAGS: &[&str] = &["-O2", "-fPIC", "-Wall"];

/// The C compiler: MSVC's cl.exe on Windows, `cc` (or `$CC`) elsewhere.
pub(crate) fn c_compiler() -> Result<(Command, String), String> {
    if cfg!(windows) {
        let cmd = cc::windows_registry::find(TARGET, "cl.exe")
            .ok_or_else(|| "cl.exe not found: install the Visual Studio C++ build tools".to_string())?;
        Ok((cmd, "cl.exe".into()))
    } else {
        let cc = std::env::var("CC").unwrap_or_else(|_| "cc".into());
        Ok((Command::new(&cc), cc))
    }
}

/// Writes each C file into `dir` (as c0/name.c, c1/...) and compiles it there to an object;
/// `#include "x.h"` still finds headers next to the original. Compiling in the file's own
/// directory keeps every path out of the object, so builds stay byte-identical.
pub fn compile_c(files: &[anvil_front::ir::CFile], dir: &Path) -> Result<Vec<PathBuf>, String> {
    let mut objects = Vec::new();
    for (i, f) in files.iter().enumerate() {
        let sub = dir.join(format!("c{i}"));
        std::fs::create_dir_all(&sub).map_err(|e| format!("cannot create {}: {e}", sub.display()))?;
        let base = Path::new(&f.name).file_name().and_then(|n| n.to_str()).unwrap_or("extern.c").to_string();
        std::fs::write(sub.join(&base), &f.text).map_err(|e| format!("cannot write {base}: {e}"))?;
        let stem = base.strip_suffix(".c").unwrap_or(&base);
        let obj = if cfg!(windows) { format!("{stem}.obj") } else { format!("{stem}.o") };
        let (mut cmd, name) = c_compiler()?;
        cmd.current_dir(&sub).args(C_FLAGS);
        if let Some(parent) = Path::new(&f.name).parent().filter(|p| !p.as_os_str().is_empty()) {
            let inc = std::env::current_dir().unwrap().join(parent);
            cmd.arg(if cfg!(windows) { format!("/I{}", inc.display()) } else { format!("-I{}", inc.display()) });
        }
        if cfg!(windows) {
            cmd.args(["/c", &base, &format!("/Fo{obj}")]);
        } else {
            cmd.args(["-c", &base, "-o", &obj]);
        }
        run_compiler(&mut cmd, &name, &f.name)?;
        objects.push(sub.join(obj));
    }
    Ok(objects)
}

/// Runs a compiler; a failure is an error naming the file with everything the compiler said.
pub(crate) fn run_compiler(cmd: &mut Command, name: &str, file: &str) -> Result<(), String> {
    let output = cmd.output().map_err(|e| format!("cannot run {name}: {e}"))?;
    if !output.status.success() {
        return Err(format!(
            "{name} could not compile {file} ({}):\n{}{}",
            output.status,
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        ));
    }
    Ok(())
}
