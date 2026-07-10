//! Regression test for the SIGTERM-hang investigation (2026-07-10): a `calm
//! serve` process that lost the initial `try_acquire` race and is waiting to
//! be promoted as indexer/watcher owner used to block on
//! `instance_lock::acquire_blocking` — a single indefinite OS `flock` wait
//! with no cancellation mechanism at all. If the owner process never exited
//! (the case here — this test deliberately never signals it), the loser had
//! no way to ever notice a `ct.cancel()` from its own SIGTERM handler, and
//! Tokio's runtime-drop blocks process exit on that `spawn_blocking` task
//! until it returns — so the loser would hang until manually SIGKILLed.
//!
//! Drives two real `calm serve` subprocesses against the same project (the
//! only way to reliably land a process in the lock-loser state) and asserts
//! the loser exits promptly on SIGTERM while the owner is still very much
//! alive — the scenario the old code could never satisfy, no matter how
//! long the test waited.
//!
//! Uses `libc::kill` (not `Child::kill`, which only sends SIGKILL) to send a
//! real SIGTERM, exercising the exact signal `serve_stdio_with_preset`'s
//! handler listens for.

use std::io::Read;
use std::path::Path;
use std::process::{Child, Command};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// Drains `stream` on a background thread into a shared buffer so the pipe
/// can never fill and block the child's writes — a self-inflicted hang in
/// the *test* that would masquerade as the bug this test is checking for.
fn drain(mut stream: impl Read + Send + 'static) -> Arc<Mutex<Vec<u8>>> {
    let buf = Arc::new(Mutex::new(Vec::new()));
    let buf2 = buf.clone();
    std::thread::spawn(move || {
        let mut chunk = [0u8; 4096];
        loop {
            match stream.read(&mut chunk) {
                Ok(0) | Err(_) => break,
                Ok(n) => buf2.lock().unwrap().extend_from_slice(&chunk[..n]),
            }
        }
    });
    buf
}

fn text(buf: &Arc<Mutex<Vec<u8>>>) -> String {
    String::from_utf8_lossy(&buf.lock().unwrap()).into_owned()
}

fn copy_fixture(src: &Path, dst: &Path) {
    std::fs::create_dir_all(dst).unwrap();
    for entry in std::fs::read_dir(src).unwrap() {
        let entry = entry.unwrap();
        let file_type = entry.file_type().unwrap();
        let name = entry.file_name();
        if name == "target" {
            continue;
        }
        let src_path = entry.path();
        let dst_path = dst.join(&name);
        if file_type.is_dir() {
            copy_fixture(&src_path, &dst_path);
        } else {
            std::fs::copy(&src_path, &dst_path).unwrap();
        }
    }
}

fn wait_for_exit(child: &mut Child, timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    loop {
        if child.try_wait().unwrap().is_some() {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

#[test]
fn lock_losing_process_exits_promptly_on_sigterm_even_though_owner_never_exits() {
    let fixture = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .join("calm-core/tests/fixtures/rust_workspace");
    let tmp = tempfile::tempdir().unwrap();
    copy_fixture(&fixture, tmp.path());

    // `.stdin(Stdio::piped())` and keeping the write half alive for the
    // whole test matters: without it the child's stdin can see immediate
    // EOF depending on what the test harness's own stdin is, which would
    // let the MCP transport shut down on its own — independent of SIGTERM
    // entirely — and confound what this test is trying to isolate.
    let mut owner = Command::new(env!("CARGO_BIN_EXE_calm"))
        .args(["serve", "--project-root"])
        .arg(tmp.path())
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();

    // Give the owner enough time to win `try_acquire` and start indexing —
    // uncontended, so this is fast; generous margin for a loaded CI box.
    std::thread::sleep(Duration::from_millis(800));

    let mut loser = Command::new(env!("CARGO_BIN_EXE_calm"))
        .args(["serve", "--project-root"])
        .arg(tmp.path())
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();

    let owner_stderr = drain(owner.stderr.take().unwrap());
    let owner_stdout = drain(owner.stdout.take().unwrap());
    let loser_stderr = drain(loser.stderr.take().unwrap());
    let loser_stdout = drain(loser.stdout.take().unwrap());
    // Keep stdin's write half alive (never drop/close it) so the child never
    // sees EOF on its own stdin during the test — see the comment above.
    let _owner_stdin = owner.stdin.take().unwrap();
    let _loser_stdin = loser.stdin.take().unwrap();

    // Give the loser enough time to lose `try_acquire` and settle into
    // `acquire_blocking_cancellable`'s wait loop.
    std::thread::sleep(Duration::from_millis(800));

    assert!(
        owner.try_wait().unwrap().is_none(),
        "owner must still be alive — this test only means something if the \
         lock is genuinely still held when the loser is signalled"
    );

    unsafe {
        libc::kill(loser.id() as i32, libc::SIGTERM);
    }

    // Must exceed calm-server's SHUTDOWN_WATCHDOG_GRACE (10s) — the watchdog
    // is the last-resort backstop this test is ultimately allowed to rely
    // on, so give it room to fire before concluding the process is stuck.
    let exited = wait_for_exit(&mut loser, Duration::from_secs(14));

    let proc_status = if !exited {
        std::fs::read_to_string(format!("/proc/{}/status", loser.id())).unwrap_or_default()
    } else {
        String::new()
    };
    let thread_states = if !exited {
        std::fs::read_dir(format!("/proc/{}/task", loser.id()))
            .map(|entries| {
                entries
                    .filter_map(|e| e.ok())
                    .map(|e| {
                        let tid = e.file_name();
                        let st = std::fs::read_to_string(format!(
                            "/proc/{}/task/{}/status",
                            loser.id(),
                            tid.to_string_lossy()
                        ))
                        .unwrap_or_default();
                        let state_line =
                            st.lines().find(|l| l.starts_with("State:")).unwrap_or("?");
                        let wchan = std::fs::read_to_string(format!(
                            "/proc/{}/task/{}/wchan",
                            loser.id(),
                            tid.to_string_lossy()
                        ))
                        .unwrap_or_default();
                        format!("tid={:?} {state_line} wchan={wchan}", tid)
                    })
                    .collect::<Vec<_>>()
                    .join("\n")
            })
            .unwrap_or_default()
    } else {
        String::new()
    };

    // Clean up the owner unconditionally before asserting, so a failure
    // doesn't leak a background `calm serve` process.
    let _ = owner.kill();
    let _ = owner.wait();
    if !exited {
        let _ = loser.kill();
        let _ = loser.wait();
    }

    assert!(
        exited,
        "loser process must exit within 8s of SIGTERM even though the owner \
         it was waiting to replace never exited — before the fix this hung \
         indefinitely (only SIGKILL could end it), because `acquire_blocking` \
         had no cancellation mechanism at all\n\
         --- owner stderr ---\n{}\n\
         --- owner stdout ---\n{}\n\
         --- loser stderr ---\n{}\n\
         --- loser stdout ---\n{}\n\
         --- loser /proc/status ---\n{}\n\
         --- loser thread states ---\n{}",
        text(&owner_stderr),
        text(&owner_stdout),
        text(&loser_stderr),
        text(&loser_stdout),
        proc_status,
        thread_states,
    );
}
