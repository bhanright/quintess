//! Runs each input on a thread of its own with a large stack.
//!
//! libFuzzer calls targets on its own main thread, where Rust has no
//! stack-overflow handler: an overflow is a bare SIGSEGV that kills the
//! process before libFuzzer can save the input (this happened to
//! save_round_trip). On a thread Rust spawns, an overflow is reported and
//! aborts, which libFuzzer catches and saves. The larger stack also leaves
//! room for the instrumentation, which makes every frame bigger, so only
//! recursion that is truly unbounded is reported.

const STACK_BYTES: usize = 64 * 1024 * 1024;

pub fn run_with_large_stack(data: &[u8], check: fn(&[u8])) {
    let input = data.to_vec();
    let worker = std::thread::Builder::new().stack_size(STACK_BYTES).spawn(move || check(&input)).expect("could not start the fuzzing thread");
    if let Err(panic) = worker.join() {
        // Carried over to libFuzzer's thread, which treats it as a crash.
        std::panic::resume_unwind(panic);
    }
}
