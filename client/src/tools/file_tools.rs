use mirage_core::{Tool, completion::ToolDefinition};
use serde::Deserialize;
use serde_json::json;
use std::{
    fs,
    path::{Path, PathBuf},
};
use thiserror::Error;

#[derive(Debug, Deserialize)]
pub struct ReadFileArgs {
    path: String,
    #[serde(default)]
    start_line: Option<usize>,
    #[serde(default)]
    line_count: Option<usize>,
}

#[derive(Debug, Deserialize)]
pub struct WriteFileArgs {
    path: String,
    content: String,
    #[serde(default)]
    append: bool,
    #[serde(default)]
    overwrite_existing: bool,
    #[serde(default)]
    create_parent_directories: bool,
}

#[derive(Debug, Deserialize)]
pub struct EditFileArgs {
    path: String,
    old_text: String,
    new_text: String,
    #[serde(default)]
    replace_all: bool,
}

#[derive(Debug, Error)]
pub enum FileToolError {
    #[error("failed to access file: {0}")]
    Io(#[from] std::io::Error),
    #[error("line numbering starts at 1")]
    InvalidStartLine,
    #[error("`old_text` must not be empty")]
    EmptyOldText,
    #[error("path has no parent directory: {0}")]
    MissingParentDirectory(String),
    #[error("path is empty")]
    EmptyPath,
    #[error(
        "refusing to overwrite existing file {0}; use `edit_file` for targeted edits or set `overwrite_existing=true` for an intentional whole-file rewrite"
    )]
    OverwriteRequiresExplicitIntent(String),
    #[error("text to replace was not found in {0}")]
    OldTextNotFound(String),
    #[error(
        "found {count} matching occurrences in {path}; refine `old_text` or set `replace_all=true`"
    )]
    AmbiguousEdit { path: String, count: usize },
    #[error("failed to join file task: {0}")]
    Join(#[from] tokio::task::JoinError),
}

pub struct ReadFileTool;

impl Tool for ReadFileTool {
    const NAME: &'static str = "read_file";

    type Error = FileToolError;
    type Args = ReadFileArgs;
    type Output = String;

    async fn definition(&self, _prompt: String) -> ToolDefinition {
        ToolDefinition {
            name: Self::NAME.to_owned(),
            description: "Read a UTF-8 text file from disk, optionally slicing by line range."
                .to_owned(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "Relative or absolute path to the file to read."
                    },
                    "start_line": {
                        "type": "integer",
                        "description": "Optional 1-based starting line number."
                    },
                    "line_count": {
                        "type": "integer",
                        "description": "Optional number of lines to return."
                    }
                },
                "required": ["path"]
            }),
        }
    }

    async fn call(&self, args: Self::Args) -> Result<Self::Output, Self::Error> {
        tokio::task::spawn_blocking(move || read_text_file(args)).await?
    }
}

pub struct WriteFileTool;

impl Tool for WriteFileTool {
    const NAME: &'static str = "write_file";

    type Error = FileToolError;
    type Args = WriteFileArgs;
    type Output = String;

    async fn definition(&self, _prompt: String) -> ToolDefinition {
        ToolDefinition {
            name: Self::NAME.to_owned(),
            description: "Write UTF-8 text to a file, optionally appending or creating parents. Use this for whole-file writes or new files, not targeted edits.".to_owned(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "Relative or absolute path to the file to write."
                    },
                    "content": {
                        "type": "string",
                        "description": "The full text content to write."
                    },
                    "append": {
                        "type": "boolean",
                        "description": "Append to the file instead of overwriting it."
                    },
                    "overwrite_existing": {
                        "type": "boolean",
                        "description": "Required when intentionally replacing the full contents of an existing file."
                    },
                    "create_parent_directories": {
                        "type": "boolean",
                        "description": "Create missing parent directories before writing."
                    }
                },
                "required": ["path", "content"]
            }),
        }
    }

    async fn call(&self, args: Self::Args) -> Result<Self::Output, Self::Error> {
        tokio::task::spawn_blocking(move || write_text_file(args)).await?
    }
}

pub struct EditFileTool;

impl Tool for EditFileTool {
    const NAME: &'static str = "edit_file";

    type Error = FileToolError;
    type Args = EditFileArgs;
    type Output = String;

    async fn definition(&self, _prompt: String) -> ToolDefinition {
        ToolDefinition {
            name: Self::NAME.to_owned(),
            description: "Edit part of an existing UTF-8 text file by replacing matching text. Prefer this over `write_file` for targeted edits.".to_owned(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "Relative or absolute path to the file to edit."
                    },
                    "old_text": {
                        "type": "string",
                        "description": "Exact existing text to replace. Make this specific enough to identify the intended edit."
                    },
                    "new_text": {
                        "type": "string",
                        "description": "Replacement text."
                    },
                    "replace_all": {
                        "type": "boolean",
                        "description": "Replace every occurrence of `old_text` instead of requiring a unique match."
                    }
                },
                "required": ["path", "old_text", "new_text"]
            }),
        }
    }

    async fn call(&self, args: Self::Args) -> Result<Self::Output, Self::Error> {
        tokio::task::spawn_blocking(move || edit_text_file(args)).await?
    }
}

fn read_text_file(args: ReadFileArgs) -> Result<String, FileToolError> {
    let path = resolve_path(&args.path)?;
    let content = fs::read_to_string(&path)?;
    let lines: Vec<&str> = content.lines().collect();

    if lines.is_empty() {
        return Ok("File is empty.".to_owned());
    }

    let start_line = args.start_line.unwrap_or(1);
    if start_line == 0 {
        return Err(FileToolError::InvalidStartLine);
    }

    let start_index = start_line.saturating_sub(1);
    let selected = if start_index >= lines.len() {
        Vec::new()
    } else if let Some(line_count) = args.line_count {
        lines
            .into_iter()
            .skip(start_index)
            .take(line_count)
            .collect()
    } else {
        lines.into_iter().skip(start_index).collect()
    };

    if selected.is_empty() {
        return Ok("No lines matched the requested range.".to_owned());
    }

    Ok(selected
        .into_iter()
        .enumerate()
        .map(|(offset, line)| format!("{}|{}", start_line + offset, line))
        .collect::<Vec<_>>()
        .join("\n"))
}

fn write_text_file(args: WriteFileArgs) -> Result<String, FileToolError> {
    let path = resolve_path(&args.path)?;

    if path.exists() && !args.append && !args.overwrite_existing {
        return Err(FileToolError::OverwriteRequiresExplicitIntent(
            path.display().to_string(),
        ));
    }

    if args.create_parent_directories {
        let parent = path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
            .ok_or_else(|| FileToolError::MissingParentDirectory(path.display().to_string()))?;
        fs::create_dir_all(parent)?;
    }

    if args.append {
        use std::io::Write;

        let mut file = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)?;
        file.write_all(args.content.as_bytes())?;
    } else {
        fs::write(&path, args.content.as_bytes())?;
    }

    Ok(format!(
        "Wrote {} bytes to {}",
        args.content.len(),
        path.display()
    ))
}

fn edit_text_file(args: EditFileArgs) -> Result<String, FileToolError> {
    if args.old_text.is_empty() {
        return Err(FileToolError::EmptyOldText);
    }

    let path = resolve_path(&args.path)?;
    let original = fs::read_to_string(&path)?;
    let occurrences = original.matches(&args.old_text).count();

    if occurrences == 0 {
        return Err(FileToolError::OldTextNotFound(path.display().to_string()));
    }

    let updated = if args.replace_all {
        original.replace(&args.old_text, &args.new_text)
    } else {
        if occurrences != 1 {
            return Err(FileToolError::AmbiguousEdit {
                path: path.display().to_string(),
                count: occurrences,
            });
        }

        original.replacen(&args.old_text, &args.new_text, 1)
    };

    fs::write(&path, updated.as_bytes())?;

    Ok(format!(
        "Edited {} occurrence(s) in {}",
        if args.replace_all { occurrences } else { 1 },
        path.display()
    ))
}

fn resolve_path(path: &str) -> Result<PathBuf, FileToolError> {
    if path.trim().is_empty() {
        return Err(FileToolError::EmptyPath);
    }

    let candidate = Path::new(path);
    if candidate.is_absolute() {
        Ok(candidate.to_path_buf())
    } else {
        Ok(std::env::current_dir()?.join(candidate))
    }
}

#[cfg(test)]
mod tests {
    use super::{
        EditFileArgs, ReadFileArgs, WriteFileArgs, edit_text_file, read_text_file, write_text_file,
    };
    use std::{
        fs,
        path::PathBuf,
        time::{SystemTime, UNIX_EPOCH},
    };

    fn unique_test_path(name: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!("mirage-{name}-{}-{nanos}", std::process::id()))
    }

    #[test]
    fn reads_line_ranges_with_numbers() {
        let path = unique_test_path("read");
        fs::write(&path, "alpha\nbeta\ngamma\n").unwrap();

        let output = read_text_file(ReadFileArgs {
            path: path.display().to_string(),
            start_line: Some(2),
            line_count: Some(2),
        })
        .unwrap();

        assert_eq!(output, "2|beta\n3|gamma");

        let _ = fs::remove_file(path);
    }

    #[test]
    fn writes_and_appends_file_contents() {
        let path = unique_test_path("write");

        let first = write_text_file(WriteFileArgs {
            path: path.display().to_string(),
            content: "hello".to_owned(),
            append: false,
            overwrite_existing: false,
            create_parent_directories: false,
        })
        .unwrap();
        assert!(first.contains("Wrote 5 bytes"));

        write_text_file(WriteFileArgs {
            path: path.display().to_string(),
            content: " world".to_owned(),
            append: true,
            overwrite_existing: false,
            create_parent_directories: false,
        })
        .unwrap();

        assert_eq!(fs::read_to_string(&path).unwrap(), "hello world");

        let _ = fs::remove_file(path);
    }

    #[test]
    fn write_file_requires_explicit_overwrite_for_existing_files() {
        let path = unique_test_path("overwrite-guard");
        fs::write(&path, "before").unwrap();

        let error = write_text_file(WriteFileArgs {
            path: path.display().to_string(),
            content: "after".to_owned(),
            append: false,
            overwrite_existing: false,
            create_parent_directories: false,
        })
        .unwrap_err();

        assert!(error.to_string().contains("overwrite_existing=true"));
        assert_eq!(fs::read_to_string(&path).unwrap(), "before");

        let _ = fs::remove_file(path);
    }

    #[test]
    fn write_file_can_explicitly_overwrite_existing_files() {
        let path = unique_test_path("overwrite");
        fs::write(&path, "before").unwrap();

        let result = write_text_file(WriteFileArgs {
            path: path.display().to_string(),
            content: "after".to_owned(),
            append: false,
            overwrite_existing: true,
            create_parent_directories: false,
        })
        .unwrap();

        assert!(result.contains("Wrote 5 bytes"));
        assert_eq!(fs::read_to_string(&path).unwrap(), "after");

        let _ = fs::remove_file(path);
    }

    #[test]
    fn edits_unique_match_in_existing_file() {
        let path = unique_test_path("edit");
        fs::write(&path, "alpha\nbeta\ngamma\n").unwrap();

        let result = edit_text_file(EditFileArgs {
            path: path.display().to_string(),
            old_text: "beta".to_owned(),
            new_text: "delta".to_owned(),
            replace_all: false,
        })
        .unwrap();

        assert!(result.contains("Edited 1 occurrence"));
        assert_eq!(fs::read_to_string(&path).unwrap(), "alpha\ndelta\ngamma\n");

        let _ = fs::remove_file(path);
    }

    #[test]
    fn edit_file_requires_unique_match_without_replace_all() {
        let path = unique_test_path("edit-ambiguous");
        fs::write(&path, "same\nsame\n").unwrap();

        let error = edit_text_file(EditFileArgs {
            path: path.display().to_string(),
            old_text: "same".to_owned(),
            new_text: "diff".to_owned(),
            replace_all: false,
        })
        .unwrap_err();

        assert!(error.to_string().contains("refine `old_text`"));

        let _ = fs::remove_file(path);
    }

    #[test]
    fn edit_file_can_replace_all_matches() {
        let path = unique_test_path("edit-all");
        fs::write(&path, "same\nsame\n").unwrap();

        let result = edit_text_file(EditFileArgs {
            path: path.display().to_string(),
            old_text: "same".to_owned(),
            new_text: "diff".to_owned(),
            replace_all: true,
        })
        .unwrap();

        assert!(result.contains("Edited 2 occurrence"));
        assert_eq!(fs::read_to_string(&path).unwrap(), "diff\ndiff\n");

        let _ = fs::remove_file(path);
    }
}
