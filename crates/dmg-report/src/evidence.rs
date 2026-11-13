use std::{
    fmt::{self, Arguments},
    io::{self, Write},
};

use super::{html, markdown};

/// Human review formats shared by the schema comparison and rehearsal.
#[derive(Debug, Clone, Copy)]
pub enum EvidenceFormat {
    Markdown,
    Html,
}

const MAX_LINE_BYTES: usize = 640;
const MAX_DETAILS: usize = 20;

pub(super) struct Review<W> {
    output: W,
    /// A missing format preserves the native text view used by dmg diff.
    format: Option<EvidenceFormat>,
}

impl<W: Write> Review<W> {
    pub(super) fn new(output: W, format: Option<EvidenceFormat>) -> Self {
        Self { output, format }
    }

    pub(super) fn start(&mut self, title: &'static str) -> io::Result<()> {
        match self.format {
            Some(EvidenceFormat::Markdown) => {
                writeln!(self.output, "## Database Migration Guard · {title}\n")
            }
            Some(EvidenceFormat::Html) => {
                writeln!(self.output, "{}\n<h2>{title}</h2>", html::PAGE_START)
            }
            None => Ok(()),
        }
    }

    pub(super) fn finish(mut self) -> io::Result<()> {
        if matches!(self.format, Some(EvidenceFormat::Html)) {
            writeln!(self.output, "{}", html::PAGE_END)?;
        }
        Ok(())
    }

    pub(super) fn section(&mut self, title: &'static str) -> io::Result<()> {
        match self.format {
            Some(EvidenceFormat::Markdown) => writeln!(self.output, "\n### {title}\n"),
            Some(EvidenceFormat::Html) => writeln!(self.output, "<h3>{title}</h3>"),
            None => Ok(()),
        }
    }

    pub(super) fn summary(
        &mut self,
        title: &'static str,
        content: Arguments<'_>,
    ) -> io::Result<()> {
        if self.format.is_none() {
            self.line(format_args!("{title} — {content}"))
        } else {
            self.section(title)?;
            self.line(content)
        }
    }

    pub(super) fn line(&mut self, content: Arguments<'_>) -> io::Result<()> {
        match self.format {
            Some(EvidenceFormat::Markdown) => {
                writeln!(
                    self.output,
                    "{}\n",
                    markdown::escape_prose(&bounded(content))
                )
            }
            Some(EvidenceFormat::Html) => {
                writeln!(self.output, "<p>{}</p>", html::text(&bounded(content)))
            }
            None => writeln!(self.output, "{content}"),
        }
    }

    pub(super) fn detail(&mut self, content: Arguments<'_>) -> io::Result<()> {
        match self.format {
            Some(EvidenceFormat::Markdown) => {
                writeln!(
                    self.output,
                    "- {}",
                    markdown::escape_prose(&bounded(content))
                )
            }
            Some(EvidenceFormat::Html) => writeln!(
                self.output,
                "<p class=\"evidence-detail\">{}</p>",
                html::text(&bounded(content))
            ),
            None => writeln!(self.output, "  {content}"),
        }
    }

    pub(super) fn details<T>(
        &mut self,
        mut entries: impl Iterator<Item = T>,
        mut render: impl FnMut(&mut Self, T) -> io::Result<()>,
    ) -> io::Result<()> {
        for entry in entries.by_ref().take(MAX_DETAILS) {
            render(self, entry)?;
        }
        self.omitted(entries.count(), "entries")
    }

    pub(super) fn omitted(&mut self, count: usize, label: &'static str) -> io::Result<()> {
        if count > 0 {
            self.detail(format_args!("{count} more {label} in JSON output."))?;
        }
        Ok(())
    }
}

fn bounded(content: Arguments<'_>) -> String {
    struct Line(String);
    impl fmt::Write for Line {
        fn write_str(&mut self, text: &str) -> fmt::Result {
            let limit = MAX_LINE_BYTES - self.0.len();
            if text.len() <= limit {
                self.0.push_str(text);
                return Ok(());
            }
            self.0.push_str(&text[..text.floor_char_boundary(limit)]);
            Err(fmt::Error)
        }
    }

    let mut line = Line(String::new());
    if fmt::write(&mut line, content).is_err() {
        line.0.push('…');
    }
    line.0
}
