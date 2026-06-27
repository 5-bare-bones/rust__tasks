use std::path::{Path, PathBuf};

use taskfile_schema::Taskfile;

#[derive(Debug, thiserror::Error)]
pub enum LoadError {
    #[error("No taskfile found starting from {0}")]
    NotFound(PathBuf),
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
    #[error("YAML parse error: {0}")]
    Yaml(#[from] serde_yaml::Error),
    #[error("SYON error: {0}")]
    Syon(#[from] syon_integration::SyonError),
}

#[derive(Debug, Clone, Copy)]
enum Format {
    Yaml,
    Syon,
}

pub fn find_and_load(start_dir: &Path) -> Result<(Taskfile, PathBuf), LoadError> {
    let (path, format) = detect_taskfile(start_dir)?;
    let tf = load_file(&path, format)?;
    let base_dir = path
        .parent()
        .unwrap_or(Path::new("."))
        .to_path_buf();
    let tf = resolve_includes(tf, &base_dir)?;
    Ok((tf, path))
}

fn detect_taskfile(start: &Path) -> Result<(PathBuf, Format), LoadError> {
    let mut dir = start.to_path_buf();
    loop {
        for &(name, format) in &[
            ("Taskfile.syon", Format::Syon),
            ("Taskfile.yaml", Format::Yaml),
            ("Taskfile.yml", Format::Yaml),
        ] {
            let candidate = dir.join(name);
            if candidate.exists() {
                return Ok((candidate, format));
            }
        }
        if !dir.pop() {
            return Err(LoadError::NotFound(start.to_path_buf()));
        }
    }
}

fn load_file(path: &Path, format: Format) -> Result<Taskfile, LoadError> {
    match format {
        Format::Syon => Ok(syon_integration::load_syon_taskfile(path)?),
        Format::Yaml => {
            let content = std::fs::read_to_string(path)?;
            Ok(serde_yaml::from_str(&content)?)
        }
    }
}

fn resolve_includes(mut tf: Taskfile, base_dir: &Path) -> Result<Taskfile, LoadError> {
    let includes = match tf.includes.take() {
        Some(m) => m,
        None => return Ok(tf),
    };

    for (namespace, include) in includes {
        let include_path = base_dir.join(&include.taskfile);
        if !include_path.exists() {
            if include.optional.unwrap_or(false) {
                continue;
            }
            return Err(LoadError::Io(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                format!(
                    "included taskfile not found: {}",
                    include_path.display()
                ),
            )));
        }

        let include_dir = include_path
            .parent()
            .unwrap_or(Path::new("."))
            .to_path_buf();

        let ext = include_path
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("");
        let format = if ext == "syon" {
            Format::Syon
        } else {
            Format::Yaml
        };

        let included = load_file(&include_path, format)?;
        let included = resolve_includes(included, &include_dir)?;

        for (task_name, task) in included.tasks {
            tf.tasks
                .insert(format!("{namespace}:{task_name}"), task);
        }
    }

    Ok(tf)
}
