//! Console output from an evaluation — `echo` lines and warnings, in a SINGLE ordered log.
//!
//! echo and warnings interleave in OpenSCAD's console, and I.5's string-equal-vs-oracle gate needs
//! that order preserved (the determinism doctrine's "buffered echo/warning order"). So one
//! `Vec<Message>` in emission order, not two side buffers. Warning TEXT bug-for-bug is a follow-on —
//! the [`Message::Warning`] variant exists now so the ordering is right the day it lands.

use crate::Mesh;

/// One line of console output, carrying the CONTENT (what follows the `ECHO: ` / `WARNING: ` prefix).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Message {
    /// An `echo(...)` line's content, already formatted (`a = 5, "hi", [1, 2, 3]`).
    Echo(String),
    /// A warning's content. Emitted, but not (yet) matched to the oracle word-for-word.
    Warning(String),
}

impl Message {
    /// The full console line as OpenSCAD prints it — `ECHO: …` / `WARNING: …`.
    #[must_use]
    pub fn render(&self) -> String {
        match self {
            Message::Echo(s) => format!("ECHO: {s}"),
            Message::Warning(s) => format!("WARNING: {s}"),
        }
    }
}

/// A full evaluation: the geometry PLUS the ordered console messages. Returned by the `*_full` entry
/// points; the plain `evaluate*` sugar drops the messages and hands back just the [`Mesh`].
#[derive(Debug, Clone)]
pub struct Evaluation {
    /// The rendered mesh.
    pub mesh: Mesh,
    /// Console output (echo + warnings) in emission order.
    pub messages: Vec<Message>,
}

impl Evaluation {
    /// The echo CONTENTS in order, warnings dropped — the common assertion
    /// (`assert_eq!(ev.echos(), ["9", "0.333333"])`).
    #[must_use]
    pub fn echos(&self) -> Vec<&str> {
        self.messages
            .iter()
            .filter_map(|m| match m {
                Message::Echo(s) => Some(s.as_str()),
                Message::Warning(_) => None,
            })
            .collect()
    }

    /// The warning CONTENTS in order, echo dropped.
    #[must_use]
    pub fn warnings(&self) -> Vec<&str> {
        self.messages
            .iter()
            .filter_map(|m| match m {
                Message::Warning(s) => Some(s.as_str()),
                Message::Echo(_) => None,
            })
            .collect()
    }

    /// Every message as its full console line (`ECHO: …` / `WARNING: …`), in order — for a whole-console
    /// comparison against the oracle's captured output.
    #[must_use]
    pub fn console(&self) -> Vec<String> {
        self.messages.iter().map(Message::render).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::{Evaluation, Message};
    use crate::Mesh;

    #[test]
    fn helpers_split_and_render_the_message_log() {
        let ev = Evaluation {
            mesh: Mesh::new(),
            messages: vec![
                Message::Echo("9".to_string()),
                Message::Warning("\"x\" was overwritten".to_string()),
                Message::Echo("0.333333".to_string()),
            ],
        };
        assert_eq!(ev.echos(), ["9", "0.333333"]); // echo contents, warnings dropped, in order
        assert_eq!(ev.warnings(), ["\"x\" was overwritten"]);
        assert_eq!(
            ev.console(),
            [
                "ECHO: 9",
                "WARNING: \"x\" was overwritten",
                "ECHO: 0.333333",
            ]
        );
        assert_eq!(Message::Echo("a = 5".to_string()).render(), "ECHO: a = 5");
    }
}
