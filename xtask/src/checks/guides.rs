// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! Keeps what a guide in `docs/guides/` shows its reader true to the tree.
//!
//! A guide marks a block with an HTML comment on the line before its fence, which a rendered page
//! does not show. `guide:command` holds commands the reader types; each `cargo xtask new` among them
//! must plan as `--dry-run` plans it. `guide:verify` holds what proves the result, and may hold only
//! `cargo test`, `cargo check` and `cargo xtask` commands. `guide:excerpt <path>` quotes code that
//! must still be in `<path>`, contiguous and verbatim under one shared indent; nothing is marked in
//! the source, so a guide can quote `pos-core` or `pos-proto` without changing either. An unknown or
//! dangling marker is a finding, so a typo cannot turn a check off unseen.

use super::{Error, repo_root};
use crate::Finding;

const RULE: &str = "guide";

/// Where the guides live, relative to the repository root.
const GUIDES_DIR: &str = "docs/guides";

/// The commands a `guide:verify` block may hold.
const VERIFY_COMMANDS: [&str; 3] = ["cargo test", "cargo check", "cargo xtask"];

/// What a marked block asks of the tree: commands the reader types, commands that prove the
/// result, or code quoted from the file at a repository-relative path.
#[derive(Debug, PartialEq, Eq)]
enum Kind {
    Command,
    Verify,
    Excerpt(String),
}

/// One marked block: its kind, the one-based line of its marker, and the lines between its fences.
#[derive(Debug)]
struct Block {
    kind: Kind,
    line: u32,
    body: Vec<String>,
}

/// Checks every marked block of every guide.
///
/// # Errors
///
/// [`Error`] if `docs/guides/` or a guide cannot be read.
pub fn run(_args: &[String]) -> Result<Vec<Finding>, Error> {
    let root = repo_root();
    let mut findings = Vec::new();
    let mut paths: Vec<_> = std::fs::read_dir(root.join(GUIDES_DIR))?
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.extension().is_some_and(|extension| extension == "md"))
        .collect();
    paths.sort();
    for path in paths {
        let text = std::fs::read_to_string(&path)?;
        let file = path
            .strip_prefix(&root)
            .unwrap_or(&path)
            .display()
            .to_string();
        findings.extend(check_guide(
            &file,
            &text,
            |args| {
                crate::scaffold::plan(&root, args)
                    .map(|_| ())
                    .map_err(|error| error.to_string())
            },
            |quoted| std::fs::read_to_string(root.join(quoted)).ok(),
        ));
    }
    Ok(findings)
}

/// The findings for one guide, `file`, whose text is `text`.
///
/// `plan` plans the arguments after `cargo xtask new`, and `read` reads a repository-relative path,
/// so the rules can be tested without the tree.
fn check_guide(
    file: &str,
    text: &str,
    plan: impl Fn(&[String]) -> Result<(), String>,
    read: impl Fn(&str) -> Option<String>,
) -> Vec<Finding> {
    let (blocks, mut findings) = parse(file, text);
    for block in &blocks {
        let finding = |message: String| Finding::new(file, RULE, message).at_line(block.line);
        match &block.kind {
            Kind::Command => {
                for command in commands(&block.body) {
                    let words = words(&command);
                    if words.get(..3) == Some(&["cargo", "xtask", "new"].map(String::from)[..])
                        && let Err(refusal) = plan(words.get(3..).unwrap_or_default())
                    {
                        findings.push(
                            finding(format!("`{command}` is refused: {refusal}")).with_hint(
                                "make the guide's command one the generator accepts today",
                            ),
                        );
                    }
                }
            }
            Kind::Verify => {
                for command in commands(&block.body) {
                    if !VERIFY_COMMANDS.iter().any(|allowed| {
                        command == *allowed || command.starts_with(&format!("{allowed} "))
                    }) {
                        findings.push(finding(format!(
                            "`{command}` is not a `cargo test`, `cargo check` or `cargo xtask` command"
                        )));
                    }
                }
            }
            Kind::Excerpt(quoted) => match read(quoted) {
                None => findings.push(finding(format!("quotes `{quoted}`, which does not exist"))),
                Some(source) if !quotes(&source, &block.body) => findings.push(
                    finding(format!("the excerpt is no longer in `{quoted}`, verbatim"))
                        .with_hint("copy the code as it now reads, or quote what replaced it"),
                ),
                Some(_) => {}
            },
        }
    }
    findings
}

/// The marked blocks of `text`, and a finding for each marker that is unknown or has no fence after
/// it.
fn parse(file: &str, text: &str) -> (Vec<Block>, Vec<Finding>) {
    let lines: Vec<&str> = text.lines().collect();
    let (mut blocks, mut findings) = (Vec::new(), Vec::new());
    let mut at = 0;
    while let Some(line) = lines.get(at) {
        let line_number = u32::try_from(at + 1).unwrap_or(u32::MAX);
        at += 1;
        let refuse = |message: &str| Finding::new(file, RULE, message).at_line(line_number);
        let kind = match marker(line) {
            None => continue,
            Some(Err(unknown)) => {
                findings.push(
                    refuse(&format!("unknown marker `guide:{unknown}`"))
                        .with_hint("use guide:command, guide:verify or guide:excerpt <path>"),
                );
                continue;
            }
            Some(Ok(kind)) => kind,
        };
        let fence = lines.get(at).copied().unwrap_or_default();
        let indent = fence.len() - fence.trim_start().len();
        if !fence.trim_start().starts_with("```") {
            findings.push(refuse("the marker is not followed by a fenced block"));
            continue;
        }
        let Some(close) = lines
            .iter()
            .skip(at + 1)
            .position(|candidate| candidate.trim_start().starts_with("```"))
        else {
            findings.push(refuse("the fenced block is never closed"));
            break;
        };
        let body = lines.iter().skip(at + 1).take(close);
        let body = body.map(|body_line| body_line.get(indent..).unwrap_or(body_line.trim_start()));
        blocks.push(Block {
            kind,
            line: line_number,
            body: body.map(str::to_owned).collect(),
        });
        at += close + 2;
    }
    (blocks, findings)
}

/// What a `<!-- guide:… -->` line asks for, the marker's text if it is not one this check knows, or
/// `None` for a line that is no marker.
fn marker(line: &str) -> Option<Result<Kind, String>> {
    let marker = line
        .trim()
        .strip_prefix("<!-- guide:")?
        .strip_suffix("-->")?
        .trim();
    Some(match marker.split_once(' ') {
        None if marker == "command" => Ok(Kind::Command),
        None if marker == "verify" => Ok(Kind::Verify),
        Some(("excerpt", path)) => Ok(Kind::Excerpt(path.trim().to_owned())),
        _ => Err(marker.to_owned()),
    })
}

/// A block's commands: a line ending in `\` joined to the next, and comments and blanks left out.
fn commands(body: &[String]) -> Vec<String> {
    let joined = body.join("\n").replace("\\\n", " ");
    joined
        .lines()
        .map(|line| line.split_whitespace().collect::<Vec<_>>().join(" "))
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .collect()
}

/// `command` split on blanks and quotes, as a shell would; it has no variables, globs or escapes.
fn words(command: &str) -> Vec<String> {
    let (mut words, mut word, mut quote) = (Vec::new(), None::<String>, None);
    for character in command.chars() {
        match (quote, character) {
            (Some(open), _) if character == open => quote = None,
            (None, '"' | '\'') => {
                quote = Some(character);
                word.get_or_insert_default();
            }
            (None, ' ' | '\t') => words.extend(word.take()),
            _ => word.get_or_insert_default().push(character),
        }
    }
    words.extend(word);
    words
}

/// Whether `source` holds `excerpt`'s lines, contiguous and verbatim, each under the same indent.
fn quotes(source: &str, excerpt: &[String]) -> bool {
    let lines: Vec<&str> = source.lines().collect();
    let Some(first) = excerpt.iter().find(|line| !line.is_empty()) else {
        return false;
    };
    let offset = excerpt.iter().position(|line| line == first).unwrap_or(0);
    (offset..lines.len()).any(|start| {
        let indent = lines
            .get(start)
            .and_then(|line| line.strip_suffix(first.as_str()));
        let Some(indent) = indent.filter(|indent| indent.trim().is_empty()) else {
            return false;
        };
        excerpt.iter().enumerate().all(|(index, line)| {
            lines.get(start - offset + index).is_some_and(|actual| {
                if line.is_empty() {
                    actual.trim().is_empty()
                } else {
                    actual.strip_prefix(indent) == Some(line.as_str())
                }
            })
        })
    })
}

#[cfg(test)]
mod tests {
    use super::{Kind, check_guide, parse, words};

    const SOURCE: &str = "fn main() {\n    let total = 1;\n\n    println!(\"{total}\");\n}\n";

    /// Each finding of `text`, as `line: message` lines, with a planner that accepts only `crate`.
    fn findings(text: &str) -> String {
        let plan = |args: &[String]| match args.first().map(String::as_str) {
            Some("crate") => Ok(()),
            other => Err(format!("no generator {other:?}")),
        };
        let read = |path: &str| (path == "src/main.rs").then(|| SOURCE.to_owned());
        let found = check_guide("docs/guides/example.md", text, plan, read);
        let lines: Vec<String> = found
            .iter()
            .map(|finding| format!("{}: {}", finding.line.unwrap_or(0), finding.message))
            .collect();
        lines.join("\n")
    }

    #[test]
    fn a_command_the_generator_accepts_passes_and_one_it_refuses_is_named() {
        let text = "<!-- guide:command -->\n```bash\ncargo xtask new crate pos-x \\\n  --purpose \"A crate\"\ngit status\n```\n\
                    <!-- guide:command -->\n```bash\ncargo xtask new widget --dry-run\n```\n";
        let refused =
            "7: `cargo xtask new widget --dry-run` is refused: no generator Some(\"widget\")";
        assert_eq!(findings(text), refused);
    }

    #[test]
    fn a_verify_block_holds_only_cargo_test_check_and_xtask() {
        let text = "<!-- guide:verify -->\n```bash\n# proves it\ncargo test -p pos-x\ncargo xtask crates\nrm -rf target\n```\n";
        let refused =
            "1: `rm -rf target` is not a `cargo test`, `cargo check` or `cargo xtask` command";
        assert_eq!(findings(text), refused);
    }

    #[test]
    fn an_excerpt_must_be_in_its_file_verbatim_under_one_indent() {
        let excerpt = |path: &str, code: &str| {
            format!("<!-- guide:excerpt {path} -->\n```rust\n{code}\n```\n")
        };
        let dedented = excerpt("src/main.rs", "let total = 1;\n\nprintln!(\"{total}\");");
        assert_eq!(findings(&dedented), "", "dedented once is still verbatim");
        // Changed, cut mid-line, or indented unlike its source.
        for wrong in [
            "let total = 2;",
            "total = 1;",
            "let total = 1;\n\n  println!(\"{total}\");",
        ] {
            let found = findings(&excerpt("src/main.rs", wrong));
            assert_eq!(
                found, "1: the excerpt is no longer in `src/main.rs`, verbatim",
                "{wrong}"
            );
        }
        let missing = excerpt("src/gone.rs", "let total = 1;");
        assert_eq!(
            findings(&missing),
            "1: quotes `src/gone.rs`, which does not exist"
        );
    }

    #[test]
    fn a_mistyped_or_dangling_marker_is_a_finding_not_a_silent_skip() {
        let text = "<!-- guide:comand -->\n```bash\ncargo test\n```\n<!-- guide:verify -->\nprose\n<!-- guide:excerpt -->\n";
        let found = "1: unknown marker `guide:comand`\n5: the marker is not followed by a fenced block\n\
                     7: unknown marker `guide:excerpt`";
        assert_eq!(findings(text), found);
    }

    #[test]
    fn a_block_inside_a_list_item_is_read_under_its_indent() {
        let (blocks, findings) = parse(
            "g.md",
            "1. Run it.\n   <!-- guide:verify -->\n   ```bash\n   cargo test -p x\n   ```\n",
        );
        assert!(findings.is_empty());
        assert_eq!(blocks.first().map(|block| &block.kind), Some(&Kind::Verify));
        assert_eq!(
            blocks.first().map(|block| block.body.clone()),
            Some(vec!["cargo test -p x".to_owned()])
        );
    }

    #[test]
    fn words_split_as_a_shell_splits_them() {
        let split =
            words("cargo xtask new country xa --display-name \"Example country\" --x 'a b'");
        assert_eq!(
            split.join("|"),
            "cargo|xtask|new|country|xa|--display-name|Example country|--x|a b"
        );
    }
}
