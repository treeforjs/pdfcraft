//! Minimal WASIp1 adapter: only bounded byte streams and deterministic clock/random values.
//! There are no inherited files, directories, arguments, environment, sockets or processes.
use std::time::Duration;
use wasmi::{
    Caller, CompilationMode, Config, Engine, Error, Extern, ExternType, Linker, Memory, Module, Store, StoreLimits, StoreLimitsBuilder,
    TypedResumableCall, Val,
};

const MEMORY_BYTES: usize = 128 * 1024 * 1024;
const STDERR_BYTES: usize = 8 * 1024;
const MAX_IOVECS: usize = 64;
const FUEL_SLICE: u64 = 250_000;

pub(crate) struct Kernel {
    engine: Engine,
    module: Module,
    linker: Linker<State>,
}

struct State {
    limits: StoreLimits,
    input: Vec<u8>,
    cursor: usize,
    output: Vec<u8>,
    errors: Vec<u8>,
    output_limit: usize,
    random: u64,
    clock: u64,
    work_left: u64,
    deadline: web_time::Instant,
}

impl Kernel {
    pub(crate) fn new(bytes: &[u8]) -> Result<Self, String> {
        if bytes.len() > 32 * 1024 * 1024 {
            return Err("script kernel is too large; rebuild the sandbox".into());
        }
        let mut config = Config::default();
        config
            .compilation_mode(CompilationMode::Eager)
            .consume_fuel(true)
            .set_max_recursion_depth(16_384)
            .set_max_stack_height(1024 * 1024)
            .set_max_cached_stacks(0);
        let engine = Engine::new(&config);
        let module = Module::new(&engine, bytes).map_err(|e| format!("invalid script kernel: {e}"))?;
        let mut linker = Linker::new(&engine);
        for import in module.imports() {
            let name = import.name();
            if import.module() != "wasi_snapshot_preview1"
                || !matches!(
                    name,
                    "fd_read"
                        | "fd_write"
                        | "fd_close"
                        | "fd_fdstat_get"
                        | "fd_filestat_get"
                        | "fd_prestat_get"
                        | "fd_prestat_dir_name"
                        | "path_open"
                        | "poll_oneoff"
                        | "environ_get"
                        | "environ_sizes_get"
                        | "args_get"
                        | "args_sizes_get"
                        | "clock_time_get"
                        | "random_get"
                        | "sched_yield"
                        | "proc_exit"
                )
            {
                return Err(format!("script kernel requested an unavailable capability: {}/{name}", import.module()));
            }
            let ExternType::Func(ty) = import.ty() else {
                return Err("script kernel may import only the supported WASIp1 functions".into());
            };
            let name = name.to_string();
            linker
                .func_new(import.module(), import.name(), ty.clone(), move |caller, args, results| wasi(&name, caller, args, results))
                .map_err(|e| format!("script kernel import failed: {e}"))?;
        }
        Ok(Self { engine, module, linker })
    }

    pub(crate) fn run(&self, input: Vec<u8>, fuel: u64, output_limit: usize, timeout: Duration) -> Result<Vec<u8>, String> {
        let deadline = web_time::Instant::now().checked_add(timeout.min(Duration::from_secs(2))).ok_or("invalid script deadline")?;
        let limits = StoreLimitsBuilder::new()
            .memory_size(MEMORY_BYTES)
            .memories(1)
            .tables(1)
            .table_elements(16_384)
            .instances(1)
            // Do not turn memory.grow fuel exhaustion into a permanent allocation trap.
            // Refused growth returns -1; any Rust allocator abort stays inside the guest.
            .build();
        let clock = web_time::SystemTime::now().duration_since(web_time::UNIX_EPOCH).unwrap_or_default().as_nanos().min(u128::from(u64::MAX)) as u64;
        let state = State {
            limits,
            input,
            cursor: 0,
            output: Vec::new(),
            errors: Vec::new(),
            output_limit,
            random: 0x4d59_5df4_d0f3_3173,
            clock,
            work_left: fuel,
            deadline,
        };
        let mut store = Store::new(&self.engine, state);
        store.limiter(|s| &mut s.limits);
        grant(&mut store, 0)?;
        let instance = self.linker.instantiate_and_start(&mut store, &self.module).map_err(|e| format!("script kernel could not start: {e}"))?;
        let start = instance.get_typed_func::<(), ()>(&store, "_start").map_err(|e| format!("script kernel has no entry point: {e}"))?;
        let mut call = start.call_resumable(&mut store, ());
        loop {
            check_time(store.data()).map_err(|e| e.to_string())?;
            match call {
                Ok(TypedResumableCall::Finished(())) => break,
                Ok(TypedResumableCall::OutOfFuel(paused)) => {
                    grant(&mut store, paused.required_fuel())?;
                    call = paused.resume(&mut store);
                }
                Ok(TypedResumableCall::HostTrap(stopped)) => {
                    if stopped.host_error().i32_exit_status() == Some(0) {
                        break;
                    }
                    return Err(format!("the script ran too long or exceeded its isolated memory/output limit: {}", stopped.host_error()));
                }
                Err(e) => {
                    // Never return partial effects from a trapped or aborted guest.
                    return Err(format!("the script ran too long or exceeded its isolated memory/output limit: {e}"));
                }
            }
        }
        Ok(std::mem::take(&mut store.data_mut().output))
    }
}

fn check_time(state: &State) -> Result<(), Error> {
    if web_time::Instant::now() >= state.deadline {
        return Err(Error::new("the script ran too long (deadline reached)"));
    }
    Ok(())
}

fn grant(store: &mut Store<State>, required: u64) -> Result<(), String> {
    check_time(store.data()).map_err(|e| e.to_string())?;
    let amount = FUEL_SLICE.max(required).min(store.data().work_left);
    if amount == 0 || amount < required {
        return Err("the script ran too long (instruction budget exhausted)".into());
    }
    store.data_mut().work_left -= amount;
    store.set_fuel(amount).map_err(|e| e.to_string())
}

fn bad() -> Error {
    Error::new("invalid script kernel memory or WASIp1 arguments")
}

fn arg(args: &[Val], index: usize) -> Result<usize, Error> {
    // WASIp1 pointers/counts are unsigned i32 bit patterns, including the high bit.
    args.get(index).and_then(Val::i32).map(|n| n as u32 as usize).ok_or_else(bad)
}

fn memory(caller: &Caller<'_, State>) -> Result<Memory, Error> {
    caller.get_export("memory").and_then(Extern::into_memory).ok_or_else(bad)
}

fn slice(bytes: &[u8], at: usize, len: usize) -> Result<&[u8], Error> {
    bytes.get(at..at.checked_add(len).ok_or_else(bad)?).ok_or_else(bad)
}

fn put(bytes: &mut [u8], at: usize, value: &[u8]) -> Result<(), Error> {
    bytes.get_mut(at..at.checked_add(value.len()).ok_or_else(bad)?).ok_or_else(bad)?.copy_from_slice(value);
    Ok(())
}

fn u32_at(bytes: &[u8], at: usize) -> Result<usize, Error> {
    let raw: [u8; 4] = slice(bytes, at, 4)?.try_into().map_err(|_| bad())?;
    Ok(u32::from_le_bytes(raw) as usize)
}

fn charge(caller: &mut Caller<'_, State>, bytes: usize) -> Result<(), Error> {
    check_time(caller.data())?;
    let cost = u64::try_from(bytes).map_err(|_| bad())?.saturating_add(100);
    // Host byte-copy work and reserved interpreter slices share one aggregate budget.
    caller.data_mut().work_left = caller.data().work_left.checked_sub(cost).ok_or_else(|| Error::new("script host-call work budget exhausted"))?;
    Ok(())
}

fn io(caller: &mut Caller<'_, State>, args: &[Val], write: bool) -> Result<i32, Error> {
    let (fd, iovs, count, result) = (arg(args, 0)?, arg(args, 1)?, arg(args, 2)?, arg(args, 3)?);
    if count > MAX_IOVECS {
        return Err(Error::new("script stream vector budget exhausted"));
    }
    if (write && fd != 1 && fd != 2) || (!write && fd != 0) {
        return Ok(8); // BADF: no other descriptors exist.
    }
    let memory = memory(caller)?;
    let mut done = 0usize;
    for i in 0..count {
        let at = iovs.checked_add(i.checked_mul(8).ok_or_else(bad)?).ok_or_else(bad)?;
        let (ptr, len) = {
            let bytes = memory.data(&*caller);
            (u32_at(bytes, at)?, u32_at(bytes, at.checked_add(4).ok_or_else(bad)?)?)
        };
        if write {
            let cap = if fd == 1 { caller.data().output_limit } else { STDERR_BYTES };
            let used = if fd == 1 { caller.data().output.len() } else { caller.data().errors.len() };
            if len > cap.saturating_sub(used) {
                return Err(Error::new("script output byte budget exhausted"));
            }
            charge(caller, len)?;
            let (bytes, state) = memory.data_and_store_mut(&mut *caller);
            let source = slice(bytes, ptr, len)?;
            let out = if fd == 1 { &mut state.output } else { &mut state.errors };
            let needed = out.len().saturating_add(len);
            if needed > out.capacity() {
                let capacity = out.capacity().max(256).saturating_mul(2).min(cap).max(needed);
                out.try_reserve_exact(capacity.saturating_sub(out.len())).map_err(|_| Error::new("script output allocation refused"))?;
            }
            out.extend_from_slice(source);
            done = done.checked_add(len).ok_or_else(bad)?;
        } else {
            let len = len.min(caller.data().input.len().saturating_sub(caller.data().cursor));
            charge(caller, len)?;
            let (bytes, state) = memory.data_and_store_mut(&mut *caller);
            put(bytes, ptr, slice(&state.input, state.cursor, len)?)?;
            state.cursor += len;
            done = done.checked_add(len).ok_or_else(bad)?;
        }
    }
    memory.write(&mut *caller, result, &u32::try_from(done).map_err(|_| bad())?.to_le_bytes()).map_err(|_| bad())?;
    Ok(0)
}

fn wasi(name: &str, mut caller: Caller<'_, State>, args: &[Val], results: &mut [Val]) -> Result<(), Error> {
    charge(&mut caller, 0)?;
    let errno = match name {
        "proc_exit" => return Err(Error::i32_exit(i32::try_from(arg(args, 0)?).map_err(|_| bad())?)),
        "fd_read" => io(&mut caller, args, false)?,
        "fd_write" => io(&mut caller, args, true)?,
        "fd_close" => {
            if arg(args, 0)? <= 2 {
                0
            } else {
                8
            }
        }
        "fd_prestat_get" | "fd_prestat_dir_name" => 8,
        "path_open" => 76,   // NOTCAPABLE: never open a real host path.
        "poll_oneoff" => 52, // NOSYS: timers and polling are unavailable.
        "sched_yield" | "environ_get" | "args_get" => 0,
        "environ_sizes_get" | "args_sizes_get" => {
            let memory = memory(&caller)?;
            for index in 0..2 {
                memory.write(&mut caller, arg(args, index)?, &0u32.to_le_bytes()).map_err(|_| bad())?;
            }
            0
        }
        "fd_fdstat_get" | "fd_filestat_get" => {
            if arg(args, 0)? > 2 {
                8
            } else {
                let mut data = [0u8; 64];
                let len = if name == "fd_fdstat_get" {
                    data[0] = 2;
                    data[8] = 0x42;
                    24
                } else {
                    data[16] = 2;
                    64
                };
                memory(&caller)?.write(&mut caller, arg(args, 1)?, data.get(..len).ok_or_else(bad)?).map_err(|_| bad())?;
                0
            }
        }
        "clock_time_get" => {
            // Freeze one real time point for this run: date fields remain correct, while
            // scripts cannot use a progressing clock to escape deterministic work metering.
            let clock = caller.data().clock;
            memory(&caller)?.write(&mut caller, arg(args, 2)?, &clock.to_le_bytes()).map_err(|_| bad())?;
            0
        }
        "random_get" => {
            let (ptr, len) = (arg(args, 0)?, arg(args, 1)?);
            if len > 1024 * 1024 {
                return Err(Error::new("script random byte budget exhausted"));
            }
            charge(&mut caller, len)?;
            let memory = memory(&caller)?;
            let (bytes, state) = memory.data_and_store_mut(&mut caller);
            let out = bytes.get_mut(ptr..ptr.checked_add(len).ok_or_else(bad)?).ok_or_else(bad)?;
            // Only interpreter seeding/Math.random: this is deliberately deterministic,
            // never an entropy API for keys, authentication or other host operations.
            for byte in out {
                state.random ^= state.random << 13;
                state.random ^= state.random >> 7;
                state.random ^= state.random << 17;
                *byte = state.random as u8;
            }
            0
        }
        _ => return Err(Error::new("unavailable script capability")),
    };
    *results.first_mut().ok_or_else(bad)? = Val::I32(errno);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn section(module: &mut Vec<u8>, id: u8, bytes: &[u8]) {
        module.push(id);
        assert!(bytes.len() < 128);
        module.push(bytes.len() as u8);
        module.extend_from_slice(bytes);
    }

    /// Synthetic executable code, built in memory: no downloaded or committed Wasm fixture.
    fn fixture(instructions: &[u8]) -> Vec<u8> {
        let mut module = b"\0asm\x01\0\0\0".to_vec();
        section(&mut module, 1, &[1, 0x60, 0, 0]);
        section(&mut module, 3, &[1, 0]);
        section(&mut module, 5, &[1, 0, 1]); // one 64 KiB memory, no declared maximum
        section(&mut module, 7, b"\x02\x06memory\x02\0\x06_start\0\0");
        let mut body = vec![0];
        body.extend_from_slice(instructions);
        body.push(0x0b);
        let mut code = vec![1, body.len() as u8];
        code.extend_from_slice(&body);
        section(&mut module, 10, &code);
        module
    }

    #[test]
    fn actual_memory_and_work_limits_do_not_depend_on_javascript_counters() {
        // memory.grow(3000 pages) must return -1 even though the module declares no max.
        // Trap if it succeeds: this test would fail with the host memory limit removed.
        let memory = fixture(&[0x41, 0xb8, 0x17, 0x40, 0, 0x41, 0x7f, 0x47, 0x04, 0x40, 0, 0x0b]);
        let kernel = Kernel::new(&memory).unwrap();
        assert!(kernel.run(Vec::new(), 1_000_000, 32, Duration::from_secs(1)).is_ok());
        let forever = fixture(&[0x03, 0x40, 0x0c, 0, 0x0b]);
        let kernel = Kernel::new(&forever).unwrap();
        let err = kernel.run(Vec::new(), 1_000, 32, Duration::from_secs(1)).unwrap_err();
        assert!(err.contains("instruction budget"), "{err}");
        let started = web_time::Instant::now();
        let err = kernel.run(Vec::new(), 300_000_000, 32, Duration::from_millis(10)).unwrap_err();
        assert!(err.contains("deadline"), "{err}");
        assert!(started.elapsed() < Duration::from_secs(2), "{:?}", started.elapsed());
        // A refused call never poisons a later independent store.
        let kernel = Kernel::new(&fixture(&[])).unwrap();
        assert!(kernel.run(Vec::new(), 1_000, 32, Duration::from_secs(1)).is_ok());
    }
}
