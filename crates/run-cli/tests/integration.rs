use std::fs;
use std::path::PathBuf;
use std::process::{Command, Output};

fn run_bin() -> PathBuf {
    // CARGO_BIN_EXE_run is set by cargo when building integration tests
    PathBuf::from(env!("CARGO_BIN_EXE_run"))
}

fn run_with_taskfile(taskfile_content: &str, args: &[&str]) -> Output {
    let dir = tempfile::tempdir().expect("tempdir");
    let tf_path = dir.path().join("Taskfile.yaml");
    fs::write(&tf_path, taskfile_content).expect("write taskfile");

    let mut cmd = Command::new(run_bin());
    cmd.arg("--taskfile").arg(&tf_path).args(args);
    let out = cmd.output().expect("run binary");
    // Keep tempdir alive until output is captured
    drop(dir);
    out
}

fn run_with_syon_taskfile(taskfile_content: &str, args: &[&str]) -> Output {
    let dir = tempfile::tempdir().expect("tempdir");
    let tf_path = dir.path().join("Taskfile.syon");
    fs::write(&tf_path, taskfile_content).expect("write syon taskfile");

    let mut cmd = Command::new(run_bin());
    cmd.arg("--taskfile").arg(&tf_path).args(args);
    let out = cmd.output().expect("run binary");
    drop(dir);
    out
}

// ── test_list_tasks ──────────────────────────────────────────────────────────

#[test]
fn test_list_tasks() {
    let tf = r#"
version: "3"
tasks:
  hello-world:
    desc: Say hello
    cmds:
      - echo hello
  build-thing:
    desc: Build the thing
    cmds:
      - echo building
"#;
    let out = run_with_taskfile(tf, &["list"]);
    let stdout = String::from_utf8_lossy(&out.stdout);
    // ANSI escapes may be present; check plain text content
    assert!(
        stdout.contains("hello-world"),
        "expected 'hello-world' in output:\n{stdout}"
    );
    assert!(
        stdout.contains("build-thing"),
        "expected 'build-thing' in output:\n{stdout}"
    );
    assert!(
        stdout.contains("Say hello"),
        "expected desc in output:\n{stdout}"
    );
}

// ── test_run_simple_task ─────────────────────────────────────────────────────

#[test]
fn test_run_simple_task() {
    let tf = r#"
version: "3"
tasks:
  say-hello:
    desc: Echo hello
    cmds:
      - echo hello
"#;
    let out = run_with_taskfile(tf, &["say-hello"]);
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        combined.contains("hello"),
        "expected 'hello' in output:\n{combined}"
    );
    assert!(out.status.success(), "exit status: {:?}", out.status);
}

// ── test_var_substitution ────────────────────────────────────────────────────

#[test]
fn test_var_substitution() {
    let tf = r#"
version: "3"
tasks:
  greet:
    desc: Greet someone
    cmds:
      - echo Hello {{.NAME}}
"#;
    let out = run_with_taskfile(tf, &["greet", "NAME=world"]);
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        combined.contains("world"),
        "expected 'world' in output:\n{combined}"
    );
    assert!(out.status.success(), "exit status: {:?}", out.status);
}

// ── test_dep_order ───────────────────────────────────────────────────────────

#[test]
fn test_dep_order() {
    // task-b must run before task-a
    let tf = r#"
version: "3"
tasks:
  task-a:
    desc: Task A (depends on B)
    deps:
      - task-b
    cmds:
      - echo ran-a
  task-b:
    desc: Task B
    cmds:
      - echo ran-b
"#;
    let out = run_with_taskfile(tf, &["task-a"]);
    let stderr = String::from_utf8_lossy(&out.stderr);
    let pos_b = stderr.find("ran-b");
    let pos_a = stderr.find("ran-a");
    assert!(pos_b.is_some(), "expected 'ran-b' in stderr:\n{stderr}");
    assert!(pos_a.is_some(), "expected 'ran-a' in stderr:\n{stderr}");
    assert!(
        pos_b < pos_a,
        "expected 'ran-b' to appear before 'ran-a' in stderr:\n{stderr}"
    );
    assert!(out.status.success(), "exit status: {:?}", out.status);
}

// ── test_syon_taskfile ───────────────────────────────────────────────────────

#[test]
fn test_syon_taskfile() {
    let syon = r#"version: 3

tasks:
  hello-syon:
    desc: SYON hello task
    cmds:
      - echo syon-hello
  build-syon:
    desc: SYON build task
    cmds:
      - echo syon-build
"#;
    // list
    let out = run_with_syon_taskfile(syon, &["list"]);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("hello-syon"),
        "expected 'hello-syon' in list output:\n{stdout}"
    );
    assert!(
        stdout.contains("SYON hello task"),
        "expected desc in list output:\n{stdout}"
    );

    // run
    let out2 = run_with_syon_taskfile(syon, &["hello-syon"]);
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&out2.stdout),
        String::from_utf8_lossy(&out2.stderr)
    );
    assert!(
        combined.contains("syon-hello"),
        "expected 'syon-hello' in output:\n{combined}"
    );
    assert!(out2.status.success(), "exit status: {:?}", out2.status);
}

// ── test_cycle_detection ─────────────────────────────────────────────────────

#[test]
fn test_cycle_detection() {
    // A deps B deps A → should fail with an error, not hang
    let tf = r#"
version: "3"
tasks:
  cycle-a:
    deps:
      - cycle-b
    cmds:
      - echo a
  cycle-b:
    deps:
      - cycle-a
    cmds:
      - echo b
"#;
    let out = run_with_taskfile(tf, &["cycle-a"]);
    assert!(
        !out.status.success(),
        "expected non-zero exit for cycle, got success"
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.to_ascii_lowercase().contains("cycle"),
        "expected 'cycle' in error output:\n{stderr}"
    );
}
