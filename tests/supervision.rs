//! Exercises a real headless daemon and real subprocesses in an isolated profile.
//! Nothing is registered at login and no existing process/service is modified.
use aservice::{command, model::ManagedApp, store::Store};
use std::{
    fs,
    process::{Child, Command, Stdio},
    time::{Duration, Instant},
};

struct Daemon(Child);
impl Drop for Daemon {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}
fn daemon(store: &Store) -> Daemon {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_aservice"));
    cmd.arg("--data-dir").arg(&store.root).arg("daemon");
    Daemon(
        command::hidden(&mut cmd)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap(),
    )
}
fn wait_for(store: &Store, check: impl Fn(&aservice::model::SupervisorStatus) -> bool) {
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        if let Ok(s) = store.status()
            && check(&s)
        {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "Deadline exceeded. Status: {:?}",
            store.status()
        );
        std::thread::sleep(Duration::from_millis(50));
    }
}
fn fixture(store: &Store, mode: &str) -> ManagedApp {
    let mut env = std::collections::BTreeMap::new();
    env.insert("ASERVICE_FIXTURE".into(), mode.into());
    env.insert(
        "ASERVICE_MARKER".into(),
        store.path("fixture-marker.json").display().to_string(),
    );
    env.insert(
        "ASERVICE_LITERAL".into(),
        "spaces & $literal ; stay literal".into(),
    );
    ManagedApp {
        name: "Lifecycle fixture".into(),
        program: std::env::current_exe().unwrap(),
        args: vec![
            "--exact".into(),
            "subprocess_fixture".into(),
            "--ignored".into(),
            "--nocapture".into(),
        ],
        env,
        desired_running: true,
        ..Default::default()
    }
}

#[test]
#[ignore = "Helper executed only as a child of lifecycle tests"]
fn subprocess_fixture() {
    let Ok(mode) = std::env::var("ASERVICE_FIXTURE") else {
        return;
    };
    let marker = std::env::var("ASERVICE_MARKER").unwrap();
    let _descendant = if mode == "tree" {
        let mut child = Command::new(std::env::current_exe().unwrap());
        child
            .args(["--exact", "subprocess_fixture", "--ignored", "--nocapture"])
            .env("ASERVICE_FIXTURE", "leaf")
            .env(
                "ASERVICE_MARKER",
                std::path::Path::new(&marker).with_file_name("leaf-marker.json"),
            );
        Some(
            command::hidden(&mut child)
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .unwrap(),
        )
    } else {
        None
    };
    fs::write(marker,serde_json::to_vec(&serde_json::json!({"pid":std::process::id(),"value":std::env::var("ASERVICE_LITERAL").unwrap()})).unwrap()).unwrap();
    println!("fixture started silently");
    match mode.as_str() {
        "clean" => std::process::exit(0),
        "fail" => std::process::exit(7),
        _ => loop {
            std::thread::sleep(Duration::from_millis(100));
        },
    }
}

#[test]
fn start_stop_restart_and_single_supervisor() {
    let temp = tempfile::tempdir().unwrap();
    let store = Store::open(Some(temp.path().into())).unwrap();
    let app = fixture(&store, "wait");
    let id = app.id.clone();
    store
        .update(|c| {
            c.apps.push(app);
            Ok(())
        })
        .unwrap();
    let mut first = daemon(&store);
    wait_for(&store, |s| s.apps.get(&id).is_some_and(|a| a.pid.is_some()));
    let first_pid = store.status().unwrap().apps[&id].pid;
    wait_for(&store, |_| store.path("fixture-marker.json").exists());
    let marker: serde_json::Value =
        serde_json::from_slice(&fs::read(store.path("fixture-marker.json")).unwrap()).unwrap();
    assert_eq!(marker["value"], "spaces & $literal ; stay literal");
    let mut duplicate = daemon(&store);
    assert!(duplicate.0.wait().unwrap().success());
    assert!(first.0.try_wait().unwrap().is_none());
    assert_eq!(first_pid, store.status().unwrap().apps[&id].pid);
    store
        .update(|c| {
            c.apps[0].generation += 1;
            Ok(())
        })
        .unwrap();
    wait_for(&store, |s| {
        s.apps
            .get(&id)
            .is_some_and(|a| a.pid.is_some() && a.pid != first_pid)
    });
    store
        .update(|c| {
            c.apps[0].desired_running = false;
            Ok(())
        })
        .unwrap();
    wait_for(&store, |s| {
        s.apps
            .get(&id)
            .is_some_and(|a| a.state == "Stopped" && a.pid.is_none())
    });
    std::thread::sleep(Duration::from_millis(900));
    assert!(store.status().unwrap().apps[&id].pid.is_none());
}

#[test]
fn clean_exit_does_not_restart_and_window_only_apps_stay_stopped() {
    let temp = tempfile::tempdir().unwrap();
    let store = Store::open(Some(temp.path().into())).unwrap();
    let app = fixture(&store, "clean");
    let id = app.id.clone();
    store
        .update(|c| {
            c.apps.push(app);
            Ok(())
        })
        .unwrap();
    let _daemon = daemon(&store);
    wait_for(&store, |s| {
        s.apps.get(&id).is_some_and(|a| a.state == "Exited")
    });
    assert_eq!(store.status().unwrap().apps[&id].restarts, 0);
    store
        .update(|c| {
            c.apps[0]
                .env
                .insert("ASERVICE_FIXTURE".into(), "wait".into());
            c.apps[0].run_in_background = false;
            c.apps[0].generation += 1;
            Ok(())
        })
        .unwrap();
    wait_for(&store, |s| {
        s.apps.get(&id).is_some_and(|a| a.state == "Stopped")
    });
    let lock = store.lock("gui.lock", true).unwrap();
    wait_for(&store, |s| s.apps.get(&id).is_some_and(|a| a.pid.is_some()));
    drop(lock);
    wait_for(&store, |s| {
        s.apps.get(&id).is_some_and(|a| a.state == "Stopped")
    });
}

#[test]
fn failed_process_backs_off_and_explicit_stop_cancels_retry() {
    let temp = tempfile::tempdir().unwrap();
    let store = Store::open(Some(temp.path().into())).unwrap();
    let app = fixture(&store, "fail");
    let id = app.id.clone();
    store
        .update(|c| {
            c.apps.push(app);
            Ok(())
        })
        .unwrap();
    let _daemon = daemon(&store);
    wait_for(&store, |s| {
        s.apps
            .get(&id)
            .is_some_and(|a| a.state == "Backing off" && a.restarts == 1 && a.next_retry.is_some())
    });
    store
        .update(|c| {
            c.apps[0].desired_running = false;
            Ok(())
        })
        .unwrap();
    wait_for(&store, |s| {
        s.apps.get(&id).is_some_and(|a| a.state == "Stopped")
    });
    std::thread::sleep(Duration::from_secs(3));
    let status = store.status().unwrap();
    assert_eq!(status.apps[&id].restarts, 1);
    assert!(status.apps[&id].next_retry.is_none());
}

#[test]
fn stop_cleans_up_descendants() {
    let temp = tempfile::tempdir().unwrap();
    let store = Store::open(Some(temp.path().into())).unwrap();
    let app = fixture(&store, "tree");
    let id = app.id.clone();
    store
        .update(|c| {
            c.apps.push(app);
            Ok(())
        })
        .unwrap();
    let _daemon = daemon(&store);
    wait_for(&store, |_| store.path("leaf-marker.json").exists());
    let leaf: serde_json::Value =
        serde_json::from_slice(&fs::read(store.path("leaf-marker.json")).unwrap()).unwrap();
    let pid = leaf["pid"].as_u64().unwrap() as u32;
    store
        .update(|c| {
            c.apps[0].desired_running = false;
            Ok(())
        })
        .unwrap();
    wait_for(&store, |s| {
        s.apps.get(&id).is_some_and(|a| a.state == "Stopped")
    });
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let system = sysinfo::System::new_all();
        let alive = system
            .process(sysinfo::Pid::from_u32(pid))
            .is_some_and(|p| {
                !matches!(
                    p.status(),
                    sysinfo::ProcessStatus::Zombie | sysinfo::ProcessStatus::Dead
                )
            });
        if !alive {
            break;
        }
        assert!(Instant::now() < deadline, "A descendant survived Stop");
        std::thread::sleep(Duration::from_millis(50));
    }
}
