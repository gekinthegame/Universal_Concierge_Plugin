//! Pure REPL command parsing.
//!
//! These helpers only classify and split already-read input lines. Command
//! execution stays in `repl.rs`.

pub(crate) fn checkpoint_command_arg(line: &str) -> Option<&str> {
    let arg = line.strip_prefix("/checkpoint")?;
    match arg.chars().next() {
        None => Some(arg),
        Some(c) if c.is_whitespace() => Some(arg),
        Some(_) => None,
    }
}

pub(crate) fn parse_checkpoint_args(arg: &str) -> (&str, &str) {
    if arg.is_empty() {
        return ("current-project", "chat-checkpoint");
    }
    match arg.split_once(char::is_whitespace) {
        Some((name, label)) => (name, label.trim()),
        None => (arg, "chat-checkpoint"),
    }
}

/// Match `/work <job>` (but not `/workfoo`), returning the trailing argument.
pub(crate) fn work_command_arg(line: &str) -> Option<&str> {
    let arg = line.strip_prefix("/work")?;
    match arg.chars().next() {
        None => Some(arg),
        Some(c) if c.is_whitespace() => Some(arg),
        Some(_) => None,
    }
}

/// Match `/resume` and `/resume <arg>` (but not `/resumefoo`), returning the
/// trailing argument (empty for a bare `/resume`).
pub(crate) fn resume_command_arg(line: &str) -> Option<&str> {
    let arg = line.strip_prefix("/resume")?;
    match arg.chars().next() {
        None => Some(arg),
        Some(c) if c.is_whitespace() => Some(arg),
        Some(_) => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn checkpoint_command_args_have_stable_defaults() {
        assert_eq!(checkpoint_command_arg("/checkpoint"), Some(""));
        assert_eq!(
            checkpoint_command_arg("/checkpoint current-project"),
            Some(" current-project")
        );
        assert_eq!(checkpoint_command_arg("/checkpointfoo"), None);
        assert_eq!(
            parse_checkpoint_args(""),
            ("current-project", "chat-checkpoint")
        );
        assert_eq!(
            parse_checkpoint_args("current-project"),
            ("current-project", "chat-checkpoint")
        );
        assert_eq!(
            parse_checkpoint_args("current-project before restart"),
            ("current-project", "before restart")
        );
    }

    #[test]
    fn resume_command_arg_handles_bare_and_named() {
        assert_eq!(resume_command_arg("/resume"), Some(""));
        assert_eq!(resume_command_arg("/resume latest"), Some(" latest"));
        assert_eq!(resume_command_arg("/resumefoo"), None);
    }

    #[test]
    fn work_command_arg_is_not_a_prefix_match() {
        assert_eq!(work_command_arg("/work build"), Some(" build"));
        assert_eq!(work_command_arg("/work"), Some(""));
        assert_eq!(work_command_arg("/workflow"), None);
    }
}
