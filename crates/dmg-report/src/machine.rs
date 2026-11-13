use std::io;

use dmg_core::{report::ReportV1, source::RepoPath};

const MAX_PATH_BYTES: usize = 1024;
const MAX_MESSAGE_BYTES: usize = 640;

pub(super) fn validate_locations(report: &ReportV1) -> io::Result<()> {
    if report
        .findings()
        .iter()
        .any(|finding| !valid_path(&finding.span.path))
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "finding source path exceeds the review format bounds",
        ));
    }
    Ok(())
}

pub(super) fn valid_path(path: &RepoPath) -> bool {
    let path = path.as_str();
    path.len() <= MAX_PATH_BYTES && !path.chars().any(char::is_control)
}

pub(super) fn message(value: &str, fallback: &str) -> String {
    let text = super::bounded_line(value, MAX_MESSAGE_BYTES);
    if text.trim().is_empty() {
        fallback.to_owned()
    } else {
        text
    }
}
