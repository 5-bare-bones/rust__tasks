use std::collections::HashMap;
use std::path::PathBuf;
use std::process;

use clap::Parser;
use colored::Colorize;

#[derive(Parser)]
#[command(name = "run", about = "Taskfile runner (run-cli)")]
struct Cli {
    /// Explicit taskfile path (overrides auto-detection)
    #[arg(short = 't', long, value_name = "PATH")]
    taskfile: Option<PathBuf>,

    /// Print task description and commands without executing
    #[arg(long, value_name = "TASK")]
    summary: Option<String>,

    /// Positional args: "list", or "<task> [KEY=VAL ...]"
    #[arg(trailing_var_arg = true)]
    args: Vec<String>,
}

fn main() {
    let cli = Cli::parse();

    let start_dir = cli
        .taskfile
        .as_deref()
        .and_then(|p| p.parent())
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|| std::env::current_dir().expect("cannot read cwd"));

    let (taskfile, tf_path) = match cli.taskfile {
        Some(ref explicit) => {
            let ext = explicit
                .extension()
                .and_then(|e| e.to_str())
                .unwrap_or("");
            let tf = if ext == "syon" {
                syon_integration::load_syon_taskfile(explicit).map_err(|e| e.to_string())
            } else {
                std::fs::read_to_string(explicit)
                    .map_err(|e| e.to_string())
                    .and_then(|s| serde_yaml_load(&s).map_err(|e| e.to_string()))
            };
            match tf {
                Ok(t) => (t, explicit.clone()),
                Err(e) => {
                    eprintln!("{}: {e}", "error".red().bold());
                    process::exit(1);
                }
            }
        }
        None => match taskfile_loader::find_and_load(&start_dir) {
            Ok(pair) => pair,
            Err(e) => {
                eprintln!("{}: {e}", "error".red().bold());
                process::exit(1);
            }
        },
    };

    // Initialize the embedded Python interpreter once at startup.
    // Without `bundled-python` feature this is a no-op (returns None).
    let python_runner = build_python_runner();
    let py_runner_ref = python_runner.as_ref();

    if let Some(ref task_name) = cli.summary {
        print_summary(&taskfile, task_name);
        return;
    }

    let first_arg = cli.args.first().map(String::as_str);

    match first_arg {
        None => {
            if taskfile.tasks.contains_key("default") {
                run_or_exit(&taskfile, "default", &HashMap::new(), py_runner_ref);
            } else {
                print_list(&taskfile);
            }
        }
        Some("list") => {
            print_list(&taskfile);
        }
        Some(task_name) => {
            let extra_vars = parse_vars(&cli.args[1..]);
            run_or_exit(&taskfile, task_name, &extra_vars, py_runner_ref);
        }
    }

    let _ = tf_path;
}

// ── Python runner construction ───────────────────────────────────────────────

#[cfg(not(feature = "bundled-python"))]
fn build_python_runner() -> Option<task_runner::PythonRunner> {
    None
}

/// Initializes the embedded CPython interpreter and returns a `PythonRunner`
/// callback that routes `python3 <script>` commands through it.
///
/// Requires: `cargo build --features bundled-python` with the PyOxidizer
/// toolchain installed. See README for build instructions.
#[cfg(feature = "bundled-python")]
fn build_python_runner() -> Option<task_runner::PythonRunner> {
    let interp = init_python();
    Some(Box::new(move |script: &str, args: &[String]| {
        interp
            .with_gil(|py| run_python_script(py, script, args))
            .map_err(|e| {
                task_runner::RunError::Io(std::io::Error::new(
                    std::io::ErrorKind::Other,
                    e.to_string(),
                ))
            })
    }))
}

#[cfg(feature = "bundled-python")]
fn init_python() -> pyembed::MainPythonInterpreter {
    let config = pyembed::OxidizedPythonInterpreterConfig::default();
    pyembed::MainPythonInterpreter::new(config)
        .expect("embedded Python interpreter failed to initialize")
}

// pyo3 is a transitive dependency through pyembed; its types are only
// referenced here, which is inside the `bundled-python` cfg gate.
#[cfg(feature = "bundled-python")]
fn run_python_script(
    py: pyo3::Python<'_>,
    script: &str,
    args: &[String],
) -> pyo3::PyResult<i32> {
    let sys = py.import("sys")?;
    let argv: Vec<&str> = std::iter::once(script)
        .chain(args.iter().map(String::as_str))
        .collect();
    sys.setattr("argv", argv)?;
    let code = std::fs::read_to_string(script)
        .map_err(|e| pyo3::exceptions::PyIOError::new_err(e.to_string()))?;
    py.run(&code, None, None)?;
    Ok(0)
}

// ── CLI helpers ──────────────────────────────────────────────────────────────

fn serde_yaml_load(s: &str) -> Result<taskfile_schema::Taskfile, serde_yaml::Error> {
    serde_yaml::from_str(s)
}

fn print_list(taskfile: &taskfile_schema::Taskfile) {
    let mut names: Vec<&str> = taskfile.tasks.keys().map(String::as_str).collect();
    names.sort_unstable();
    let max_len = names.iter().map(|n| n.len()).max().unwrap_or(0);

    println!("{}", "Available tasks:".bold());
    for name in names {
        let desc = taskfile.tasks[name].desc.as_deref().unwrap_or("");
        println!("  {:<width$}  {}", name.green(), desc, width = max_len);
    }
}

fn print_summary(taskfile: &taskfile_schema::Taskfile, task_name: &str) {
    let task = match taskfile.tasks.get(task_name) {
        Some(t) => t,
        None => {
            eprintln!("{}: task '{}' not found", "error".red().bold(), task_name);
            process::exit(1);
        }
    };

    println!("{} {}", "task:".bold(), task_name.green().bold());
    if let Some(ref desc) = task.desc {
        println!("{} {desc}", "desc:".bold());
    }
    if let Some(ref cmds) = task.cmds {
        println!("{}:", "cmds".bold());
        for cmd in cmds {
            match cmd {
                taskfile_schema::Cmd::Shell(s) => println!("  - {s}"),
                taskfile_schema::Cmd::TaskCall { task, .. } => {
                    println!("  - task: {}", task.cyan())
                }
            }
        }
    }
}

fn parse_vars(args: &[String]) -> HashMap<String, String> {
    let mut map = HashMap::new();
    for arg in args {
        if let Some((k, v)) = arg.split_once('=') {
            map.insert(k.to_string(), v.to_string());
        }
    }
    map
}

fn run_or_exit(
    taskfile: &taskfile_schema::Taskfile,
    task_name: &str,
    vars: &HashMap<String, String>,
    python_runner: Option<&task_runner::PythonRunner>,
) {
    if let Err(e) = task_runner::run_task(taskfile, task_name, vars, python_runner) {
        eprintln!("{}: {e}", "error".red().bold());
        process::exit(1);
    }
}
