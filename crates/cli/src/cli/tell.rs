//! What a command says to a program: one JSON object on each line of
//! standard output, when it is run with `--json`.
//!
//! The shape of these lines is part of the tool's interface. Every line
//! carries `v`, the version of the shape, and `event`, which says what
//! kind of line it is. A field is added without changing `v`. Taking one
//! away, or changing what one means, changes it. docs/commands.md describes
//! each line.

use serde::Serialize;

/// The version of the shape of the lines.
const VERSION: u8 = 1;

/// One line.
#[derive(Debug, Serialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub(super) enum Line<'a> {
    /// A copy begins, and this is what there is to copy.
    Start {
        source: &'a str,
        destination: &'a str,
        bytes: u64,
        files: u64,
    },
    /// So many of the bytes are at the destination.
    Progress { bytes: u64, of: u64 },
    /// A file is whole and has its final name.
    File { path: &'a str },
    /// A copy is finished.
    Done {
        path: &'a str,
        bytes: u64,
        checked: &'a str,
        fetched: u64,
        kept: u64,
    },
    /// What a copy would do, from a dry run.
    Plan {
        source: &'a str,
        destination: &'a str,
        path: &'a str,
        bytes: u64,
        files: u64,
        checked: &'a str,
    },
    /// What is at a place.
    Probe {
        place: &'a str,
        name: &'a str,
        kind: &'a str,
        bytes: u64,
        files: u64,
        checked: &'a str,
        parts: bool,
        digests: Vec<Digest>,
    },
    /// The command failed. This is its last line.
    Error {
        code: u8,
        kind: &'a str,
        message: &'a str,
        hint: Option<&'a str>,
    },
}

/// The digest known of one file.
#[derive(Debug, Serialize)]
pub(super) struct Digest {
    /// The file's path under the place, with `/` between its parts. Empty
    /// if the place is the file.
    pub path: String,
    /// The digest, as `--hash` takes it.
    pub digest: String,
}

/// A line with the version of its shape in front.
#[derive(Serialize)]
struct Versioned<'a> {
    v: u8,
    #[serde(flatten)]
    line: &'a Line<'a>,
}

/// The line as it is written.
pub(super) fn json(line: &Line<'_>) -> String {
    let line = Versioned { v: VERSION, line };
    // Every field is a number, a string, or a list of them, which cannot
    // fail to be written.
    serde_json::to_string(&line).unwrap_or_default()
}

/// Writes the line to standard output.
pub(super) fn say(line: &Line<'_>) {
    println!("{}", json(line));
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The shape of every line, as a program that reads them relies on it.
    #[test]
    fn the_lines_are_written_as_documented() {
        let lines = [
            (
                Line::Start {
                    source: "https://example.org/a.bin",
                    destination: "./out/",
                    bytes: 5,
                    files: 1,
                },
                r#"{"v":1,"event":"start","source":"https://example.org/a.bin","destination":"./out/","bytes":5,"files":1}"#,
            ),
            (
                Line::Progress { bytes: 3, of: 5 },
                r#"{"v":1,"event":"progress","bytes":3,"of":5}"#,
            ),
            (
                Line::File { path: "out/a.bin" },
                r#"{"v":1,"event":"file","path":"out/a.bin"}"#,
            ),
            (
                Line::Done {
                    path: "out/a.bin",
                    bytes: 5,
                    checked: "verified",
                    fetched: 2,
                    kept: 3,
                },
                r#"{"v":1,"event":"done","path":"out/a.bin","bytes":5,"checked":"verified","fetched":2,"kept":3}"#,
            ),
            (
                Line::Plan {
                    source: "./a",
                    destination: "./out/",
                    path: "./out/a",
                    bytes: 5,
                    files: 2,
                    checked: "consistent",
                },
                r#"{"v":1,"event":"plan","source":"./a","destination":"./out/","path":"./out/a","bytes":5,"files":2,"checked":"consistent"}"#,
            ),
            (
                Line::Probe {
                    place: "./a",
                    name: "a",
                    kind: "folder",
                    bytes: 5,
                    files: 2,
                    checked: "consistent",
                    parts: true,
                    digests: vec![Digest {
                        path: "sub/b".into(),
                        digest: "sha256:00".into(),
                    }],
                },
                r#"{"v":1,"event":"probe","place":"./a","name":"a","kind":"folder","bytes":5,"files":2,"checked":"consistent","parts":true,"digests":[{"path":"sub/b","digest":"sha256:00"}]}"#,
            ),
            (
                Line::Error {
                    code: 4,
                    kind: "unreachable",
                    message: "it said \"no\"\n",
                    hint: None,
                },
                r#"{"v":1,"event":"error","code":4,"kind":"unreachable","message":"it said \"no\"\n","hint":null}"#,
            ),
        ];
        for (line, written) in lines {
            assert_eq!(json(&line), written);
        }
    }
}
