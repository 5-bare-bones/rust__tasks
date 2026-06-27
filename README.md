# rust__tasks

`run` — a Rust-native taskfile runner that reads `Taskfile.yaml` / `Taskfile.yml`
(taskfile.dev v3 format) and `Taskfile.syon` (SYON format).

## Build modes

### Standard build (no Python required on host)

```sh
cargo build -p run-cli --release
```

The resulting binary executes shell commands via the system shell. If a task
contains a `python3 script.py` command, `run` falls through to whatever
`python3` is on `PATH` with a warning when no embedded interpreter is
available.

### Fully self-contained build (embedded CPython via PyOxidizer)

```sh
cargo build -p run-cli --release --features bundled-python
```

Requires [PyOxidizer](https://pyoxidizer.readthedocs.io/) to be installed.
The `bundled-python` feature links CPython directly into the binary as frozen
bytecode, so `python3 script.py` task commands work on hosts with no Python
installed. The interpreter is initialised once at startup; tasks that contain
no Python commands pay no extra cost at runtime.

## Usage

```sh
run list                   # list all tasks with descriptions
run <task> [KEY=VALUE...]  # run a task with optional variable overrides
run --taskfile <path>      # use an explicit taskfile
run --summary <task>       # print task description + commands without executing
```

## Taskfile formats

| File | Parser |
|------|--------|
| `Taskfile.syon` | syon-parser (SYON format — no anchors, no flow collections) |
| `Taskfile.yaml` / `Taskfile.yml` | serde_yaml (full YAML v3 schema) |

Auto-detection order: `Taskfile.syon` → `Taskfile.yaml` → `Taskfile.yml`,
walking up parent directories until found.
