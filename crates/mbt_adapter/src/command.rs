//! Parses one line of REPL input into a [`Command`].
//!
//! Tokenizing is shell-style word splitting (via `shlex`), so an argument
//! containing whitespace can be quoted (`input send "hello world"`) instead
//! of silently splitting into two tokens. Dispatch — matching the first word
//! against the known command set, binding the rest to typed fields,
//! reporting usage errors — is delegated to `clap`'s "multicall" mode: the
//! same mechanism clap's own `repl` example uses for a busybox-style command
//! line, where the first token picks the subcommand instead of naming an
//! `argv[0]` program.

use clap::Parser;

/// One REPL command. The request-shaped variants (`Input`, `Output`,
/// `Quiescence`, `GetEnabled`, `Reset`, `Heartbeat`, `Close`, `Hello`) are
/// turned into an `AdapterMessage` by the caller; the rest control the REPL
/// itself.
#[derive(Parser, Debug, Clone, PartialEq, Eq)]
#[command(multicall = true, disable_help_subcommand = true)]
pub enum Command {
    /// Show this message
    #[command(aliases = ["h", "?"])]
    Help,
    /// Close the connection and exit
    #[command(aliases = ["exit", "q"])]
    Quit,
    /// Send `hello`; an explicit protocol version overrides the default, for
    /// exercising the adapter/tool version-mismatch path
    Hello { protocol_version: Option<String> },
    /// Send `input` with a single-action multi-action
    Input {
        name: String,
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },
    /// Send `output` with a single-action multi-action
    Output {
        name: String,
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },
    /// Send `quiescence`
    #[command(alias = "quiesce")]
    Quiescence,
    /// Send `get_enabled`
    #[command(name = "get_enabled", alias = "enabled")]
    GetEnabled,
    /// Send `reset`
    Reset,
    /// Send `heartbeat`
    #[command(alias = "hb")]
    Heartbeat,
    /// Send `close`
    Close {
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        reason: Vec<String>,
    },
    /// Sends `text` verbatim as a single WebSocket text frame, bypassing
    /// `AdapterMessage` entirely — the escape hatch for malformed-on-purpose
    /// frames (bad `type`, missing fields, non-JSON). Built by [`parse`]
    /// before clap ever tokenizes the line, since the whole rest of the line
    /// must survive untouched (clap would strip/re-split quoting); excluded
    /// from clap's own parsing with `skip` accordingly.
    #[command(skip)]
    Raw(String),
    /// Block (up to a few seconds) for the next frame
    Recv,
    /// Check once, without blocking, for a pending frame
    Poll,
}

/// Parses one trimmed, non-empty line of REPL input.
pub fn parse(line: &str) -> Result<Command, String> {
    if let Some(rest) = strip_raw_prefix(line) {
        return if rest.is_empty() {
            Err("error: usage: raw <json text>".to_string())
        } else {
            Ok(Command::Raw(rest.to_string()))
        };
    }

    let mut tokens =
        shlex::split(line).ok_or_else(|| "error: unterminated quote or trailing backslash".to_string())?;
    if let Some(head) = tokens.first_mut() {
        head.make_ascii_lowercase();
    }

    Command::try_parse_from(tokens).map_err(|err| err.to_string())
}

/// If `line` starts with the (case-insensitive) word `raw`, returns the rest
/// of the line trimmed of leading/trailing whitespace; every other
/// character (including internal quotes and braces) survives verbatim,
/// since `raw` exists to send purposely-malformed text.
fn strip_raw_prefix(line: &str) -> Option<&str> {
    let mut parts = line.splitn(2, char::is_whitespace);
    let head = parts.next()?;
    if !head.eq_ignore_ascii_case("raw") {
        return None;
    }
    Some(parts.next().unwrap_or("").trim())
}

#[cfg(test)]
mod tests {
    use super::Command;
    use super::parse;

    #[test]
    fn parses_input_with_args() {
        assert_eq!(
            parse("input login 3 alice").unwrap(),
            Command::Input {
                name: "login".to_string(),
                args: vec!["3".to_string(), "alice".to_string()],
            }
        );
    }

    #[test]
    fn parses_quoted_argument_as_a_single_token() {
        assert_eq!(
            parse(r#"input send "hello world""#).unwrap(),
            Command::Input {
                name: "send".to_string(),
                args: vec!["hello world".to_string()],
            }
        );
    }

    #[test]
    fn unterminated_quote_is_an_error() {
        assert!(parse(r#"input send "hello"#).is_err());
    }

    #[test]
    fn parses_input_with_no_args() {
        assert_eq!(
            parse("output ack").unwrap(),
            Command::Output {
                name: "ack".to_string(),
                args: vec![],
            }
        );
    }

    #[test]
    fn input_without_a_name_is_an_error() {
        assert!(parse("input").is_err());
    }

    #[test]
    fn hello_without_version_is_none() {
        assert_eq!(parse("hello").unwrap(), Command::Hello { protocol_version: None });
    }

    #[test]
    fn hello_with_version() {
        assert_eq!(
            parse("hello 9.9").unwrap(),
            Command::Hello {
                protocol_version: Some("9.9".to_string())
            }
        );
    }

    #[test]
    fn close_collects_a_multi_word_reason() {
        assert_eq!(
            parse("close test finished early").unwrap(),
            Command::Close {
                reason: vec!["test".to_string(), "finished".to_string(), "early".to_string()]
            }
        );
    }

    #[test]
    fn raw_requires_a_body() {
        assert!(parse("raw").is_err());
    }

    #[test]
    fn raw_keeps_the_rest_of_the_line_verbatim() {
        assert_eq!(
            parse(r#"raw {"type": "nope"}"#).unwrap(),
            Command::Raw(r#"{"type": "nope"}"#.to_string())
        );
    }

    #[test]
    fn unknown_command_is_an_error() {
        assert!(parse("frobnicate").is_err());
    }

    #[test]
    fn commands_are_case_insensitive() {
        assert_eq!(parse("HELLO").unwrap(), Command::Hello { protocol_version: None });
        assert_eq!(parse("RAW {}").unwrap(), Command::Raw("{}".to_string()));
    }

    #[test]
    fn aliases_work() {
        assert_eq!(parse("q").unwrap(), Command::Quit);
        assert_eq!(parse("?").unwrap(), Command::Help);
        assert_eq!(parse("hb").unwrap(), Command::Heartbeat);
    }
}
