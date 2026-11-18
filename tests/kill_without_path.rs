//! A timed-out plugin's process group dies even when no `kill` is on PATH. This binary holds one
//! test because it empties PATH for the whole process.

use rrecall::process::{Ctx, run_hooks};
use rrecall::wire::Event;
use std::os::unix::fs::PermissionsExt;
use std::time::{Duration, Instant};

#[test]
fn a_timeout_kills_the_group_without_kill_on_path() {
    let root = std::env::temp_dir().join(format!("rrecall-kill-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let store = root.join("store");
    let dir = store.join("plugins/gate");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("rrecall-plugin.toml"),
        "id = \"gate\"\nname = \"Gate\"\nversion = \"0.1.0\"\nmin_rrecall = \"0.1.0\"\nkind = \"hook\"\n[hook]\nevents = [\"run.before\"]\ncommand = [\"./gate\"]\n",
    )
    .unwrap();
    // Absolute paths only: the script runs with an empty PATH too.
    std::fs::write(
        dir.join("gate"),
        "#!/bin/sh\n/bin/sleep 30 &\necho $! > \"$RRECALL_STATE_DIR/pid\"\nwait\n",
    )
    .unwrap();
    std::fs::set_permissions(dir.join("gate"), std::fs::Permissions::from_mode(0o755)).unwrap();
    std::fs::write(
        store.join("rrecall.toml"),
        "[[plugins]]\nid = \"gate\"\ntimeout = \"1s\"\n",
    )
    .unwrap();
    let empty = root.join("empty-path");
    std::fs::create_dir_all(&empty).unwrap();
    // SAFETY: the only test in this binary, so no other thread reads the environment.
    unsafe { std::env::set_var("PATH", &empty) };

    let ins = rrecall::plugin::load_with(
        &store,
        &rrecall::config::UserConfig::default(),
        &root.join("user"),
    )
    .unwrap();
    let ctx = Ctx {
        project: "kiln".into(),
        store: store.clone(),
        run_id: "r1".into(),
        run_dir: store.join("runs/r1"),
    };
    let e = run_hooks(&ctx, &ins, Event::RunBefore, &serde_json::json!({})).unwrap_err();
    assert_eq!(rrecall::error::exit_code(&e), 4, "{e:#}");
    assert!(format!("{e:#}").contains("timed out"), "{e:#}");
    assert!(!format!("{e:#}").contains("killing"), "{e:#}");

    let pid: libc::pid_t = std::fs::read_to_string(store.join("state/gate/pid"))
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    // SAFETY: signal 0 only checks that the process exists.
    let alive = || unsafe { libc::kill(pid, 0) } == 0;
    let deadline = Instant::now() + Duration::from_secs(3);
    while alive() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(50));
    }
    let survived = alive();
    if survived {
        // SAFETY: as above; cleans up the child the test failed to see killed.
        unsafe { libc::kill(pid, libc::SIGKILL) };
    }
    assert!(!survived, "the plugin's child {pid} survived the timeout");
    let _ = std::fs::remove_dir_all(&root);
}
