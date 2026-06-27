use indexmap::IndexMap;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Taskfile {
    pub version: String,
    #[serde(default)]
    pub tasks: IndexMap<String, Task>,
    pub vars: Option<IndexMap<String, VarValue>>,
    pub includes: Option<IndexMap<String, Include>>,
    pub env: Option<IndexMap<String, String>>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Task {
    pub desc: Option<String>,
    pub cmds: Option<Vec<Cmd>>,
    pub deps: Option<Vec<Dep>>,
    pub vars: Option<IndexMap<String, VarValue>>,
    pub dir: Option<String>,
    pub env: Option<IndexMap<String, String>>,
    pub silent: Option<bool>,
    pub ignore_error: Option<bool>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(untagged)]
pub enum Cmd {
    Shell(String),
    TaskCall {
        task: String,
        vars: Option<IndexMap<String, VarValue>>,
    },
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(untagged)]
pub enum Dep {
    Simple(String),
    Full {
        task: String,
        vars: Option<IndexMap<String, VarValue>>,
    },
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(untagged)]
pub enum VarValue {
    Scalar(String),
    Shell { sh: String },
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Include {
    pub taskfile: String,
    pub dir: Option<String>,
    pub optional: Option<bool>,
}

#[cfg(test)]
mod tests {
    use super::*;

    const MINIMAL_YAML: &str = r#"
version: "3"
tasks:
  hello:
    desc: Say hello
    cmds:
      - echo hello
"#;

    const FULL_YAML: &str = r#"
version: "3"
vars:
  GREETING: hello
  WHO:
    sh: echo world
env:
  LOG_LEVEL: info
tasks:
  say-hello:
    desc: Greet the user
    silent: false
    ignore_error: false
    dir: /tmp
    env:
      TASK_ENV: test
    vars:
      MSG: hi
    deps:
      - prepare-env
      - task: other-task
        vars:
          K: v
    cmds:
      - echo {{.GREETING}}
      - task: other-task
        vars:
          K: v
  prepare-env:
    cmds:
      - echo preparing
  other-task:
    cmds:
      - echo other
includes:
  sub:
    taskfile: ./sub/Taskfile.yaml
    dir: ./sub
    optional: true
"#;

    #[test]
    fn test_minimal_round_trip() {
        let tf: Taskfile = serde_yaml::from_str(MINIMAL_YAML).unwrap();
        assert_eq!(tf.version, "3");
        assert!(tf.tasks.contains_key("hello"));
        let task = &tf.tasks["hello"];
        assert_eq!(task.desc.as_deref(), Some("Say hello"));
        let cmds = task.cmds.as_ref().unwrap();
        assert_eq!(cmds.len(), 1);
        assert!(matches!(&cmds[0], Cmd::Shell(s) if s == "echo hello"));

        let yaml = serde_yaml::to_string(&tf).unwrap();
        let tf2: Taskfile = serde_yaml::from_str(&yaml).unwrap();
        assert_eq!(tf2.version, tf.version);
        assert_eq!(tf2.tasks.len(), tf.tasks.len());
    }

    #[test]
    fn test_full_round_trip() {
        let tf: Taskfile = serde_yaml::from_str(FULL_YAML).unwrap();
        assert_eq!(tf.version, "3");

        let vars = tf.vars.as_ref().unwrap();
        assert!(matches!(&vars["GREETING"], VarValue::Scalar(s) if s == "hello"));
        assert!(matches!(&vars["WHO"], VarValue::Shell { sh } if sh == "echo world"));

        let env = tf.env.as_ref().unwrap();
        assert_eq!(env["LOG_LEVEL"], "info");

        let task = &tf.tasks["say-hello"];
        let deps = task.deps.as_ref().unwrap();
        assert!(matches!(&deps[0], Dep::Simple(s) if s == "prepare-env"));
        assert!(matches!(&deps[1], Dep::Full { task, .. } if task == "other-task"));

        let cmds = task.cmds.as_ref().unwrap();
        assert!(matches!(&cmds[0], Cmd::Shell(s) if s.contains("GREETING")));
        assert!(matches!(&cmds[1], Cmd::TaskCall { task, .. } if task == "other-task"));

        let includes = tf.includes.as_ref().unwrap();
        let sub = &includes["sub"];
        assert_eq!(sub.taskfile, "./sub/Taskfile.yaml");
        assert_eq!(sub.optional, Some(true));

        let json = serde_json::to_string(&tf).unwrap();
        let tf2: Taskfile = serde_json::from_str(&json).unwrap();
        assert_eq!(tf2.tasks.len(), tf.tasks.len());
    }

    #[test]
    fn test_cmd_untagged_deserialization() {
        let yaml = r#"
version: "3"
tasks:
  t:
    cmds:
      - shell command here
      - task: other
"#;
        let tf: Taskfile = serde_yaml::from_str(yaml).unwrap();
        let cmds = tf.tasks["t"].cmds.as_ref().unwrap();
        assert!(matches!(&cmds[0], Cmd::Shell(_)));
        assert!(matches!(&cmds[1], Cmd::TaskCall { .. }));
    }

    #[test]
    fn test_dep_untagged_deserialization() {
        let yaml = r#"
version: "3"
tasks:
  t:
    deps:
      - simple-dep
      - task: complex-dep
        vars:
          KEY: val
    cmds:
      - echo hi
"#;
        let tf: Taskfile = serde_yaml::from_str(yaml).unwrap();
        let deps = tf.tasks["t"].deps.as_ref().unwrap();
        assert!(matches!(&deps[0], Dep::Simple(s) if s == "simple-dep"));
        assert!(matches!(&deps[1], Dep::Full { task, .. } if task == "complex-dep"));
    }
}
