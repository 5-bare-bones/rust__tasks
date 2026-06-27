use std::collections::{HashMap, HashSet};
use std::process::Command;
use std::sync::OnceLock;

use petgraph::algo::toposort;
use petgraph::graphmap::DiGraphMap;
use regex::Regex;
use taskfile_schema::{Cmd, Dep, Taskfile, VarValue};

#[derive(Debug, thiserror::Error)]
pub enum RunError {
    #[error("task '{0}' not found")]
    TaskNotFound(String),
    #[error("cycle detected in task dependency graph involving: {0}")]
    CycleDetected(String),
    #[error("command exited with code {0}")]
    CommandFailed(i32),
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
}

/// Callback for routing Python commands to an embedded interpreter.
/// Receives the script path and its arguments; returns the exit code.
pub type PythonRunner = Box<dyn Fn(&str, &[String]) -> Result<i32, RunError>>;

pub fn run_task(
    taskfile: &Taskfile,
    task_name: &str,
    extra_vars: &HashMap<String, String>,
    python_runner: Option<&PythonRunner>,
) -> Result<(), RunError> {
    let order = topo_order(taskfile, task_name)?;
    for name in &order {
        execute_task(taskfile, name, extra_vars, python_runner)?;
    }
    Ok(())
}

fn topo_order<'tf>(
    taskfile: &'tf Taskfile,
    root: &'tf str,
) -> Result<Vec<String>, RunError> {
    let reachable = collect_dep_reachable(taskfile, root)?;

    let mut graph: DiGraphMap<&str, ()> = DiGraphMap::new();
    for name in &reachable {
        graph.add_node(name.as_str());
    }

    for name in &reachable {
        let task = taskfile
            .tasks
            .get(name.as_str())
            .ok_or_else(|| RunError::TaskNotFound(name.clone()))?;
        if let Some(deps) = &task.deps {
            for dep in deps {
                let dep_name = dep_task_name(dep);
                // edge: dep_name → name means dep runs before name
                graph.add_edge(dep_name, name.as_str(), ());
            }
        }
    }

    toposort(&graph, None)
        .map_err(|cycle| RunError::CycleDetected(cycle.node_id().to_string()))
        .map(|order| {
            order
                .into_iter()
                .filter(|&n| reachable.iter().any(|r| r == n))
                .map(str::to_string)
                .collect()
        })
}

fn collect_dep_reachable(taskfile: &Taskfile, root: &str) -> Result<Vec<String>, RunError> {
    let mut visited: HashSet<String> = HashSet::new();
    let mut queue = vec![root.to_string()];
    let mut order = Vec::new();

    while let Some(name) = queue.pop() {
        if visited.contains(&name) {
            continue;
        }
        if !taskfile.tasks.contains_key(&name) {
            return Err(RunError::TaskNotFound(name));
        }
        visited.insert(name.clone());
        order.push(name.clone());

        if let Some(deps) = taskfile.tasks[&name].deps.as_ref() {
            for dep in deps {
                queue.push(dep_task_name(dep).to_string());
            }
        }
    }

    Ok(order)
}

fn dep_task_name(dep: &Dep) -> &str {
    match dep {
        Dep::Simple(s) => s.as_str(),
        Dep::Full { task, .. } => task.as_str(),
    }
}

fn is_python_cmd(s: &str) -> bool {
    s.starts_with("python3 ") || s.starts_with("python ")
        || s == "python3"
        || s == "python"
}

fn split_python_args(cmd: &str) -> (String, Vec<String>) {
    let rest = cmd
        .strip_prefix("python3 ")
        .or_else(|| cmd.strip_prefix("python "))
        .unwrap_or("");
    let mut parts = rest.split_whitespace();
    let script = parts.next().unwrap_or("").to_string();
    let args = parts.map(str::to_string).collect();
    (script, args)
}

fn execute_task(
    taskfile: &Taskfile,
    task_name: &str,
    extra_vars: &HashMap<String, String>,
    python_runner: Option<&PythonRunner>,
) -> Result<(), RunError> {
    let task = taskfile
        .tasks
        .get(task_name)
        .ok_or_else(|| RunError::TaskNotFound(task_name.to_string()))?;

    let mut vars: HashMap<String, String> = HashMap::new();

    vars.insert("TASK".into(), task_name.to_string());
    if let Ok(cwd) = std::env::current_dir() {
        let cwd_str = cwd.display().to_string();
        vars.insert("USER_WORKING_DIR".into(), cwd_str.clone());
        vars.insert("TASKFILE_DIR".into(), cwd_str);
    }

    if let Some(tf_vars) = &taskfile.vars {
        for (k, v) in tf_vars {
            vars.insert(k.clone(), expand_var(v)?);
        }
    }
    if let Some(task_vars) = &task.vars {
        for (k, v) in task_vars {
            vars.insert(k.clone(), expand_var(v)?);
        }
    }
    for (k, v) in extra_vars {
        vars.insert(k.clone(), v.clone());
    }

    let silent = task.silent.unwrap_or(false);
    let ignore_error = task.ignore_error.unwrap_or(false);

    let cmds = match &task.cmds {
        Some(c) => c,
        None => return Ok(()),
    };

    for cmd in cmds {
        match cmd {
            Cmd::Shell(s) => {
                let expanded = substitute_vars(s, &vars);
                if !silent {
                    eprintln!("  > {expanded}");
                }

                // Route Python commands to the embedded interpreter when available.
                if is_python_cmd(&expanded) {
                    if let Some(runner) = python_runner {
                        let (script, args) = split_python_args(&expanded);
                        let code = runner(&script, &args)?;
                        if code != 0 && !ignore_error {
                            return Err(RunError::CommandFailed(code));
                        }
                        continue;
                    }
                    // No embedded runner — fall through to system shell with a note.
                    eprintln!(
                        "note: no embedded Python; running via system shell: {expanded}"
                    );
                }

                let mut builder = shell_command(&expanded);
                if let Some(dir) = &task.dir {
                    builder.current_dir(substitute_vars(dir, &vars));
                }
                if let Some(tf_env) = &taskfile.env {
                    for (k, v) in tf_env {
                        builder.env(k, substitute_vars(v, &vars));
                    }
                }
                if let Some(task_env) = &task.env {
                    for (k, v) in task_env {
                        builder.env(k, substitute_vars(v, &vars));
                    }
                }

                let status = builder.status()?;
                if !status.success() && !ignore_error {
                    return Err(RunError::CommandFailed(status.code().unwrap_or(1)));
                }
            }
            Cmd::TaskCall {
                task: sub_task,
                vars: call_vars,
            } => {
                let mut sub_extra = extra_vars.clone();
                if let Some(cv) = call_vars {
                    for (k, v) in cv {
                        sub_extra.insert(k.clone(), expand_var(v)?);
                    }
                }
                run_task(taskfile, sub_task, &sub_extra, python_runner)?;
            }
        }
    }

    Ok(())
}

fn expand_var(v: &VarValue) -> Result<String, RunError> {
    match v {
        VarValue::Scalar(s) => Ok(s.clone()),
        VarValue::Shell { sh } => {
            let out = Command::new("sh").arg("-c").arg(sh).output()?;
            Ok(String::from_utf8_lossy(&out.stdout)
                .trim_end_matches('\n')
                .to_string())
        }
    }
}

fn shell_command(cmd: &str) -> Command {
    if cfg!(windows) {
        let mut c = Command::new("cmd");
        c.args(["/C", cmd]);
        c
    } else {
        let mut c = Command::new("sh");
        c.args(["-c", cmd]);
        c
    }
}

static VAR_RE: OnceLock<Regex> = OnceLock::new();

fn var_re() -> &'static Regex {
    VAR_RE.get_or_init(|| {
        Regex::new(r"\{\{\s*\.([A-Za-z_][A-Za-z0-9_]*)\s*\}\}").unwrap()
    })
}

fn substitute_vars(s: &str, vars: &HashMap<String, String>) -> String {
    var_re()
        .replace_all(s, |caps: &regex::Captures| {
            let name = &caps[1];
            vars.get(name)
                .cloned()
                .or_else(|| std::env::var(name).ok())
                .unwrap_or_default()
        })
        .into_owned()
}
