use std::path::Path;

use indexmap::IndexMap;
use syon_parser::{MappingEntry, Value};
use taskfile_schema::{Cmd, Dep, Include, Task, Taskfile, VarValue};

#[derive(Debug, thiserror::Error)]
pub enum SyonError {
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
    #[error("SYON parse error: {0}")]
    Parse(#[from] syon_parser::SyonError),
    #[error("Invalid structure: {0}")]
    Structure(String),
}

pub fn load_syon_taskfile(path: &Path) -> Result<Taskfile, SyonError> {
    let input = std::fs::read_to_string(path)?;
    let syon_file = syon_parser::parse(&input)?;
    let doc = syon_file
        .documents
        .into_iter()
        .next()
        .ok_or_else(|| SyonError::Structure("SYON file has no documents".into()))?;
    let entries = match doc.body {
        Value::Mapping(m) => m,
        _ => return Err(SyonError::Structure("top-level must be a mapping".into())),
    };
    mapping_to_taskfile(entries)
}

fn mapping_to_taskfile(entries: Vec<MappingEntry>) -> Result<Taskfile, SyonError> {
    let mut version = None;
    let mut tasks = IndexMap::new();
    let mut vars = None;
    let mut includes = None;
    let mut env = None;

    for entry in entries {
        let key = entry.key;
        let value = entry.value;
        match key.as_str() {
            "version" => {
                version = Some(scalar(value, "version")?);
            }
            "tasks" => {
                tasks = match value {
                    Value::Mapping(m) => {
                        let mut map = IndexMap::new();
                        for e in m {
                            let task_key = e.key.clone();
                            let task_entries = match e.value {
                                Value::Mapping(tm) => tm,
                                _ => {
                                    return Err(SyonError::Structure(format!(
                                        "task '{task_key}' must be a mapping"
                                    )))
                                }
                            };
                            map.insert(task_key, mapping_to_task(task_entries)?);
                        }
                        map
                    }
                    _ => return Err(SyonError::Structure("tasks must be a mapping".into())),
                };
            }
            "vars" => {
                vars = Some(mapping_to_var_map(require_mapping(value, "vars")?)?);
            }
            "includes" => {
                includes = Some(match value {
                    Value::Mapping(m) => {
                        let mut map = IndexMap::new();
                        for e in m {
                            let ns = e.key.clone();
                            let inc_entries = require_mapping(e.value, "include")?;
                            map.insert(ns, mapping_to_include(inc_entries)?);
                        }
                        map
                    }
                    _ => return Err(SyonError::Structure("includes must be a mapping".into())),
                });
            }
            "env" => {
                env = Some(mapping_to_string_map(require_mapping(value, "env")?)?);
            }
            _ => {}
        }
    }

    Ok(Taskfile {
        version: version.unwrap_or_else(|| "3".into()),
        tasks,
        vars,
        includes,
        env,
    })
}

fn mapping_to_task(entries: Vec<MappingEntry>) -> Result<Task, SyonError> {
    let mut desc = None;
    let mut cmds = None;
    let mut deps = None;
    let mut vars = None;
    let mut dir = None;
    let mut env = None;
    let mut silent = None;
    let mut ignore_error = None;

    for entry in entries {
        let key = entry.key;
        let value = entry.value;
        match key.as_str() {
            "desc" => desc = Some(scalar(value, "desc")?),
            "dir" => dir = Some(scalar(value, "dir")?),
            "silent" => silent = Some(parse_bool(scalar(value, "silent")?, "silent")?),
            "ignore_error" => {
                ignore_error = Some(parse_bool(scalar(value, "ignore_error")?, "ignore_error")?)
            }
            "cmds" => {
                cmds = Some(match value {
                    Value::Sequence(items) => items
                        .into_iter()
                        .map(|item| value_to_cmd(item.value))
                        .collect::<Result<_, _>>()?,
                    _ => return Err(SyonError::Structure("cmds must be a sequence".into())),
                });
            }
            "deps" => {
                deps = Some(match value {
                    Value::Sequence(items) => items
                        .into_iter()
                        .map(|item| value_to_dep(item.value))
                        .collect::<Result<_, _>>()?,
                    _ => return Err(SyonError::Structure("deps must be a sequence".into())),
                });
            }
            "vars" => {
                vars = Some(mapping_to_var_map(require_mapping(value, "task vars")?)?);
            }
            "env" => {
                env = Some(mapping_to_string_map(require_mapping(value, "task env")?)?);
            }
            _ => {}
        }
    }

    Ok(Task {
        desc,
        cmds,
        deps,
        vars,
        dir,
        env,
        silent,
        ignore_error,
    })
}

fn value_to_cmd(value: Value) -> Result<Cmd, SyonError> {
    match value {
        Value::Scalar(s) | Value::LiteralBlock(s) => Ok(Cmd::Shell(s)),
        Value::Mapping(entries) => {
            let mut task_name = None;
            let mut vars = None;
            for entry in entries {
                let k = entry.key;
                let v = entry.value;
                match k.as_str() {
                    "task" => task_name = Some(scalar(v, "cmd.task")?),
                    "vars" => vars = Some(mapping_to_var_map(require_mapping(v, "cmd.vars")?)?),
                    _ => {}
                }
            }
            Ok(Cmd::TaskCall {
                task: task_name
                    .ok_or_else(|| SyonError::Structure("cmd task-call missing 'task' key".into()))?,
                vars,
            })
        }
        _ => Err(SyonError::Structure(
            "cmd must be a string or mapping".into(),
        )),
    }
}

fn value_to_dep(value: Value) -> Result<Dep, SyonError> {
    match value {
        Value::Scalar(s) => Ok(Dep::Simple(s)),
        Value::Mapping(entries) => {
            let mut task_name = None;
            let mut vars = None;
            for entry in entries {
                let k = entry.key;
                let v = entry.value;
                match k.as_str() {
                    "task" => task_name = Some(scalar(v, "dep.task")?),
                    "vars" => vars = Some(mapping_to_var_map(require_mapping(v, "dep.vars")?)?),
                    _ => {}
                }
            }
            Ok(Dep::Full {
                task: task_name
                    .ok_or_else(|| SyonError::Structure("dep task-call missing 'task' key".into()))?,
                vars,
            })
        }
        _ => Err(SyonError::Structure(
            "dep must be a string or mapping".into(),
        )),
    }
}

fn mapping_to_var_map(
    entries: Vec<MappingEntry>,
) -> Result<IndexMap<String, VarValue>, SyonError> {
    let mut map = IndexMap::new();
    for entry in entries {
        let key = entry.key;
        let value = match entry.value {
            Value::Scalar(s) => VarValue::Scalar(s),
            Value::Mapping(m) => {
                let sh = m
                    .into_iter()
                    .find(|e| e.key == "sh")
                    .and_then(|e| match e.value {
                        Value::Scalar(s) => Some(s),
                        _ => None,
                    })
                    .ok_or_else(|| {
                        SyonError::Structure(format!(
                            "var '{key}' mapping must have a scalar 'sh' key"
                        ))
                    })?;
                VarValue::Shell { sh }
            }
            _ => {
                return Err(SyonError::Structure(format!(
                    "var '{key}' must be a scalar or {{sh: ...}} mapping"
                )))
            }
        };
        map.insert(key, value);
    }
    Ok(map)
}

fn mapping_to_string_map(
    entries: Vec<MappingEntry>,
) -> Result<IndexMap<String, String>, SyonError> {
    let mut map = IndexMap::new();
    for entry in entries {
        let key = entry.key;
        let val = scalar(entry.value, &key)?;
        map.insert(key, val);
    }
    Ok(map)
}

fn mapping_to_include(entries: Vec<MappingEntry>) -> Result<Include, SyonError> {
    let mut taskfile = None;
    let mut dir = None;
    let mut optional = None;
    for entry in entries {
        let k = entry.key;
        let v = entry.value;
        match k.as_str() {
            "taskfile" => taskfile = Some(scalar(v, "include.taskfile")?),
            "dir" => dir = Some(scalar(v, "include.dir")?),
            "optional" => optional = Some(parse_bool(scalar(v, "include.optional")?, "optional")?),
            _ => {}
        }
    }
    Ok(Include {
        taskfile: taskfile
            .ok_or_else(|| SyonError::Structure("include missing 'taskfile' key".into()))?,
        dir,
        optional,
    })
}

fn scalar(value: Value, ctx: &str) -> Result<String, SyonError> {
    match value {
        Value::Scalar(s) | Value::LiteralBlock(s) => Ok(s),
        _ => Err(SyonError::Structure(format!("{ctx} must be a scalar"))),
    }
}

fn require_mapping(value: Value, ctx: &str) -> Result<Vec<MappingEntry>, SyonError> {
    match value {
        Value::Mapping(m) => Ok(m),
        _ => Err(SyonError::Structure(format!("{ctx} must be a mapping"))),
    }
}

fn parse_bool(s: String, ctx: &str) -> Result<bool, SyonError> {
    match s.to_ascii_lowercase().as_str() {
        "true" | "yes" | "1" => Ok(true),
        "false" | "no" | "0" => Ok(false),
        _ => Err(SyonError::Structure(format!(
            "{ctx} must be a boolean (true/false)"
        ))),
    }
}
