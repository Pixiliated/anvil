//! `anvil compare`: which of two programs is faster, on machines where plain timing misleads. Cores
//! differ in speed and other work shares them, so one program can move by 2x between runs, and even
//! taking turns has agreed on a gain that was not there. So each run gets one logical processor of a
//! fast core at high priority, a thread here keeps the core's other logical processor idle, the time
//! is what the system charged the process rather than what passed, and a difference is only called
//! one when the best and the typical run agree on it by more than these runs move on their own.

use crate::golden::{self, Input};
use crate::project::Project;
use crate::{ProgramInput, Report, EXIT_FAILED};
use serde_json::{json, Value};
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// The least a difference must be to count, however steady the runs.
const SMALLEST: f64 = 0.02;
/// A worker that did not finish the rounds asked for in this long has gone wrong.
const DEFAULT_LIMIT: u64 = 600;
const DEFAULT_ROUNDS: u64 = 7;

/// A processor and the other one on its core, if it has one.
#[derive(Clone, Copy)]
struct Place {
    cpu: usize,
    sibling: Option<usize>,
}

#[cfg(windows)]
mod os {
    use super::Place;
    use std::ffi::c_void;
    use std::os::windows::io::AsRawHandle;
    use std::os::windows::process::CommandExt;
    use std::process::{Child, Command};

    /// The smallest time the system charges a process in.
    pub const TICK: f64 = 1.0 / 64.0;
    pub const PRIORITY: &str = "at high priority";
    const HIGH_PRIORITY_CLASS: u32 = 0x80;
    const THREAD_PRIORITY_TIME_CRITICAL: i32 = 15;

    #[repr(C)]
    #[derive(Default)]
    struct FileTime {
        low: u32,
        high: u32,
    }

    #[link(name = "kernel32")]
    extern "system" {
        fn GetLogicalProcessorInformationEx(relationship: i32, buffer: *mut u8, length: *mut u32) -> i32;
        fn SetProcessAffinityMask(process: *mut c_void, mask: usize) -> i32;
        fn SetThreadAffinityMask(thread: *mut c_void, mask: usize) -> usize;
        fn SetThreadPriority(thread: *mut c_void, priority: i32) -> i32;
        fn GetCurrentThread() -> *mut c_void;
        fn GetProcessTimes(
            process: *mut c_void,
            created: *mut FileTime,
            ended: *mut FileTime,
            kernel: *mut FileTime,
            user: *mut FileTime,
        ) -> i32;
    }

    /// The last core of the fastest class, by the processor records the system gives.
    pub fn place() -> Option<Place> {
        let mut length = 0u32;
        // SAFETY: a null buffer with length 0 only asks how long the records are.
        unsafe { GetLogicalProcessorInformationEx(0, std::ptr::null_mut(), &mut length) };
        let mut records = vec![0u8; length as usize];
        // SAFETY: the buffer is as long as the length says.
        if unsafe { GetLogicalProcessorInformationEx(0, records.as_mut_ptr(), &mut length) } == 0 {
            return None;
        }
        let (mut at, mut best): (usize, Option<(u8, usize)>) = (0, None);
        while at + 40 <= records.len() {
            let size = u32::from_le_bytes(records[at + 4..at + 8].try_into().ok()?) as usize;
            let class = records[at + 9];
            let mask = usize::from_le_bytes(records[at + 32..at + 40].try_into().ok()?);
            if mask != 0 && best.is_none_or(|(c, _)| class >= c) {
                best = Some((class, mask));
            }
            at += size.max(1);
        }
        let mask = best?.1;
        let cpu = mask.trailing_zeros() as usize;
        let rest = mask & !(1 << cpu);
        Some(Place { cpu, sibling: (rest != 0).then(|| rest.trailing_zeros() as usize) })
    }

    pub fn high_priority(command: &mut Command) {
        command.creation_flags(HIGH_PRIORITY_CLASS);
    }

    pub fn pin(child: &Child, cpu: usize) -> bool {
        // SAFETY: the handle is the child's, and lives as long as `child`.
        unsafe { SetProcessAffinityMask(child.as_raw_handle(), 1 << cpu) != 0 }
    }

    pub fn keep_this_thread_on(cpu: usize) {
        // SAFETY: the pseudo-handle stands for this thread.
        unsafe {
            SetThreadAffinityMask(GetCurrentThread(), 1 << cpu);
            SetThreadPriority(GetCurrentThread(), THREAD_PRIORITY_TIME_CRITICAL);
        }
    }

    /// How the child ended and the seconds it was charged, if it has ended.
    pub fn ended(child: &mut Child) -> Result<Option<(i32, f64)>, String> {
        let Some(status) = child.try_wait().map_err(|e| format!("cannot wait for a run: {e}"))? else {
            return Ok(None);
        };
        let (mut created, mut stopped, mut kernel, mut user) = Default::default();
        // SAFETY: the handle is the child's and each pointer is to a FileTime.
        let got = unsafe { GetProcessTimes(child.as_raw_handle(), &mut created, &mut stopped, &mut kernel, &mut user) };
        if got == 0 {
            return Err("cannot read the time a run took".to_string());
        }
        let ticks = |t: &FileTime| ((t.high as u64) << 32 | t.low as u64) as f64 / 1e7;
        Ok(Some((status.code().unwrap_or(-1), ticks(&kernel) + ticks(&user))))
    }

    pub fn stop(child: &mut Child) {
        let _ = child.kill();
        let _ = child.wait();
    }
}

#[cfg(not(windows))]
mod os {
    use super::Place;
    use std::process::{Child, Command};

    /// The smallest time the system is sure to charge a process in.
    pub const TICK: f64 = 0.004;
    pub const PRIORITY: &str = "at normal priority";
    const WNOHANG: i32 = 1;

    extern "C" {
        fn wait4(pid: i32, status: *mut i32, options: i32, usage: *mut [i64; 32]) -> i32;
        #[cfg(target_os = "linux")]
        fn sched_setaffinity(pid: i32, size: usize, mask: *const [u64; 16]) -> i32;
    }

    #[cfg(target_os = "linux")]
    fn set(pid: i32, cpu: usize) -> bool {
        let mut mask = [0u64; 16];
        if cpu >= 64 * mask.len() {
            return false;
        }
        mask[cpu / 64] = 1 << (cpu % 64);
        // SAFETY: the mask is as long as the size says.
        unsafe { sched_setaffinity(pid, std::mem::size_of_val(&mask), &mask) == 0 }
    }

    #[cfg(not(target_os = "linux"))]
    fn set(_pid: i32, _cpu: usize) -> bool {
        false
    }

    fn cpu_file(cpu: usize, what: &str) -> Option<String> {
        std::fs::read_to_string(format!("/sys/devices/system/cpu/cpu{cpu}/{what}")).ok()
    }

    /// The numbers in a list like `2-3,6`.
    fn cpu_list(text: &str) -> Vec<usize> {
        let mut out = Vec::new();
        for part in text.trim().split(',') {
            let mut ends = part.split('-').filter_map(|n| n.parse::<usize>().ok());
            if let Some(first) = ends.next() {
                out.extend(first..=ends.next().unwrap_or(first));
            }
        }
        out
    }

    /// The last processor of those that go fastest, and the other one on its core.
    pub fn place() -> Option<Place> {
        let n = std::thread::available_parallelism().ok()?.get();
        let speed = |cpu: usize| {
            cpu_file(cpu, "cpufreq/cpuinfo_max_freq").and_then(|s| s.trim().parse::<u64>().ok()).unwrap_or(0)
        };
        let cpu = (0..n).max_by_key(|c| (speed(*c), *c))?;
        let siblings = cpu_file(cpu, "topology/thread_siblings_list").map_or(Vec::new(), |t| cpu_list(&t));
        Some(Place { cpu, sibling: siblings.into_iter().find(|s| *s != cpu) })
    }

    pub fn high_priority(_command: &mut Command) {}

    pub fn pin(child: &Child, cpu: usize) -> bool {
        set(child.id() as i32, cpu)
    }

    pub fn keep_this_thread_on(cpu: usize) {
        set(0, cpu);
    }

    /// How the child ended and the seconds it was charged, if it has ended.
    pub fn ended(child: &mut Child) -> Result<Option<(i32, f64)>, String> {
        let (mut status, mut usage) = (0i32, [0i64; 32]);
        // SAFETY: both pointers are to values as large as the call writes; 32 words pass any struct rusage.
        let got = unsafe { wait4(child.id() as i32, &mut status, WNOHANG, &mut usage) };
        match got {
            0 => Ok(None),
            g if g < 0 => Err(format!("cannot wait for a run: {}", std::io::Error::last_os_error())),
            _ => {
                let code = if status & 0x7f == 0 { (status >> 8) & 0xff } else { 128 + (status & 0x7f) };
                // user seconds and microseconds, then the system's.
                let seconds = usage[0] as f64 + usage[1] as f64 / 1e6 + usage[2] as f64 + usage[3] as f64 / 1e6;
                Ok(Some((code, seconds)))
            }
        }
    }

    pub fn stop(child: &mut Child) {
        let _ = child.kill();
        let (mut status, mut usage) = (0i32, [0i64; 32]);
        // SAFETY: as in `ended`.
        unsafe { wait4(child.id() as i32, &mut status, 0, &mut usage) };
    }
}

/// Keeps a processor idle for as long as it lives, so no other work slows the core next to it.
struct Idle {
    stop: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Idle {
    fn on(cpu: usize) -> Idle {
        let stop = Arc::new(AtomicBool::new(false));
        let seen = stop.clone();
        let thread = std::thread::spawn(move || {
            os::keep_this_thread_on(cpu);
            while !seen.load(Ordering::Relaxed) {
                std::hint::spin_loop();
            }
        });
        Idle { stop, thread: Some(thread) }
    }
}

impl Drop for Idle {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

/// What a run needs that does not change between rounds.
struct Runs<'a> {
    root: &'a Path,
    input: &'a Input,
    stdin: &'a Path,
    limit: u64,
    place: Option<Place>,
}

impl Runs<'_> {
    /// One run: its exit code and the seconds charged to it, and whether it stayed on the processor.
    fn run(&self, name: &str, exe: &Path, stdout: Stdio) -> Result<(i32, f64, bool), String> {
        let mut command = Command::new(exe);
        command
            .args(&self.input.args)
            .current_dir(self.root)
            .stdin(std::fs::File::open(self.stdin).map_err(|e| format!("cannot open the input: {e}"))?)
            .stdout(stdout)
            .stderr(Stdio::null())
            .envs(self.input.env.iter().map(|(k, v)| (k, v)))
            .env("ANVIL_THREADS", "1");
        os::high_priority(&mut command);
        let mut child = command.spawn().map_err(|e| format!("cannot run {name}: {e}"))?;
        let pinned = self.place.is_some_and(|p| os::pin(&child, p.cpu));
        let started = Instant::now();
        loop {
            if let Some((code, seconds)) = os::ended(&mut child)? {
                return Ok((code, seconds, pinned));
            }
            if started.elapsed() > Duration::from_secs(self.limit) {
                os::stop(&mut child);
                return Err(format!("{name} ran past the limit of {} s; stopped it (--limit sets it)", self.limit));
            }
            std::thread::sleep(Duration::from_millis(2));
        }
    }
}

fn median(seconds: &[f64]) -> f64 {
    let mut s = seconds.to_vec();
    s.sort_by(f64::total_cmp);
    s[s.len() / 2]
}

fn fastest(seconds: &[f64]) -> f64 {
    seconds.iter().copied().fold(f64::INFINITY, f64::min)
}

/// How far these runs move on their own, as a share: the spread each side shows, and how coarsely
/// time is charged against the shorter run.
fn noise_of(a: &[f64], b: &[f64]) -> f64 {
    let spread = |s: &[f64]| (median(s) - fastest(s)) / fastest(s).max(os::TICK);
    let coarse = 2.0 * os::TICK / fastest(a).min(fastest(b)).max(os::TICK);
    SMALLEST.max(spread(a)).max(spread(b)).max(coarse)
}

/// Which side is clearly faster, if the best and the typical run agree by more than `noise`.
fn verdict(a: &[f64], b: &[f64], noise: f64) -> Option<&'static str> {
    let by_best = fastest(a) / fastest(b).max(os::TICK);
    let by_median = median(a) / median(b).max(os::TICK);
    let over = 1.0 + noise;
    if by_best > over && by_median > over {
        Some("b")
    } else if by_best < 1.0 / over && by_median < 1.0 / over {
        Some("a")
    } else {
        None
    }
}

fn side(file: &str, seconds: &[f64], exit: i32) -> Value {
    json!({ "file": file, "min": fastest(seconds), "median": median(seconds), "seconds": seconds, "exit": exit })
}

/// Builds two programs and runs them in turn: which is faster, by how much, and whether that is more
/// than these runs move by on their own.
pub fn compare(root: &Path, a: &str, b: &str, rounds: Option<u64>, limit: Option<u64>, input: &ProgramInput) -> Report {
    let rounds = rounds.unwrap_or(DEFAULT_ROUNDS);
    if rounds < 3 {
        return Report::usage("--rounds must be at least 3: fewer cannot tell a difference from noise");
    }
    let limit = limit.unwrap_or(DEFAULT_LIMIT);
    if limit == 0 {
        return Report::usage("--limit must be at least 1 second");
    }
    let run_input = if input.given() {
        let stdin = match input.file_bytes(root) {
            Ok(b) => b,
            Err(r) => return r,
        };
        Input { args: input.args.clone(), stdin: stdin.unwrap_or_default(), env: Vec::new() }
    } else {
        match Project::discover(root).map(|p| golden::read_input(&p, a)) {
            Ok(Ok(i)) => i,
            Ok(Err(e)) => return Report::error(e),
            Err(_) => Input::default(),
        }
    };
    let mut programs = Vec::new();
    for file in [a, b] {
        match crate::with_main(root, file) {
            Ok((_, program)) => programs.push(program),
            Err(r) => return r,
        }
    }
    let scratch = std::env::temp_dir().join(format!("anvil_compare_{}_{}", std::process::id(), crate::unique()));
    let result = (|| {
        std::fs::create_dir_all(&scratch).map_err(|e| format!("cannot create {}: {e}", scratch.display()))?;
        let mut exes = Vec::new();
        for (i, program) in programs.iter().enumerate() {
            let exe = scratch.join(format!("program{i}{}", std::env::consts::EXE_SUFFIX));
            anvil_codegen::build_exe(program, &exe, &scratch.join(format!("work{i}")), false)?;
            exes.push(exe);
        }
        let stdin = scratch.join("stdin");
        std::fs::write(&stdin, &run_input.stdin).map_err(|e| format!("cannot write the input: {e}"))?;
        let place = os::place();
        let runs = Runs { root, input: &run_input, stdin: &stdin, limit, place };
        let _idle = place.and_then(|p| p.sibling).map(Idle::on);
        // The first run of each is not timed: it reads what files need reading, and shows what each prints.
        let mut printed = Vec::new();
        let mut exits = Vec::new();
        let mut pinned = true;
        for (i, (exe, name)) in exes.iter().zip([a, b]).enumerate() {
            let path = scratch.join(format!("stdout{i}"));
            let file = std::fs::File::create(&path).map_err(|e| format!("cannot write the output: {e}"))?;
            let (code, _, stayed) = runs.run(name, exe, Stdio::from(file))?;
            pinned &= stayed;
            exits.push(code);
            printed.push(std::fs::read(&path).map_err(|e| format!("cannot read the output: {e}"))?);
        }
        let mut seconds = [Vec::new(), Vec::new()];
        for round in 0..rounds as usize {
            // Each goes first as often as the other, as the machine warms and cools between them.
            for i in [round % 2, 1 - round % 2] {
                let name = [a, b][i];
                let (code, time, stayed) = runs.run(name, &exes[i], Stdio::null())?;
                pinned &= stayed;
                if code != exits[i] {
                    return Err(format!("{name} exited {} and then {code}: its runs differ", exits[i]));
                }
                seconds[i].push(time);
            }
        }
        Ok::<_, String>((seconds, exits, printed[0] == printed[1], pinned, place))
    })();
    let _ = std::fs::remove_dir_all(&scratch);
    let (seconds, exits, same_output, pinned, place) = match result {
        Ok(v) => v,
        Err(e) => return Report::failed(json!({ "ok": false, "error": e }), format!("error: {e}\n"), EXIT_FAILED),
    };
    let noise = noise_of(&seconds[0], &seconds[1]);
    let winner = verdict(&seconds[0], &seconds[1], noise);
    let idle = pinned && place.is_some_and(|p| p.sibling.is_some());
    let v = json!({
        "ok": same_output,
        "a": side(a, &seconds[0], exits[0]),
        "b": side(b, &seconds[1], exits[1]),
        "faster": winner,
        "ratio": winner.map(|w| {
            let (fast, slow) = if w == "a" { (&seconds[0], &seconds[1]) } else { (&seconds[1], &seconds[0]) };
            fastest(slow) / fastest(fast).max(os::TICK)
        }),
        "noise": noise,
        "same_output": same_output,
        "rounds": rounds,
        "pinned": pinned,
        "other_thread_idle": idle,
    });
    let mut text = String::new();
    for s in [&v["a"], &v["b"]] {
        text.push_str(&format!(
            "{}: {:.3} s at best, {:.3} s typically\n",
            s["file"].as_str().unwrap_or(""),
            s["min"].as_f64().unwrap_or(0.0),
            s["median"].as_f64().unwrap_or(0.0),
        ));
    }
    let percent = noise * 100.0;
    match winner {
        Some(w) => {
            let (fast, slow) = if w == "a" { (a, b) } else { (b, a) };
            let ratio = v["ratio"].as_f64().unwrap_or(0.0);
            text.push_str(&format!("{fast} is {ratio:.2}x as fast as {slow}; these runs move by {percent:.1}%\n"));
        }
        None => text.push_str(&format!("no clear difference: these runs move by {percent:.1}%\n")),
    }
    if fastest(&seconds[0]).min(fastest(&seconds[1])) < 40.0 * os::TICK {
        text.push_str("a run this short says little: give it a bigger input\n");
    }
    text.push_str(&format!(
        "{rounds} rounds of the time charged to each, {}{}\n",
        if pinned { format!("on one fast processor {}", os::PRIORITY) } else { "NOT kept to one processor, so it may mislead".to_string() },
        if idle { ", its core's other one kept idle" } else { "" },
    ));
    if !same_output || exits[0] != exits[1] {
        text.push_str(&format!(
            "{}: they do not do the same thing (exit codes {} and {})\n",
            if same_output { "their exit codes differ" } else { "their output differs" },
            exits[0],
            exits[1],
        ));
    }
    if same_output && exits[0] == exits[1] {
        Report::ok(v, text)
    } else {
        let mut v = v;
        v["ok"] = json!(false);
        Report::failed(v, text, EXIT_FAILED)
    }
}
