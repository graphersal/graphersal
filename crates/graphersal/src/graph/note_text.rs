//! The one layout of the notes the engine writes for people: the notes of a store upgrade
//! (`UpgradeReport::vector_notes`, `MissingModel::note`), of a `.gsnap` loaded without a store
//! (`SnapshotLoadReport::notes`), of a saved query (`RemovedFunctionUse::note`,
//! `VectorPropertyWrite::note`) and the refusal of an open whose model is missing.
//!
//! A note is a short headline on its first line; every further line is indented by
//! [`INDENT`]: a detail (`key: value`, one per line), or a shell command on a line of its own
//! under a one-line `# comment` saying what it does (never several commands in one line). A
//! front end prints the headline after its own prefix (`note: `, `warning: `) and the other
//! lines as they are, and separates two notes by a blank line.

/// The indentation of every line of a note after its headline.
pub(crate) const INDENT: &str = "  ";

/// A note being written ([`NoteText::finish`] gives its text).
#[derive(Debug, Clone)]
pub(crate) struct NoteText {
    text: String,
}

impl NoteText {
    /// A note whose headline is `headline` (one line).
    pub(crate) fn new(headline: impl AsRef<str>) -> Self {
        NoteText {
            text: headline.as_ref().to_string(),
        }
    }

    /// Adds the detail line `key: value`.
    pub(crate) fn detail(mut self, key: &str, value: impl AsRef<str>) -> Self {
        self.text.push('\n');
        self.text.push_str(INDENT);
        self.text.push_str(key);
        self.text.push_str(": ");
        self.text.push_str(value.as_ref());
        self
    }

    /// Adds the shell command `command` on its own line(s), under the one-line comment
    /// `comment` saying what it does. A command of several lines (a long schema patch inside
    /// its quotes) keeps them, each indented.
    pub(crate) fn command(mut self, comment: &str, command: &str) -> Self {
        self.text.push('\n');
        self.text.push_str(INDENT);
        self.text.push_str("# ");
        self.text.push_str(comment);
        for line in command.lines() {
            self.text.push('\n');
            self.text.push_str(INDENT);
            self.text.push_str(line);
        }
        self
    }

    /// Adds every `(comment, command)` of `steps` ([`Self::command`]).
    pub(crate) fn commands(self, steps: &[(String, String)]) -> Self {
        steps.iter().fold(self, |note, (comment, command)| {
            note.command(comment, command)
        })
    }

    /// The text of the note.
    pub(crate) fn finish(self) -> String {
        self.text
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_note_is_a_headline_with_indented_details_and_commented_commands() {
        let text = NoteText::new("the model acme/tiny is missing")
            .detail("needed by", "doc.emb")
            .command(
                "download the model",
                "graphersal embed-model download acme/tiny",
            )
            .command("run the patch", "g.patch_schema(#{\n  vertices: #{}\n})")
            .finish();
        assert_eq!(
            text,
            "the model acme/tiny is missing\n  needed by: doc.emb\n  # download the model\n  \
             graphersal embed-model download acme/tiny\n  # run the patch\n  \
             g.patch_schema(#{\n    vertices: #{}\n  })"
        );
    }
}
