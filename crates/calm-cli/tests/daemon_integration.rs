//! Risk-focused integration tests for the daemon (ADR-0005 revival, v1/M5).
//!
//! Unlike `calm-server`'s own `daemon::tests` (which call `bind_or_yield`/
//! `run_accept_loop`/`try_connect_current` directly, in-process), everything
//! here spawns the *real built* `calm` binary as a subprocess — the same
//! tier of test that caught 2 real bugs during M3's manual smoke-testing
//! (a unix socket path-length limit, and `daemon.meta` being written to the
//! wrong directory) that neither `cargo check` nor an in-process unit test
//! could have found, since neither exercises the actual OS-level spawn +
//! bind + connect path. These are the specific risks ADR-0005 and the
//! adversarial design review (session 34c6a934) flagged as the ones that
//! actually matter — not exhaustive coverage, the ones with real failure
//! modes if gotten wrong.
#![cfg(unix)]

use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

fn calm_bin() -> &'static Path {
    Path::new(env!("CARGO_BIN_EXE_calm"))
}

/// A short-lived tempdir git project — `tempfile`'s randomized short names
/// (unlike a hand-rolled `pid_threadid` scheme) keep `<dir>/.calm/daemon.sock`
/// comfortably under a unix socket's `sockaddr_un.sun_path` limit, so these
/// tests exercise the natural socket path rather than incidentally
/// depending on `resolve_socket_path`'s length fallback.
fn fresh_project() -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("creating a tempdir for the test project");
    Command::new("git")
        .arg("init")
        .arg("-q")
        .current_dir(dir.path())
        .status()
        .expect("git init for the test project");
    std::fs::write(dir.path().join("main.rs"), "fn main() {}\n").unwrap();
    dir
}

fn read_daemon_pid(project: &Path) -> Option<u32> {
    let text = std::fs::read_to_string(project.join(".calm").join("daemon.meta")).ok()?;
    let value: serde_json::Value = serde_json::from_str(&text).ok()?;
    value.get("pid")?.as_u64().map(|p| p as u32)
}

fn wait_for(timeout: Duration, cond: impl Fn() -> bool) -> bool {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if cond() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    false
}

fn send_initialize_and_capture(calm_dir_project: &Path) -> std::process::Output {
    let request = br#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2024-11-05","capabilities":{},"clientInfo":{"name":"it","version":"0"}}}
"#;
    let mut child = Command::new(calm_bin())
        .arg("connect")
        .arg("--project-root")
        .arg(calm_dir_project)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawning calm connect");
    {
        // Scoped so the write end drops (closes) before `wait_with_output`
        // blocks. The sleep before that close matters: a real MCP client
        // keeps stdin open for the whole session, but closing immediately
        // after the write here races `relay`'s two directions against each
        // other — `client_to_daemon` can hit EOF (this pipe closing) and
        // trip the "client closed first = normal exit" branch *before*
        // `daemon_to_client` has forwarded the response that's still in
        // flight, producing empty stdout despite a real response having
        // been sent. Hit exactly this race during manual smoke-testing
        // (`printf | calm connect` with no delay) before M3 shipped — same
        // fix here: give the response a real chance to arrive first.
        let mut stdin = child.stdin.take().unwrap();
        stdin.write_all(request).unwrap();
        std::thread::sleep(Duration::from_millis(500));
    }
    child.wait_with_output().expect("calm connect should exit")
}

/// ADR-0005's own self-flagged Risk #1, highest severity: the daemon must
/// detach into its own process group and survive its spawning forwarder's
/// process group being torn down. Get this wrong and a client closing one
/// session kills the daemon and silently takes every other session sharing
/// it down too — reproducing the exact N-process bug this feature exists to
/// fix, under a new name.
#[test]
fn daemon_survives_forwarders_process_group_sigterm() {
    let project = fresh_project();

    let mut connect = {
        use std::os::unix::process::CommandExt;
        // `process_group(0)` here stands in for what a real MCP client's
        // shell/process manager already gives the forwarder for free: its
        // own process group, separate from anything the forwarder itself
        // goes on to spawn.
        Command::new(calm_bin())
            .arg("connect")
            .arg("--project-root")
            .arg(project.path())
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .process_group(0)
            .spawn()
            .expect("spawning calm connect in its own process group")
    };
    let connect_pid = connect.id() as i32;

    assert!(
        wait_for(Duration::from_secs(10), || read_daemon_pid(project.path())
            .is_some()),
        "daemon.meta must appear once calm connect has spawned and bound the daemon"
    );
    let daemon_pid_before = read_daemon_pid(project.path()).unwrap();

    // Kill the forwarder's *entire process group* — a negative pid targets
    // the group, the same signal shape a shell's job control or an MCP
    // client tearing down a session sends. If the daemon didn't truly
    // detach into its own group, this kills it too.
    unsafe {
        libc::kill(-connect_pid, libc::SIGTERM);
    }
    let _ = connect.wait();
    std::thread::sleep(Duration::from_millis(300));

    let output = send_initialize_and_capture(project.path());
    assert!(
        output.status.success(),
        "a follow-up calm connect must still succeed against the surviving daemon: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let response = String::from_utf8_lossy(&output.stdout);
    assert!(
        response.contains("\"protocolVersion\""),
        "must get a real initialize response from the still-live daemon: {response}"
    );

    let daemon_pid_after = read_daemon_pid(project.path())
        .expect("daemon.meta must still be readable — the daemon must not have died");
    assert_eq!(
        daemon_pid_before, daemon_pid_after,
        "the daemon must be the SAME process as before the SIGTERM — a different pid \
         here means it died and got silently respawned, exactly the failure this test \
         exists to catch"
    );

    unsafe {
        libc::kill(daemon_pid_after as i32, libc::SIGTERM);
    }
}

/// `--preset`/`--db-path` on `calm connect` must reach the daemon it spawns
/// (crates/calm-server/src/daemon.rs::spawn_detached_daemon), not just be
/// accepted by the CLI parser and silently dropped. Asserts on the visible
/// effect: `tools/list` through a `--preset orient`-spawned daemon includes
/// an orient-preset tool (`repo_overview`) and excludes an edit-preset-only
/// one (`edit_context`) — if the flag weren't forwarded, the daemon would
/// spawn under the "full" default and both would be present.
#[test]
fn calm_connect_forwards_preset_to_the_daemon_it_spawns() {
    let project = fresh_project();

    let initialize = br#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2024-11-05","capabilities":{},"clientInfo":{"name":"it","version":"0"}}}
"#;
    let list_tools = br#"{"jsonrpc":"2.0","id":2,"method":"tools/list","params":{}}
"#;

    let mut child = Command::new(calm_bin())
        .arg("connect")
        .arg("--project-root")
        .arg(project.path())
        .arg("--preset")
        .arg("orient")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawning calm connect --preset orient");
    {
        let mut stdin = child.stdin.take().unwrap();
        stdin.write_all(initialize).unwrap();
        std::thread::sleep(Duration::from_millis(500));
        stdin.write_all(list_tools).unwrap();
        std::thread::sleep(Duration::from_millis(500));
    }
    let output = child
        .wait_with_output()
        .expect("calm connect --preset orient should exit");
    let stdout = String::from_utf8_lossy(&output.stdout);

    assert!(
        stdout.contains("\"repo_overview\""),
        "orient preset must include repo_overview, got stdout: {stdout}\nstderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        !stdout.contains("\"edit_context\""),
        "orient preset must NOT include edit_context — if it's present, --preset wasn't forwarded to the spawned daemon. stdout: {stdout}"
    );
}

/// Real, live, two-connection version of the `active_sessions`/
/// `other_active_sessions` unit tests in `calm-server`'s own `tools.rs` —
/// those prove the logic in-process; this proves the actual
/// `run_accept_loop` spawn/cleanup path behaves the same way over a real
/// Unix socket with two real subprocesses, the tier of test that's already
/// caught real bugs neither `cargo check` nor an in-process unit test could
/// (see this file's own header comment). Session-awareness work is flagged
/// in the roadmap plan as touching the same concurrency-sensitive area that
/// produced real production bugs before (WAL bloat, SIGTERM hangs,
/// cross-process edit races) — this is the extra rigor that flag asks for.
#[test]
fn session_context_sees_a_second_live_connection_and_stops_seeing_it_after_it_closes() {
    use std::io::{BufRead, BufReader};

    let project = fresh_project();
    let initialize = br#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2024-11-05","capabilities":{},"clientInfo":{"name":"it","version":"0"}}}
"#;
    let list_tools = br#"{"jsonrpc":"2.0","id":1,"method":"tools/list","params":{}}
"#;
    let call_session_context = br#"{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"session_context","arguments":{}}}
"#;

    // Connection A: spawns the daemon (first connect for this project),
    // stays open for the whole test — its stdin is deliberately never
    // closed until the very end, unlike `send_initialize_and_capture`'s
    // one-shot helper, since this test needs it alive while B connects.
    let mut child_a = Command::new(calm_bin())
        .arg("connect")
        .arg("--project-root")
        .arg(project.path())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawning connection A");
    let mut stdin_a = child_a.stdin.take().unwrap();
    let mut stdout_a = BufReader::new(child_a.stdout.take().unwrap());
    stdin_a.write_all(initialize).unwrap();
    let mut init_a = String::new();
    stdout_a
        .read_line(&mut init_a)
        .expect("A: initialize response");
    assert!(
        init_a.contains("\"protocolVersion\""),
        "A: bad initialize response: {init_a}"
    );
    // MCP requires an `initialized` notification before any other request —
    // list_tools doubles as a second no-op-ish call that also confirms A's
    // session is fully live before B connects.
    let initialized = br#"{"jsonrpc":"2.0","method":"notifications/initialized"}
"#;
    stdin_a.write_all(initialized).unwrap();
    stdin_a.write_all(list_tools).unwrap();
    let mut list_a = String::new();
    stdout_a
        .read_line(&mut list_a)
        .expect("A: tools/list response");

    // Connection B: connects to the now-live daemon spawned by A (does not
    // spawn its own).
    let mut child_b = Command::new(calm_bin())
        .arg("connect")
        .arg("--project-root")
        .arg(project.path())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawning connection B");
    let mut stdin_b = child_b.stdin.take().unwrap();
    let mut stdout_b = BufReader::new(child_b.stdout.take().unwrap());
    stdin_b.write_all(initialize).unwrap();
    let mut init_b = String::new();
    stdout_b
        .read_line(&mut init_b)
        .expect("B: initialize response");
    assert!(
        init_b.contains("\"protocolVersion\""),
        "B: bad initialize response: {init_b}"
    );
    stdin_b.write_all(initialized).unwrap();

    // B asks session_context — A must show up in other_active_sessions.
    stdin_b.write_all(call_session_context).unwrap();
    let mut ctx_b_line = String::new();
    stdout_b
        .read_line(&mut ctx_b_line)
        .expect("B: session_context response");
    let ctx_b: serde_json::Value = serde_json::from_str(&ctx_b_line)
        .unwrap_or_else(|e| panic!("B: bad JSON ({e}): {ctx_b_line}"));
    let text = ctx_b["result"]["content"][0]["text"]
        .as_str()
        .unwrap_or_else(|| panic!("B: unexpected tools/call shape: {ctx_b}"));
    let parsed: serde_json::Value = serde_json::from_str(text).unwrap();
    let other = parsed["other_active_sessions"]
        .as_array()
        .unwrap_or_else(|| panic!("B: other_active_sessions missing/not an array: {parsed}"));
    assert_eq!(
        other.len(),
        1,
        "B must see exactly A as another active session: {parsed}"
    );

    // Close A — deregisters its session_id from the shared registry.
    drop(stdin_a);
    let _ = child_a.wait();
    std::thread::sleep(Duration::from_millis(300));

    // B asks again — A must be gone now.
    stdin_b.write_all(call_session_context).unwrap();
    let mut ctx_b_line2 = String::new();
    stdout_b
        .read_line(&mut ctx_b_line2)
        .expect("B: second session_context response");
    let ctx_b2: serde_json::Value = serde_json::from_str(&ctx_b_line2)
        .unwrap_or_else(|e| panic!("B: bad JSON ({e}): {ctx_b_line2}"));
    let text2 = ctx_b2["result"]["content"][0]["text"].as_str().unwrap();
    let parsed2: serde_json::Value = serde_json::from_str(text2).unwrap();
    assert_eq!(
        parsed2["other_active_sessions"],
        serde_json::json!([]),
        "B must no longer see A after A's connection closed: {parsed2}"
    );

    drop(stdin_b);
    let _ = child_b.wait();
}

/// Several `calm serve --listen` candidates racing to spawn against the
/// same fresh socket path — the exact scenario that caused the original
/// N-process bug, now deliberately induced at the daemon layer instead of
/// via N separate MCP client sessions.
#[test]
fn concurrent_serve_listen_candidates_exactly_one_wins() {
    let project = fresh_project();
    let calm_dir = project.path().join(".calm");
    std::fs::create_dir_all(&calm_dir).unwrap();
    let socket_path = calm_dir.join("daemon.sock");

    const N: usize = 5;
    let mut children: Vec<_> = (0..N)
        .map(|_| {
            Command::new(calm_bin())
                .arg("serve")
                .arg("--project-root")
                .arg(project.path())
                .arg("--listen")
                .arg(format!("unix:{}", socket_path.display()))
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .expect("spawning a racing calm serve --listen candidate")
        })
        .collect();

    assert!(
        wait_for(Duration::from_secs(10), || socket_path.exists()),
        "the winning candidate must bind the socket within 10s"
    );
    // Give any losing candidates time to finish their connect-check and
    // exit — `bind_or_yield` makes this cheap (one connect + exit), but
    // still real subprocess teardown, not instantaneous.
    std::thread::sleep(Duration::from_secs(2));

    let mut alive = 0;
    for child in &mut children {
        match child.try_wait().expect("checking a candidate's status") {
            None => alive += 1,
            Some(status) => assert!(
                status.success(),
                "a losing candidate must exit 0 (yielded cleanly), not error: {status:?}"
            ),
        }
    }
    assert_eq!(
        alive, 1,
        "exactly one of {N} racing `calm serve --listen` candidates must still be running \
         — more than one means bind_or_yield's split-brain protection failed; zero means \
         even the winner died"
    );
    assert!(
        socket_path.exists(),
        "the winner must have left its socket file in place, not cleaned up prematurely"
    );

    for mut child in children {
        let _ = child.kill();
        let _ = child.wait();
    }
}

/// A daemon serving a different build than the one `calm connect` is
/// currently running must be respawned, not silently attached to — the
/// whole point of `daemon.meta`'s version handshake. `calm-server`'s own
/// `try_connect_current_...` unit test already proves the detection+signal
/// logic in isolation; this proves the full `calm connect` subcommand
/// actually respawns end-to-end against a real running daemon.
#[test]
fn calm_connect_respawns_a_daemon_running_a_stale_build() {
    let project = fresh_project();

    let mut first = Command::new(calm_bin())
        .arg("connect")
        .arg("--project-root")
        .arg(project.path())
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawning the first calm connect");
    let _ = first.wait();

    assert!(
        wait_for(Duration::from_secs(10), || read_daemon_pid(project.path())
            .is_some()),
        "the first connect must spawn a daemon"
    );
    let original_pid = read_daemon_pid(project.path()).unwrap();

    // Overwrite daemon.meta with a mismatched build, but keep the *real*
    // pid so the mismatch-triggered SIGTERM actually reaches the real
    // daemon — a decoy pid here would make `signal_shutdown` a no-op and
    // the retry loop would just keep finding the same (still genuinely
    // live) daemon forever, never actually exercising a respawn.
    let meta_path = project.path().join(".calm").join("daemon.meta");
    std::fs::write(
        &meta_path,
        format!(
            r#"{{"version":"0.0.0-not-real","build_info":"not-a-real-build","pid":{original_pid}}}"#
        ),
    )
    .unwrap();

    let mut second = Command::new(calm_bin())
        .arg("connect")
        .arg("--project-root")
        .arg(project.path())
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawning the second calm connect");
    let status = second.wait().expect("second calm connect should exit");
    assert!(
        status.success(),
        "calm connect must succeed by respawning, not error out"
    );

    let new_pid =
        read_daemon_pid(project.path()).expect("daemon.meta must exist again after the respawn");
    assert_ne!(
        original_pid, new_pid,
        "a version mismatch must cause a genuine respawn — same pid here means the old \
         (stale) daemon never actually died"
    );

    unsafe {
        libc::kill(new_pid as i32, libc::SIGTERM);
    }
}

/// The daemon socket exposes the full write-capable MCP surface
/// (`edit_lines`/`edit_symbol` write straight into the repo) — on a shared
/// multi-user machine, `.calm/` and the socket itself must not be readable
/// (let alone writable) by anyone else.
#[test]
fn daemon_calm_dir_and_socket_have_restrictive_permissions() {
    let project = fresh_project();

    let mut connect = Command::new(calm_bin())
        .arg("connect")
        .arg("--project-root")
        .arg(project.path())
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawning calm connect");
    let _ = connect.wait();

    assert!(
        wait_for(Duration::from_secs(10), || read_daemon_pid(project.path())
            .is_some()),
        "connect must spawn a daemon"
    );

    let calm_dir = project.path().join(".calm");
    let dir_mode = std::fs::metadata(&calm_dir).unwrap().permissions().mode() & 0o777;
    assert_eq!(dir_mode, 0o700, ".calm/ must be created at exactly 0700");

    let socket_path = calm_dir.join("daemon.sock");
    assert!(
        socket_path.exists(),
        "this test's tempdir-based project path is short, so the natural \
         .calm/daemon.sock path (not the length-fallback) must be in use"
    );
    let socket_mode = std::fs::metadata(&socket_path)
        .unwrap()
        .permissions()
        .mode()
        & 0o777;
    assert_eq!(
        socket_mode, 0o600,
        "the daemon socket must be chmod'd 0600 before any accept()"
    );

    let pid = read_daemon_pid(project.path()).unwrap();
    unsafe {
        libc::kill(pid as i32, libc::SIGTERM);
    }
}
