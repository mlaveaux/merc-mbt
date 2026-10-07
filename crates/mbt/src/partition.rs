use std::fmt;
use std::io::BufRead;
use std::io::BufReader;
use std::io::Read;

use merc_lps::ExplicitLinearProcessSpecification;
use merc_utilities::MercError;

/// Whether an action belongs to the input or output alphabet.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActionClass {
    Input,
    Output,
}

/// A parsed action pattern: a name and the arity implied by its parameter
/// list.
///
/// Conditional guards (`w -> a(w,v)`) are parsed but rejected in this
/// version: evaluating one needs a typed mCRL2 data expression over the
/// LPS's own data specification, which the current FFI cannot produce.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActionPattern {
    pub name: String,
    pub arity: usize,
}

/// The input/output classification of every observable action of an LPS,
/// loaded from a partition file: a restricted `lpsactionrename`-like syntax
/// with `input`/`output` sections instead of a rename section.
#[derive(Debug, Clone, Default)]
pub struct ActionPartition {
    inputs: Vec<ActionPattern>,
    outputs: Vec<ActionPattern>,
}

impl ActionPartition {
    /// Classifies a concrete action by `(name, arity)`, or `None` if no
    /// pattern in the partition covers it.
    pub fn classify(&self, name: &str, arity: usize) -> Option<ActionClass> {
        if self.inputs.iter().any(|p| p.name == name && p.arity == arity) {
            Some(ActionClass::Input)
        } else if self.outputs.iter().any(|p| p.name == name && p.arity == arity) {
            Some(ActionClass::Output)
        } else {
            None
        }
    }

    /// Checks this partition against an LPS's summands: every `(name,
    /// arity)` occurring in a multi-action must be covered by exactly one
    /// section (completeness and disjointness), and no summand's
    /// multi-action may mix input- and output-classified actions
    /// (homogeneity). Tau summands (the empty multi-action) are exempt.
    ///
    /// Run once at startup, immediately after loading both the LPS and the
    /// partition file — not on the hot path, since every `(name, arity)`
    /// pair used by the LPS is statically known from its summand templates.
    pub fn validate_against_lps(&self, lps: &ExplicitLinearProcessSpecification) -> Result<(), MercError> {
        for key in crate::action::lps_action_keys(lps) {
            if key.is_tau() {
                // Tau summand: exempt from classification.
                continue;
            }

            let mut saw_input = false;
            let mut saw_output = false;
            for action in key.as_wire() {
                match self.classify(&action.name, action.args.len()) {
                    Some(ActionClass::Input) => saw_input = true,
                    Some(ActionClass::Output) => saw_output = true,
                    None => {
                        return Err(PartitionError::Uncovered {
                            name: action.name,
                            arity: action.args.len(),
                        }
                        .into());
                    }
                }
            }

            if saw_input && saw_output {
                return Err(PartitionError::MixedMultiAction.into());
            }
        }

        Ok(())
    }
}

/// Parses a partition file, read from `reader`, into an [`ActionPartition`].
///
/// Accepted grammar (a strict subset of the spec's proposal — comments `%`
/// run to end of line, matching mCRL2):
///
/// ```text
/// file    := varsec? section+
/// varsec  := "var" (ident ":" sort ";")+          // parsed and retained; unused in v1
/// section := ("input" | "output") rule+
/// rule    := (dataexpr "->")? action ";"          // the guard is *parsed* but rejected in v1
/// action  := ident ("(" ident ("," ident)* ")")?  // arguments must be distinct variable names
/// ```
pub fn parse_partition<R: Read>(reader: R) -> Result<ActionPartition, MercError> {
    let mut tokens = Vec::new();
    for (line_no, line) in BufReader::new(reader).lines().enumerate() {
        tokenize_line(&line?, line_no + 1, &mut tokens)?;
    }

    let mut cursor = Cursor {
        tokens: &tokens,
        pos: 0,
    };

    skip_var_section(&mut cursor);

    let mut partition = ActionPartition::default();
    let mut saw_input_section = false;
    let mut saw_output_section = false;

    while let Some(tok) = cursor.peek() {
        let class = match &tok.kind {
            TokKind::Ident(name) if name == "input" => {
                saw_input_section = true;
                cursor.next();
                ActionClass::Input
            }
            TokKind::Ident(name) if name == "output" => {
                saw_output_section = true;
                cursor.next();
                ActionClass::Output
            }
            _ => {
                return Err(PartitionError::Expected {
                    line: tok.line,
                    expected: "`input` or `output`",
                }
                .into());
            }
        };

        while !cursor.at_section_keyword() && cursor.peek().is_some() {
            let pattern = parse_rule(&mut cursor)?;
            match class {
                ActionClass::Input => partition.inputs.push(pattern),
                ActionClass::Output => partition.outputs.push(pattern),
            }
        }
    }

    if !saw_input_section {
        return Err(PartitionError::MissingSection { section: "input" }.into());
    }
    if !saw_output_section {
        return Err(PartitionError::MissingSection { section: "output" }.into());
    }

    for input in &partition.inputs {
        if partition
            .outputs
            .iter()
            .any(|output| output.name == input.name && output.arity == input.arity)
        {
            return Err(PartitionError::NotDisjoint {
                name: input.name.clone(),
                arity: input.arity,
            }
            .into());
        }
    }

    Ok(partition)
}

/// Parses one `rule := (dataexpr "->")? action ";"`, starting at the current
/// cursor position and consuming through the terminating `;`.
fn parse_rule(cursor: &mut Cursor) -> Result<ActionPattern, PartitionError> {
    let start = cursor.pos;
    let line = cursor.peek().map(|t| t.line).unwrap_or(0);

    while !matches!(cursor.peek().map(|t| &t.kind), Some(TokKind::Semicolon) | None) {
        cursor.next();
    }
    let Some(semicolon) = cursor.peek() else {
        return Err(PartitionError::UnexpectedEof { line });
    };
    debug_assert!(matches!(semicolon.kind, TokKind::Semicolon));
    let rule_tokens = &cursor.tokens[start..cursor.pos];
    cursor.next(); // consume `;`

    if let Some(arrow_pos) = rule_tokens.iter().position(|t| matches!(t.kind, TokKind::Arrow)) {
        return Err(PartitionError::GuardedRule {
            line,
            guard: render_tokens(&rule_tokens[..arrow_pos]),
        });
    }

    parse_action_pattern(rule_tokens, line)
}

/// Parses `action := ident ("(" ident ("," ident)* ")")?` from a fully
/// bounded token slice (the tokens of one rule, guard already excluded).
fn parse_action_pattern(tokens: &[Token], line: usize) -> Result<ActionPattern, PartitionError> {
    let mut iter = tokens.iter().peekable();

    let name = match iter.next() {
        Some(Token {
            kind: TokKind::Ident(name),
            ..
        }) => name.clone(),
        _ => {
            return Err(PartitionError::Expected {
                line,
                expected: "an action name",
            });
        }
    };

    let mut args = Vec::new();
    if matches!(iter.peek().map(|t| &t.kind), Some(TokKind::LParen)) {
        iter.next();
        loop {
            match iter.next() {
                Some(Token {
                    kind: TokKind::Ident(arg),
                    ..
                }) => {
                    if args.contains(arg) {
                        return Err(PartitionError::DuplicateArgument { line, arg: arg.clone() });
                    }
                    args.push(arg.clone());
                }
                _ => {
                    return Err(PartitionError::Expected {
                        line,
                        expected: "a variable name",
                    });
                }
            }
            match iter.next() {
                Some(Token {
                    kind: TokKind::Comma, ..
                }) => continue,
                Some(Token {
                    kind: TokKind::RParen, ..
                }) => break,
                _ => {
                    return Err(PartitionError::Expected {
                        line,
                        expected: "`,` or `)`",
                    });
                }
            }
        }
    }

    if iter.next().is_some() {
        return Err(PartitionError::Expected { line, expected: "`;`" });
    }

    Ok(ActionPattern {
        name,
        arity: args.len(),
    })
}

/// Skips an optional `varsec := "var" (ident ":" sort ";")+`. The
/// declarations are not retained: v1 has no use for sorts (conditional
/// rules, the only feature that would need them, are rejected).
fn skip_var_section(cursor: &mut Cursor) {
    if !matches!(cursor.peek().map(|t| &t.kind), Some(TokKind::Ident(name)) if name == "var") {
        return;
    }
    cursor.next();

    while !cursor.at_section_keyword() && cursor.peek().is_some() {
        // variable name
        cursor.next();
        // `:`
        if matches!(cursor.peek().map(|t| &t.kind), Some(TokKind::Colon)) {
            cursor.next();
        }
        // sort, unparsed: skip through the terminating `;`
        while !matches!(cursor.peek().map(|t| &t.kind), Some(TokKind::Semicolon) | None) {
            cursor.next();
        }
        if cursor.peek().is_some() {
            cursor.next(); // consume `;`
        }
    }
}

fn render_tokens(tokens: &[Token]) -> String {
    tokens
        .iter()
        .map(|t| match &t.kind {
            TokKind::Ident(name) | TokKind::Number(name) => name.clone(),
            TokKind::Colon => ":".to_string(),
            TokKind::Semicolon => ";".to_string(),
            TokKind::Comma => ",".to_string(),
            TokKind::LParen => "(".to_string(),
            TokKind::RParen => ")".to_string(),
            TokKind::Arrow => "->".to_string(),
        })
        .collect::<Vec<_>>()
        .join(" ")
}

struct Cursor<'a> {
    tokens: &'a [Token],
    pos: usize,
}

impl Cursor<'_> {
    fn peek(&self) -> Option<&Token> {
        self.tokens.get(self.pos)
    }

    fn next(&mut self) -> Option<&Token> {
        let tok = self.tokens.get(self.pos);
        if tok.is_some() {
            self.pos += 1;
        }
        tok
    }

    /// Whether the cursor is positioned at the `input`/`output` keyword that
    /// starts the next section, i.e. rule parsing within the current section
    /// should stop.
    fn at_section_keyword(&self) -> bool {
        matches!(
            self.peek().map(|t| &t.kind),
            Some(TokKind::Ident(name)) if name == "input" || name == "output"
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum TokKind {
    Ident(String),
    /// A numeric literal. Never valid where the grammar expects an
    /// identifier (an action name or a variable-name argument); kept as its
    /// own token kind purely so that case produces a precise "expected a
    /// variable name" parse error instead of a tokeniser-level failure.
    Number(String),
    Colon,
    Semicolon,
    Comma,
    LParen,
    RParen,
    Arrow,
}

#[derive(Debug, Clone)]
struct Token {
    kind: TokKind,
    line: usize,
}

/// Tokenizes one line of input, appending to `tokens`. Comments (`%` to end
/// of line) and tokens never span lines, so the tokenizer can run a line at
/// a time without look-ahead across line boundaries.
fn tokenize_line(line_text: &str, line: usize, tokens: &mut Vec<Token>) -> Result<(), PartitionError> {
    let mut chars = line_text.chars().peekable();

    while let Some(&c) = chars.peek() {
        match c {
            c if c.is_whitespace() => {
                chars.next();
            }
            '%' => break,
            '(' => {
                chars.next();
                tokens.push(Token {
                    kind: TokKind::LParen,
                    line,
                });
            }
            ')' => {
                chars.next();
                tokens.push(Token {
                    kind: TokKind::RParen,
                    line,
                });
            }
            ',' => {
                chars.next();
                tokens.push(Token {
                    kind: TokKind::Comma,
                    line,
                });
            }
            ';' => {
                chars.next();
                tokens.push(Token {
                    kind: TokKind::Semicolon,
                    line,
                });
            }
            ':' => {
                chars.next();
                tokens.push(Token {
                    kind: TokKind::Colon,
                    line,
                });
            }
            '-' => {
                chars.next();
                if chars.peek() == Some(&'>') {
                    chars.next();
                    tokens.push(Token {
                        kind: TokKind::Arrow,
                        line,
                    });
                } else {
                    return Err(PartitionError::UnexpectedChar { line, ch: '-' });
                }
            }
            c if c.is_alphabetic() || c == '_' => {
                let mut ident = String::new();
                while let Some(&c) = chars.peek() {
                    if c.is_alphanumeric() || c == '_' || c == '\'' {
                        ident.push(c);
                        chars.next();
                    } else {
                        break;
                    }
                }
                tokens.push(Token {
                    kind: TokKind::Ident(ident),
                    line,
                });
            }
            c if c.is_ascii_digit() => {
                let mut number = String::new();
                while let Some(&c) = chars.peek() {
                    if c.is_ascii_digit() {
                        number.push(c);
                        chars.next();
                    } else {
                        break;
                    }
                }
                tokens.push(Token {
                    kind: TokKind::Number(number),
                    line,
                });
            }
            other => return Err(PartitionError::UnexpectedChar { line, ch: other }),
        }
    }

    Ok(())
}

/// Failure modes of [`parse_partition`] and [`ActionPartition::validate_against_lps`].
///
/// These are startup-time failures, surfaced via `report_error` before any
/// adapter connection exists — never a wire error, so this stays a plain
/// [`MercError`]-convertible type rather than a [`crate::error::MbtError`]
/// variant.
#[derive(Debug, thiserror::Error)]
enum PartitionError {
    #[error("partition file line {line}: unexpected character `{ch}`")]
    UnexpectedChar { line: usize, ch: char },

    #[error("partition file line {line}: unexpected end of file")]
    UnexpectedEof { line: usize },

    #[error("partition file line {line}: expected {expected}")]
    Expected { line: usize, expected: &'static str },

    #[error(
        "partition file line {line}: conditional rules are not supported yet (guard `{guard}`); \
         classify by action name only, or drop the guard"
    )]
    GuardedRule { line: usize, guard: String },

    #[error("partition file line {line}: argument `{arg}` is used more than once in the same action pattern")]
    DuplicateArgument { line: usize, arg: String },

    #[error("partition file has no `{section}` section")]
    MissingSection { section: &'static str },

    #[error("action `{name}/{arity}` is classified as both an input and an output")]
    NotDisjoint { name: String, arity: usize },

    #[error("the LPS has an action `{name}/{arity}` that the partition file does not classify as input or output")]
    Uncovered { name: String, arity: usize },

    #[error("a summand's multi-action mixes input- and output-classified actions")]
    MixedMultiAction,
}

impl fmt::Display for ActionClass {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ActionClass::Input => write!(f, "input"),
            ActionClass::Output => write!(f, "output"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::ActionClass;
    use super::parse_partition;

    #[test]
    fn parses_spec_example_minus_guards() {
        let text = "var v: Nat; w:Bool;\ninput\n  b(w);\noutput\n  c;\n";
        let partition = parse_partition(text.as_bytes()).unwrap();
        assert_eq!(partition.classify("b", 1), Some(ActionClass::Input));
        assert_eq!(partition.classify("c", 0), Some(ActionClass::Output));
        assert_eq!(partition.classify("unknown", 0), None);
    }

    #[test]
    fn skips_comments() {
        let text = "input\n  a; % an input action\noutput\n  b;\n";
        let partition = parse_partition(text.as_bytes()).unwrap();
        assert_eq!(partition.classify("a", 0), Some(ActionClass::Input));
        assert_eq!(partition.classify("b", 0), Some(ActionClass::Output));
    }

    #[test]
    fn distinguishes_by_arity() {
        let text = "input\n  a;\n  a(x);\noutput\n  b;\n";
        let partition = parse_partition(text.as_bytes()).unwrap();
        assert_eq!(partition.classify("a", 0), Some(ActionClass::Input));
        assert_eq!(partition.classify("a", 1), Some(ActionClass::Input));
    }

    #[test]
    fn rejects_guarded_rule() {
        let text = "var w: Bool;\ninput\n  w -> a(w);\noutput\n  b;\n";
        let err = parse_partition(text.as_bytes()).unwrap_err();
        assert!(err.to_string().contains("conditional rules are not supported"), "{err}");
    }

    #[test]
    fn rejects_duplicate_action_across_sections() {
        let text = "input\n  a;\noutput\n  a;\n";
        let err = parse_partition(text.as_bytes()).unwrap_err();
        assert!(err.to_string().contains("both an input and an output"), "{err}");
    }

    #[test]
    fn rejects_non_variable_argument() {
        let text = "input\n  a(3);\noutput\n  b;\n";
        let err = parse_partition(text.as_bytes()).unwrap_err();
        assert!(err.to_string().contains("expected a variable name"), "{err}");
    }

    #[test]
    fn rejects_duplicate_argument_name() {
        let text = "input\n  a(x, x);\noutput\n  b;\n";
        let err = parse_partition(text.as_bytes()).unwrap_err();
        assert!(err.to_string().contains("more than once"), "{err}");
    }

    #[test]
    fn rejects_missing_output_section() {
        let text = "input\n  a;\n";
        let err = parse_partition(text.as_bytes()).unwrap_err();
        assert!(err.to_string().contains("no `output` section"), "{err}");
    }

    #[test]
    fn rejects_missing_input_section() {
        let text = "output\n  a;\n";
        let err = parse_partition(text.as_bytes()).unwrap_err();
        assert!(err.to_string().contains("no `input` section"), "{err}");
    }
}
