//! A dump publishes a new directory. Existing caller data is never a replacement target.
use std::ffi::OsString;
use std::io;
use std::path::{Path, PathBuf};

use serde_json::{json, Value};

use super::{EmulatorLink, LinkError, ToolOutput};

#[cfg(unix)]
mod unix;
#[cfg(unix)]
use unix::Directory;
#[cfg(windows)]
mod windows;
#[cfg(windows)]
use windows::Directory;

fn failure(context: &str, error: impl std::fmt::Display) -> LinkError {
    LinkError::Protocol(format!("{context}: {error}"))
}

/// Stage region files and state.json together, then exclusively publish to a new caller path.
pub fn dump_memory(link: &mut dyn EmulatorLink, dir: &str) -> Result<ToolOutput, LinkError> {
    let mut staging = DumpStaging::new(Path::new(dir))
        .map_err(|e| failure("cannot prepare new dump directory", e))?;
    let capture = (|| -> Result<Value, LinkError> {
        let path = staging
            .path()
            .to_str()
            .ok_or_else(|| failure("invalid dump staging path", "UTF-8 is required"))?;
        let regions = link.call("dump_memory", json!({ "path": path }))?;
        staging
            .check_binding()
            .map_err(|e| failure("dump staging changed", e))?;
        let state = link.call("get_state", json!({}))?;
        staging
            .check_binding()
            .map_err(|e| failure("dump staging changed", e))?;
        let bytes = serde_json::to_vec(state.get("state").unwrap_or(&state))
            .map_err(|e| failure("cannot encode dump state", e))?;
        staging
            .payload
            .as_ref()
            .unwrap()
            .write_new_file("state.json", &bytes)
            .map_err(|e| failure("failed to write state.json", e))?;
        staging
            .publish()
            .map_err(|e| failure("failed to publish new dump directory", e))?;
        Ok(regions)
    })();
    let cleanup = staging.cleanup();
    let mut regions = match (capture, cleanup) {
        (Ok(regions), Ok(())) => regions,
        (Err(error), Ok(())) => return Err(error),
        (capture, Err(error)) => {
            return Err(failure(
                "dump cleanup failed",
                format!(
                    "{}; {error}; inspect {}",
                    capture
                        .err()
                        .map(|e| e.to_string())
                        .unwrap_or_else(|| format!("complete dump published at {dir}")),
                    staging.container_path.display()
                ),
            ))
        }
    };
    if let Some(object) = regions.as_object_mut() {
        object.insert("path".into(), json!(dir));
    }
    Ok(ToolOutput::Json(regions))
}

struct DumpStaging {
    parent: Directory,
    container: Option<Directory>,
    payload: Option<Directory>,
    container_name: OsString,
    container_path: PathBuf,
    destination: OsString,
}

impl DumpStaging {
    fn new(destination: &Path) -> io::Result<Self> {
        let text = destination.to_str().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "dump destination must be UTF-8",
            )
        })?;
        if matches!(
            text.rsplit(std::path::is_separator)
                .find(|part| !part.is_empty()),
            None | Some("." | "..")
        ) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "dump destination needs a new directory name",
            ));
        }
        let name = destination
            .file_name()
            .ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "dump destination needs a new directory name",
                )
            })?
            .to_owned();
        #[cfg(windows)]
        if !crate::path_safety::is_portable_relative_member(&name.to_string_lossy()) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "invalid Windows dump directory name",
            ));
        }
        // Fail before any adapter call or staging effect, including for dangling links.
        match std::fs::symlink_metadata(destination) {
            Ok(_) => {
                return Err(io::Error::new(
                    io::ErrorKind::AlreadyExists,
                    "dump destination already exists; choose a new path",
                ))
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(e) => return Err(e),
        }
        let parent_path = destination
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or(Path::new("."));
        std::fs::create_dir_all(parent_path)?;
        let parent = Directory::open_parent(&std::fs::canonicalize(parent_path)?)?;
        let container_name = OsString::from(format!(".dump-staging-{}", ulid::Ulid::generate()));
        let container = parent.create_dir(&container_name)?;
        let container_path = container.path.clone();
        let mut staging = Self {
            parent,
            container: Some(container),
            payload: None,
            container_name,
            container_path,
            destination: name,
        };
        let initialize = (|| {
            staging.payload = Some(
                staging
                    .container
                    .as_ref()
                    .unwrap()
                    .create_dir("payload".as_ref())?,
            );
            staging.check_binding()
        })();
        if let Err(error) = initialize {
            return Err(match staging.cleanup() {
                Ok(()) => error,
                Err(cleanup) => io::Error::new(
                    error.kind(),
                    format!(
                        "{error}; staging cleanup failed: {cleanup}; inspect {}",
                        staging.container_path.display()
                    ),
                ),
            });
        }
        Ok(staging)
    }

    fn path(&self) -> &Path {
        &self.payload.as_ref().unwrap().path
    }

    fn check_binding(&self) -> io::Result<()> {
        self.parent.check_binding()?;
        self.container.as_ref().unwrap().check_binding()?;
        self.payload.as_ref().unwrap().check_binding()
    }

    fn publish(&mut self) -> io::Result<()> {
        self.check_binding()?;
        self.container.as_ref().unwrap().publish_child(
            self.payload.as_ref().unwrap(),
            "payload".as_ref(),
            &self.parent,
            &self.destination,
        )?;
        // Ownership moved to the caller. Neither cleanup nor Drop may remove the published dump.
        self.payload.take();
        Ok(())
    }

    fn cleanup(&mut self) -> io::Result<()> {
        if let Some(payload) = self.payload.take() {
            payload.remove_contents(&mut 10_000, 0)?;
            self.container
                .as_ref()
                .unwrap()
                .remove_empty_child("payload".as_ref(), payload)?;
        }
        if let Some(container) = self.container.take() {
            self.parent
                .remove_empty_child(&self.container_name, container)?;
        }
        Ok(())
    }
}

impl Drop for DumpStaging {
    fn drop(&mut self) {
        let _ = self.cleanup();
    }
}

#[cfg(test)]
#[path = "dump_tests.rs"]
mod tests;
