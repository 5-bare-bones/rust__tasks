# PLAN — `run`: Rust-native taskfile runner with embedded Python

## Goal

Build `run`, a Rust binary that:

1. Reads `Taskfile.yaml` (taskfile.dev v3 format — same schema as the official `task` tool)
2. Also reads `Taskfile.syon` (SYON-based taskfile format — see §4)
3. Embeds a CPython interpreter via PyOxidizer so `python3 script.py` commands work without Python installed on the host
4. Serves as a drop-in for the official `task` CLI for the supported feature subset

**Relationship to `5bb_task`:** `5bb_task` (`github.com/5-bare-bones/5bb_task`) is a sibling project with a working YAML-only taskfile runner. `rust__tasks` adds SYON input support and PyOxidizer-embedded Python. The schema types and execution model in `5bb_task` informed this plan, but `rust__tasks` is a clean workspace — no shared code.

---

## 1. Workspace Layout

```
rust__tasks/
├── Cargo.toml               # workspace root
├── pyoxidizer.bzl           # PyOxidizer Starlark config
├── Taskfile.yml             # dogfood — this repo's own CI tasks
├── PLAN.md
├── README.md
└── crates/
    ├── run-cli/             # `run` binary: entry point, CLI parsing
    ├── taskfile-schema/     # shared serde types for Taskfile v3
    ├── taskfile-loader/     # file I/O, format detection, include resolution
    ├── task-runner/         # dependency DAG, variable substitution, execution
    └── syon-integration/   # thin wrapper around syon-parser git dep
```

### 1.1 `crates/run-cli/`

The `main` function. Owns nothing domain-specific — just wires the other crates together and holds the embedded Python interpreter handle.

**Dependencies:** `clap` (derive), `taskfile-loader`, `task-runner`, `pyembed`, `anyhow`

**Responsibilities:**
- Parse CLI args with clap derive macros
- Auto-detect or accept an explicit taskfile path
- Init the embedded Python interpreter once at startup (zero-cost if no Python cmds run)
- Delegate loading to `taskfile-loader`, execution to `task-runner`
- Exit with the task's exit code

```toml
# crates/run-cli/Cargo.toml (deps excerpt)
[dependencies]
clap            = { version = "4", features = ["derive"] }
taskfile-loader = { path = "../taskfile-loader" }
task-runner     = { path = "../task-runner" }
pyembed         = { version = "0.24", default-features = false, features = ["default-python-config"] }
anyhow          = "1"
```

### 1.2 `crates/taskfile-schema/`

Pure-data crate. No I/O. All types derive `serde::Deserialize`. No logic beyond field defaults.

**Dependencies:** `serde`, `indexmap`

```rust
pub struct Taskfile {
    pub version: String,
    pub tasks:   IndexMap<String, Task>,
    pub vars:    Option<IndexMap<String, VarValue>>,
    pub env:     Option<IndexMap<String, String>>,
    pub includes: Option<IndexMap<String, Include>>,
}

pub struct Task {
    pub desc:         Option<String>,
    pub cmds:         Option<Vec<Cmd>>,
    pub deps:         Option<Vec<Dep>>,
    pub vars:         Option<IndexMap<String, VarValue>>,
    pub env:          Option<IndexMap<String, String>>,
    pub dir:          Option<String>,
    pub silent:       Option<bool>,
    pub ignore_error: Option<bool>,
}

// Cmd is either a plain shell string or a task-call map
pub enum Cmd {
    Shell(String),
    TaskCall(TaskCall),
}

// Dep is either a task name string or a task-call map
pub enum Dep {
    Name(String),
    TaskCall(TaskCall),
}

pub struct TaskCall {
    pub task: String,
    pub vars: Option<IndexMap<String, String>>,
}

// VarValue is either a static string or a { sh: "..." } shell expansion
pub enum VarValue {
    Static(String),
    Shell { sh: String },
}

pub struct Include {
    pub taskfile: String,
    pub dir:      Option<String>,
    pub optional: Option<bool>,
}
```

`Cmd` and `Dep` require custom `Deserialize` impls (tagged union via `untagged` or a visitor) because YAML allows both string and map forms in the same sequence.

### 1.3 `crates/taskfile-loader/`

**Dependencies:** `taskfile-schema`, `syon-integration`, `serde_yaml`, `anyhow`

**Responsibilities:**
- Determine format: if `--taskfile` given, infer from extension (`.syon` → SYON, `.yaml`/`.yml` → YAML); otherwise probe `Taskfile.syon` → `Taskfile.yaml` → `Taskfile.yml`, walking up parent directories until found
- Parse YAML via `serde_yaml::from_str::<Taskfile>` or SYON via `syon_integration::parse_syon_taskfile`
- Resolve `includes` recursively: load the referenced file, prefix all task names with `<namespace>:`, merge into the parent taskfile's task map
- Return a flat, fully-merged `Taskfile`

```rust
pub fn load(explicit_path: Option<&Path>) -> anyhow::Result<taskfile_schema::Taskfile>

fn detect_taskfile(start: &Path) -> anyhow::Result<(PathBuf, Format)>
fn resolve_includes(tf: Taskfile, base_dir: &Path) -> anyhow::Result<Taskfile>

enum Format { Yaml, Syon }
```

Include resolution rules (matching official `task` behavior):
- `namespace:` prefix is the key in the `includes` map
- Included file's tasks are accessible as `namespace:task-name`
- Included file's `includes` are resolved relative to the included file's directory
- `optional: true` means a missing file is silently skipped

### 1.4 `crates/task-runner/`

**Dependencies:** `taskfile-schema`, `petgraph`, `anyhow`, `indexmap`

**Responsibilities:**
- Build a directed dependency graph with `petgraph::graphmap::DiGraphMap`; detect cycles with `petgraph::algo::is_cyclic_directed` and report them as errors before execution starts
- Topological sort deps; run them before the task body (deps run in parallel — spawn threads or `std::thread::scope`)
- Variable substitution: replace `{{.VAR}}` patterns in `cmds`, `dir`, and `env` values
- Shell execution: `std::process::Command` with `sh -c` (Unix) or `cmd /C` (Windows), inheriting the task's `env` overlay and `dir`
- Python interception: if a command string starts with `python3` or `python `, invoke the registered Python closure instead of shelling out

```rust
pub struct Runner {
    taskfile:      Taskfile,
    python_runner: Option<Box<dyn Fn(&str, &[String]) -> anyhow::Result<i32> + Send + Sync>>,
}

impl Runner {
    pub fn new(taskfile: Taskfile) -> Self
    pub fn with_python_runner(
        mut self,
        f: impl Fn(&str, &[String]) -> anyhow::Result<i32> + Send + Sync + 'static,
    ) -> Self
    pub fn run(&self, task_name: &str, cli_vars: &IndexMap<String, String>) -> anyhow::Result<()>
    pub fn list(&self) -> Vec<(&str, Option<&str>)>  // (name, desc)
}
```

**Variable substitution priority** (highest wins):

1. `--var KEY=VAL` CLI overrides (passed in as `cli_vars`)
2. Task-level `vars:`
3. Taskfile-level `vars:`
4. `{{.ENV.KEY}}` — reads the process environment
5. Built-ins: `{{.TASKFILE_DIR}}`, `{{.USER_WORKING_DIR}}`, `{{.TASK}}`

`sh:` vars are expanded lazily by running a subshell and capturing stdout.

**Error propagation:** a non-zero exit code fails the task unless `ignore_error: true`. Failure in a dep aborts the parent task.

### 1.5 `crates/syon-integration/`

Thin adapter. Single public function.

**Dependencies:** `syon-parser` (git dep), `taskfile-schema`, `anyhow`, `serde`

```rust
pub fn parse_syon_taskfile(input: &str) -> anyhow::Result<taskfile_schema::Taskfile>
```

If `syon-parser` exposes a serde `Deserializer`, the impl is a one-liner:
```rust
syon_parser::from_str(input).map_err(Into::into)
```

If it returns a raw value tree, `syon-integration` walks the tree manually and maps it into `taskfile-schema` types. Either way, the rest of the codebase never touches raw SYON values — that isolation is the crate's whole job.

---

## 2. PyOxidizer Integration

### 2.1 Approach

PyOxidizer links CPython and the stdlib directly into the Rust binary as frozen bytecode. We use **Rust-main mode**: Rust is the entry point; Python is an embeddable sub-interpreter accessed through the `pyembed` crate's `MainPythonInterpreter`.

The interpreter is initialized once at process startup (even when no Python tasks run) to avoid per-task init overhead. On processes with no Python tasks this adds ~10 ms startup cost — acceptable.

### 2.2 `pyoxidizer.bzl`

```python
def make_exe(dist):
    policy = dist.make_python_packaging_policy()
    policy.set_resource_handling_mode("in-memory")
    policy.resources_location = "in-memory"

    config = dist.make_python_interpreter_config()
    # Rust controls main; Python is sub-interpreter only
    config.run_mode = "repl"
    config.module_search_paths = ["$ORIGIN"]

    exe = dist.to_python_executable(
        name = "run",
        packaging_policy = policy,
        config = config,
    )
    # No third-party packages bundled for v1
    exe.add_python_resources(exe.pip_install([]))
    return exe

register_target("exe", make_exe)
resolve_targets()
```

### 2.3 Python interpreter init in `run-cli`

```rust
// run-cli/src/main.rs (sketch)
fn main() -> anyhow::Result<()> {
    let args = Cli::parse();
    let taskfile = taskfile_loader::load(args.taskfile.as_deref())?;

    let python_config = pyembed::OxidizedPythonInterpreterConfig::default();
    let interp = pyembed::MainPythonInterpreter::new(python_config)
        .map_err(|e| anyhow::anyhow!("Python init failed: {e:?}"))?;

    let runner = task_runner::Runner::new(taskfile)
        .with_python_runner(move |script, args| {
            interp.with_gil(|py| run_python_script(py, script, args))
        });

    match &args.command {
        Command::List => { /* print list */ }
        Command::Run { task } => runner.run(task, &args.vars)?,
    }
    Ok(())
}

fn run_python_script(py: pyo3::Python, script: &str, args: &[String]) -> anyhow::Result<i32> {
    // set sys.argv, read + exec script, return exit code
    let sys = py.import("sys")?;
    let argv: Vec<&str> = std::iter::once(script)
        .chain(args.iter().map(String::as_str))
        .collect();
    sys.setattr("argv", argv)?;
    let code = std::fs::read_to_string(script)?;
    py.run(&code, None, None)?;
    Ok(0)
}
```

### 2.4 Python command detection in `task-runner`

When executing a `Cmd::Shell(s)`, the runner checks:

```rust
fn is_python_cmd(s: &str) -> bool {
    s.starts_with("python3 ") || s.starts_with("python ")
        || s == "python3" || s == "python"
}

fn split_python_cmd(s: &str) -> (&str, Vec<String>) {
    // strip "python3 " prefix, parse remaining as script + args
}
```

If `is_python_cmd` is true and a `python_runner` is registered, call it. If no `python_runner` is registered (e.g., `cargo build` without PyOxidizer), fall back to shelling out to the system `python3` with a warning.

### 2.5 Build modes

| Command | Python source | Use case |
|---|---|---|
| `cargo build` | System `python3` (fallback) | Development |
| `pyoxidizer build exe` | Embedded CPython 3.12 | Release / distribution |
| `pyoxidizer build exe --release` | Embedded CPython 3.12, optimized | CI artifacts |

The PyOxidizer build is gated behind a `bundled-python` Cargo feature flag so `cargo build` compiles cleanly without PyOxidizer installed.

```toml
# run-cli/Cargo.toml
[features]
default = []
bundled-python = ["pyembed"]

[dependencies]
pyembed = { version = "0.24", optional = true, ... }
```

---

## 3. Feature Parity Table

| Feature | v1 scope | Notes |
|---|---|---|
| `tasks` map | **in scope** | core |
| `cmds` — shell strings | **in scope** | |
| `cmds` — `{task: name}` calls | **in scope** | |
| `deps` — parallel pre-run | **in scope** | parallel via threads |
| `desc` | **in scope** | shown in `run list` |
| `vars` — static values | **in scope** | |
| `vars` — `{sh: ...}` expansion | **in scope** | |
| `env` — task + top-level | **in scope** | |
| `dir` — per-task working dir | **in scope** | |
| `silent` | **in scope** | suppresses echo |
| `ignore_error` | **in scope** | continues on non-zero |
| `includes` — local files | **in scope** | namespaced prefix |
| Python cmds (embedded) | **in scope** | via PyOxidizer |
| `--taskfile` flag | **in scope** | explicit path |
| `--var KEY=VAL` flag | **in scope** | CLI var override |
| `--summary` | **in scope** | print descs and exit |
| `Taskfile.syon` input | **in scope** | via syon-integration |
| `watch` / `--watch` | deferred | file-watcher mode |
| `fingerprint` / `sources` | deferred | change detection |
| Remote taskfiles (http/https) | deferred | |
| `prompt` | deferred | interactive confirmation |
| `for:` loops | deferred | |
| `preconditions` | deferred | |
| `status` | deferred | up-to-date checks |
| `generates` | deferred | output file tracking |
| `method: checksum` | deferred | |
| `.env` file loading | deferred | |
| `--parallel` / `-p` flag | deferred | explicit parallelism |
| `--dry` flag | deferred | |
| `--force` flag | deferred | |

---

## 4. `Taskfile.syon` Specification

### 4.1 Overview

`Taskfile.syon` is a SYON document that encodes the same semantic model as `Taskfile.yaml`. The file extension `.syon` tells `run` to parse it with `syon-parser` instead of `serde_yaml`. All supported keys, value types, and interpolation rules are identical to `Taskfile.yaml`.

### 4.2 Top-level keys

```
version: 3                    # required; must be 3 or "3"

vars:                         # optional; global variable definitions
  KEY: static-value
  KEY:
    sh: shell-command-stdout  # dynamic var — block form only, no inline maps

env:                          # optional; global environment additions
  VAR_NAME: value

includes:                     # optional; additional taskfiles to merge
  namespace-key:
    taskfile: ./path/to/Other.syon
    dir: ./path/to             # optional; base dir for the included file
    optional: true             # optional; default false

tasks:                        # required; task definitions
  task-name:
    ...
```

### 4.3 Task keys

```
tasks:
  build-parser-crate:
    desc: Human-readable description        # optional string
    silent: false                           # optional bool; suppress command echo
    ignore_error: false                     # optional bool; continue on failure
    dir: ./relative/or/absolute/path        # optional; working directory for cmds
    env:                                    # optional; task-scoped env additions
      RUST_LOG: debug
    vars:                                   # optional; task-scoped var overrides
      TARGET: release
    deps:                                   # optional; tasks to run first (in parallel)
      - other-task-name                     # string form
      - task: other-task-name               # map form
        vars:
          KEY: value
    cmds:                                   # optional; ordered command list
      - cargo build -p syon-parser          # string form — shell command
      - task: other-task-name               # map form — task call
      - task: other-task-name
        vars:
          KEY: value
```

### 4.4 SYON restrictions relative to full YAML

The following YAML features are **not valid** in `.syon` files:

| YAML feature | SYON status | Reason |
|---|---|---|
| Anchors (`&name`) | **not supported** | SYON design: no aliasing |
| Aliases (`*name`) | **not supported** | SYON design: no aliasing |
| Flow mappings (`{k: v}`) | **not supported** | SYON design: block-only |
| Flow sequences (`[a, b]`) | **not supported** | SYON design: block-only |
| Multi-document (`---` separator) | **not supported** | single-document only |
| Tags (`!!str`, `!custom`) | **not supported** | |
| Merge keys (`<<: *base`) | **not supported** | no anchors → no merging |

Block scalars (`|` literal, `>` folded) **are** supported.

String quoting rules follow SYON: quote only when the value would otherwise be misread (leading `{`, contains `: `, etc.).

### 4.5 Variable interpolation

Same syntax as `Taskfile.yaml`: `{{.VAR_NAME}}`. Supported in:
- `cmds` strings
- `dir` value
- `env` values
- `deps` task names (in the map form `vars:` values)

Built-in variables:
- `{{.TASK}}` — current task name
- `{{.TASKFILE_DIR}}` — directory of the taskfile being executed
- `{{.USER_WORKING_DIR}}` — directory from which `run` was invoked

### 4.6 Example `Taskfile.syon`

```syon
version: 3

vars:
  PROFILE: debug

tasks:
  build-parser-crate:
    desc: Build the syon-parser crate
    cmds:
      - cargo build -p syon-parser --profile {{.PROFILE}}

  run-all-tests:
    desc: Run the full test suite
    deps:
      - build-parser-crate
    cmds:
      - python3 scripts/run_tests.py
      - cargo test

  clean-build-artifacts:
    desc: Remove target directory
    cmds:
      - cargo clean
```

---

## 5. CLI Interface

```
run [OPTIONS] [TASK]
run list [OPTIONS]
```

### 5.1 Global flags

| Flag | Short | Default | Description |
|---|---|---|---|
| `--taskfile <PATH>` | `-t` | auto-detect | Explicit taskfile path |
| `--dir <PATH>` | `-d` | cwd | Working directory (changes cwd before loading) |
| `--var KEY=VAL` | | | Set/override a variable (repeatable) |
| `--silent` | `-s` | false | Suppress command echo globally |
| `--verbose` | `-v` | false | Extra diagnostic output |
| `--summary` | | false | Print task descs and exit |
| `--version` | | | Print `run` version and exit |

### 5.2 Commands

**`run list`** (aliases: `run --list`, `run -l`)

Prints all tasks with their `desc`, sorted by name. Tasks with no `desc` are included but show empty description. Format matches the official `task --list` output:

```
build-parser-crate    Build the syon-parser crate
clean-build-artifacts Remove target directory
run-all-tests         Run the full test suite
```

**`run <task-name>`**

Execute the named task (after its `deps`). Exits with the task's exit code.

**`run` (no arguments)**

If a task named `default` exists, run it. Otherwise, behave as `run list`.

### 5.3 Taskfile auto-detection order

1. `--taskfile <path>` if given — use exactly that file
2. `Taskfile.syon` in current directory
3. `Taskfile.yaml` in current directory
4. `Taskfile.yml` in current directory
5. Repeat steps 2–4 in each parent directory, up to filesystem root
6. Error: "no Taskfile found"

---

## 6. SYON Dependency

`crates/syon-integration/Cargo.toml`:

```toml
[package]
name    = "syon-integration"
version = "0.1.0"
edition = "2021"

[dependencies]
syon-parser     = { git = "https://github.com/object-notation-environment/safe-yaml-object-notation" }
taskfile-schema = { path = "../taskfile-schema" }
anyhow          = "1"
serde           = { version = "1", features = ["derive"] }
```

No version pin on `syon-parser` for now — pin to a specific commit once the API stabilises.

The `syon-parser` crate is opaque until inspected; two scenarios:

- **Scenario A** — it exposes a serde `Deserializer`: `syon_integration::parse_syon_taskfile` is `syon_parser::from_str(input)?`
- **Scenario B** — it returns a raw value tree: `syon-integration` walks the tree with a hand-written mapper

Either way, all SYON-specific code stays inside `syon-integration`. The other crates only see `taskfile-schema` types.

---

## 7. Workspace `Cargo.toml`

```toml
[workspace]
members = [
    "crates/run-cli",
    "crates/taskfile-schema",
    "crates/taskfile-loader",
    "crates/task-runner",
    "crates/syon-integration",
]
resolver = "2"

[workspace.dependencies]
anyhow     = "1"
serde      = { version = "1", features = ["derive"] }
indexmap   = { version = "2", features = ["serde"] }
petgraph   = "0.6"
serde_yaml = "0.9"
clap       = { version = "4", features = ["derive"] }
```

Each crate re-exports workspace deps via `{ workspace = true }` to avoid version drift.

---

## 8. Dogfood `Taskfile.yml`

The repo's own automation. Three-word task names, block-style YAML only (matching the 5-bare-bones house style established in `5bb_task`).

```yaml
version: "3"

tasks:
  build-all-crates:
    desc: Build every crate in the workspace
    cmds:
      - cargo build --workspace

  test-all-crates:
    desc: Run tests for all crates
    cmds:
      - cargo test --workspace

  lint-all-crates:
    desc: Run clippy on all crates
    cmds:
      - cargo clippy --workspace -- -D warnings

  format-all-sources:
    desc: Auto-format all Rust source files
    cmds:
      - cargo fmt --all

  check-format-compliance:
    desc: Verify formatting without modifying files
    cmds:
      - cargo fmt --all -- --check

  build-pyoxidizer-release:
    desc: Build the run binary with embedded Python via PyOxidizer
    cmds:
      - pyoxidizer build exe --release

  run-integration-tests:
    desc: Run integration tests against sample taskfiles
    deps:
      - build-all-crates
    cmds:
      - cargo test --package run-cli --test integration

  clean-build-artifacts:
    desc: Remove all build artifacts
    cmds:
      - cargo clean
```

---

## 9. Implementation Sequence

| Step | Crate / area | Deliverable |
|---|---|---|
| 1 | workspace | `Cargo.toml`, empty crate skeletons (`lib.rs` / `main.rs`) |
| 2 | `taskfile-schema` | all types, serde derives, custom `Deserialize` for `Cmd`/`Dep` union |
| 3 | `syon-integration` | inspect `syon-parser` API, implement `parse_syon_taskfile`, unit tests |
| 4 | `taskfile-loader` | format detection, YAML path, SYON path, include resolution, tests |
| 5 | `task-runner` | dependency DAG, cycle detection, topo sort, var substitution, shell exec, tests |
| 6 | `run-cli` | clap CLI, wire crates, `list` + `run` commands, integration tests |
| 7 | PyOxidizer | `pyoxidizer.bzl`, `bundled-python` feature, Python closure in `run-cli`, CI step |
| 8 | dogfood | `Taskfile.yml`, smoke-test all tasks through `run` |

---

## 10. Open Questions and Risks

| Topic | Risk | Mitigation |
|---|---|---|
| `syon-parser` API surface | Unknown until we inspect the crate; may not expose a serde Deserializer | Isolate in `syon-integration`; adapt to whatever API is exposed |
| PyOxidizer CPython version | PyOxidizer pins a specific CPython build; mismatches can cause extension-module issues | Pin CPython 3.12 in `pyoxidizer.bzl`; v1 bundles no C extensions |
| `pyembed` / `pyo3` version alignment | `pyembed` and `pyo3` must be ABI-compatible | Use the versions bundled with a single PyOxidizer release; don't mix |
| `serde_yaml` 0.9 maintenance status | crate is in maintenance mode | Acceptable for v1; migration path to `serde-yaml2` is straightforward |
| Windows support | `sh -c` unavailable; `python3` naming differs | Detect OS in `task-runner`; use `cmd /C` on Windows; PyOxidizer has Windows support |
| `petgraph` cycle detection | Must run before execution, not during | Call `is_cyclic_directed` on the full graph at load time |
| Parallel dep execution | Data races if tasks share mutable state | Each task is a subprocess; no shared Rust state — safe to thread |
