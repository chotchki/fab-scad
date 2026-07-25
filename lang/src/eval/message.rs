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
    /// A fault's content, rendered `ERROR: …` as upstream prints it. Always LAST when present — upstream
    /// prints at most one and nothing after it.
    ///
    /// Two producers, matching upstream's two ways of reaching an `ERROR:` line:
    /// [`Failure::console`](crate::Failure::console) synthesizes it for a fault that ABORTED the run, and
    /// eval emits one directly for a statement-position `assert`, which reports an error and halts but
    /// still exports the geometry accumulated before it (L.5.8/AP.7). Both stop the program; they differ
    /// only in whether anything survives to export, which is why one is a `Failure` and one is not.
    Error(String),
}

impl Message {
    /// This message's text if it's an `echo`, else `None` — `msgs.iter().filter_map(Message::echo)`.
    ///
    /// Exists so callers stop hand-writing an exhaustive `match` just to pick one variant out of a
    /// `Vec<Message>`: half a dozen of them did, and every one became a compile error the day `Error`
    /// was added. Selecting through an accessor means the next variant costs them nothing.
    #[must_use]
    pub fn echo(&self) -> Option<&str> {
        match self {
            Message::Echo(s) => Some(s),
            _ => None,
        }
    }

    /// This message's text if it's a warning, else `None`.
    #[must_use]
    pub fn warning(&self) -> Option<&str> {
        match self {
            Message::Warning(s) => Some(s),
            _ => None,
        }
    }

    /// The full console line as OpenSCAD prints it — `ECHO: …` / `WARNING: …` / `ERROR: …`.
    #[must_use]
    pub fn render(&self) -> String {
        match self {
            Message::Echo(s) => format!("ECHO: {s}"),
            Message::Warning(s) => format!("WARNING: {s}"),
            Message::Error(s) => format!("ERROR: {s}"),
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
                Message::Warning(_) | Message::Error(_) => None,
            })
            .collect()
    }

    /// The warning CONTENTS in order, echo dropped. A terminal `Error` is NOT a warning and never appears
    /// here — an [`Evaluation`] only exists on the success path anyway, so it cannot hold one.
    #[must_use]
    pub fn warnings(&self) -> Vec<&str> {
        self.messages
            .iter()
            .filter_map(|m| match m {
                Message::Warning(s) => Some(s.as_str()),
                Message::Echo(_) | Message::Error(_) => None,
            })
            .collect()
    }

    /// The ERROR contents in order — in practice 0 or 1, since a fault stops the run. Non-empty here only
    /// for the non-fatal-but-reported case (a statement-position `assert`, L.5.8/AP.7); a fault that
    /// aborted never produces an [`Evaluation`] at all, it produces a [`Failure`](crate::Failure).
    #[must_use]
    pub fn errors(&self) -> Vec<&str> {
        self.messages
            .iter()
            .filter_map(|m| match m {
                Message::Error(s) => Some(s.as_str()),
                Message::Echo(_) | Message::Warning(_) => None,
            })
            .collect()
    }

    /// Every message as its full console line (`ECHO: …` / `WARNING: …` / `ERROR: …`), in order — for a
    /// whole-console comparison against the oracle's captured output.
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
