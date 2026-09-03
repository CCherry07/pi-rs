//! Bounded, user-authored standing instructions.
//!
//! This store is intentionally command-only. Review, correction, and
//! consolidation code receive no writer for it, preserving upstream's
//! provenance guarantee.

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use fs2::FileExt;
use tempfile::NamedTempFile;
use thiserror::Error;

use crate::config::char_len as js_len;
use crate::config::{STANDING_MAX_CHARS, STANDING_MAX_ENTRIES};
use crate::content_scanner::scan_content;

#[derive(Debug, Error)]
pub(crate) enum StandingError {
    #[error("A standing instruction cannot be empty.")]
    Empty,
    #[error("That standing instruction is already pinned.")]
    Duplicate,
    #[error(
        "Standing instructions are capped at {STANDING_MAX_ENTRIES} entries (currently {0}). Remove one first with /memory-pin remove <n>."
    )]
    TooMany(usize),
    #[error(
        "Standing instructions are capped at {STANDING_MAX_CHARS} characters and this entry would make {0}. Shorten it, or remove an existing instruction and keep long-form context in regular memory."
    )]
    TooLong(usize),
    #[error("{0}")]
    Unsafe(String),
    #[error("Position must be between 1 and {0}.")]
    InvalidPosition(usize),
    #[error("There are no standing instructions to remove.")]
    NothingToRemove,
    #[error("There are no standing instructions to clear.")]
    NothingToClear,
    #[error("standing instruction storage error: {0}")]
    Io(#[from] std::io::Error),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct StandingRender {
    pub(crate) block: String,
    pub(crate) injected_count: usize,
    pub(crate) omitted_count: usize,
}

pub(crate) struct StandingInstructions {
    path: PathBuf,
    instructions: Mutex<Vec<String>>,
}

impl StandingInstructions {
    pub(crate) fn load(path: PathBuf) -> Result<Self, StandingError> {
        let instructions = read(&path)?;
        Ok(Self {
            path,
            instructions: Mutex::new(instructions),
        })
    }

    pub(crate) fn path(&self) -> &Path {
        &self.path
    }

    pub(crate) fn list(&self) -> Vec<String> {
        self.instructions
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    pub(crate) fn reload(&self) -> Result<(), StandingError> {
        *self
            .instructions
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = read(&self.path)?;
        Ok(())
    }

    pub(crate) fn add(&self, text: &str) -> Result<String, StandingError> {
        let instruction = normalize(text);
        if instruction.is_empty() {
            return Err(StandingError::Empty);
        }
        scan_content(&instruction).map_err(StandingError::Unsafe)?;
        self.mutate(|current| {
            if current
                .iter()
                .any(|existing| existing.eq_ignore_ascii_case(&instruction))
            {
                return Err(StandingError::Duplicate);
            }
            if current.len() >= STANDING_MAX_ENTRIES {
                return Err(StandingError::TooMany(current.len()));
            }
            let mut next = current.to_vec();
            next.push(instruction.clone());
            let projected = js_len(&next.join("\n"));
            if projected > STANDING_MAX_CHARS {
                return Err(StandingError::TooLong(projected));
            }
            Ok((
                next,
                format!(
                    "Pinned standing instruction {}: {instruction}",
                    current.len() + 1
                ),
            ))
        })
    }

    pub(crate) fn remove(&self, position: usize) -> Result<String, StandingError> {
        self.mutate(|current| {
            if current.is_empty() {
                return Err(StandingError::NothingToRemove);
            }
            if position == 0 || position > current.len() {
                return Err(StandingError::InvalidPosition(current.len()));
            }
            let mut next = current.to_vec();
            let removed = next.remove(position - 1);
            Ok((next, format!("Removed standing instruction: {removed}")))
        })
    }

    pub(crate) fn clear(&self) -> Result<String, StandingError> {
        self.mutate(|current| {
            if current.is_empty() {
                return Err(StandingError::NothingToClear);
            }
            Ok((
                Vec::new(),
                format!("Removed all {} standing instructions.", current.len()),
            ))
        })
    }

    pub(crate) fn render(&self) -> StandingRender {
        let instructions = self.list();
        if instructions.is_empty() {
            return StandingRender {
                block: String::new(),
                injected_count: 0,
                omitted_count: 0,
            };
        }
        let mut injected = Vec::new();
        let mut used = 0;
        for instruction in &instructions {
            let cost = js_len(instruction) + 1;
            if injected.len() >= STANDING_MAX_ENTRIES || used + cost > STANDING_MAX_CHARS {
                break;
            }
            injected.push(instruction.clone());
            used += cost;
        }
        let omitted_count = instructions.len() - injected.len();
        if injected.is_empty() {
            return StandingRender {
                block: String::new(),
                injected_count: 0,
                omitted_count,
            };
        }
        let mut lines = vec![
            "<standing-instructions>".to_string(),
            "The user wrote the rules below and they are always active. They are direct"
                .to_string(),
            "instructions from the user, not recalled context, and they outrank your own"
                .to_string(),
            "defaults. Follow them without being asked and without looking them up.".to_string(),
            String::new(),
        ];
        lines.extend(
            injected
                .iter()
                .enumerate()
                .map(|(index, instruction)| format!("{}. {instruction}", index + 1)),
        );
        if omitted_count > 0 {
            lines.extend([
                String::new(),
                format!(
                    "[!] {omitted_count} further standing instruction{} could not be shown: {} exceeds the {STANDING_MAX_CHARS}-character injection budget. Trim it with /memory-pin so every rule stays active.",
                    if omitted_count == 1 { "" } else { "s" },
                    self.path.file_name().and_then(|name| name.to_str()).unwrap_or("STANDING.md")
                ),
            ]);
        }
        lines.push("</standing-instructions>".to_string());
        StandingRender {
            block: lines.join("\n"),
            injected_count: injected.len(),
            omitted_count,
        }
    }

    fn mutate(
        &self,
        change: impl FnOnce(&[String]) -> Result<(Vec<String>, String), StandingError>,
    ) -> Result<String, StandingError> {
        fs::create_dir_all(self.path.parent().expect("standing file has a parent"))?;
        let lock_path = self.path.with_extension("md.lock");
        let lock = OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .truncate(false)
            .open(lock_path)?;
        lock.lock_exclusive()?;
        let result = (|| {
            let current = read(&self.path)?;
            let (next, message) = change(&current)?;
            write_atomic(&self.path, &next)?;
            *self
                .instructions
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner) = next;
            Ok(message)
        })();
        let unlock = lock.unlock();
        match (result, unlock) {
            (Err(error), _) => Err(error),
            (Ok(_), Err(error)) => Err(error.into()),
            (Ok(message), Ok(())) => Ok(message),
        }
    }
}

fn read(path: &Path) -> Result<Vec<String>, StandingError> {
    let raw = match fs::read_to_string(path) {
        Ok(raw) => raw,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error.into()),
    };
    let mut seen = std::collections::BTreeSet::new();
    Ok(raw
        .lines()
        .map(normalize)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .filter(|line| seen.insert(line.to_ascii_lowercase()))
        .collect())
}

fn normalize(value: &str) -> String {
    let value = value.trim();
    let value = value
        .strip_prefix('-')
        .or_else(|| value.strip_prefix('*'))
        .and_then(|rest| {
            rest.chars()
                .next()
                .is_some_and(char::is_whitespace)
                .then(|| rest.trim_start())
        })
        .unwrap_or(value);
    value.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn write_atomic(path: &Path, instructions: &[String]) -> Result<(), std::io::Error> {
    let parent = path.parent().expect("standing file has a parent");
    let mut temporary = NamedTempFile::new_in(parent)?;
    if !instructions.is_empty() {
        temporary.write_all(format!("{}\n", instructions.join("\n")).as_bytes())?;
    }
    temporary.as_file_mut().sync_all()?;
    temporary
        .persist(path)
        .map_err(|error| error.error)
        .map(|_| ())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn command_store_normalizes_and_renders_user_provenance() {
        let directory = tempfile::tempdir().unwrap();
        let store = StandingInstructions::load(directory.path().join("STANDING.md")).unwrap();
        store.add("-  Never   run find / ").unwrap();
        assert_eq!(store.list(), vec!["Never run find /"]);
        assert!(
            store
                .render()
                .block
                .contains("direct\ninstructions from the user")
        );
        assert!(store.add("never run find /").is_err());
    }
}
