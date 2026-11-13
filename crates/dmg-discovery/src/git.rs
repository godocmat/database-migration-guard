use std::{
    ffi::OsStr,
    io::{self, BufRead, BufReader, Read, Write},
    path::Path,
    process::{Child, ChildStdin, ChildStdout, Command, Stdio},
    sync::mpsc::{self, RecvTimeoutError, Sender},
    thread,
    time::{Duration, Instant},
};

use dmg_core::{coverage::CoverageGapKind, source::RepoPath};

use crate::{
    DiscoveryOptions, Inventory, budget_for_file, coverage_gap, record_candidate, valid_path,
};

const GIT_TIMEOUT: Duration = Duration::from_secs(30);
const MAX_TREE_ENTRY: usize = 2048;

/// Validated full Git object IDs for a local base and candidate commit.
#[derive(Debug, Clone)]
pub struct GitRevisions {
    pub(crate) base: String,
    pub(crate) head: String,
}

impl GitRevisions {
    pub fn new(base: &str, head: &str) -> Result<Self, crate::DiscoveryConfigError> {
        if !valid_oid(base) || !valid_oid(head) || base.len() != head.len() {
            return Err(crate::DiscoveryConfigError::InvalidGitRevisions);
        }
        Ok(Self {
            base: base.to_ascii_lowercase(),
            head: head.to_ascii_lowercase(),
        })
    }
}

fn valid_oid(value: &str) -> bool {
    matches!(value.len(), 40 | 64) && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

pub(super) fn collect(
    repository: &Path,
    revision: &str,
    side: &str,
    options: &DiscoveryOptions,
) -> Inventory {
    let mut inventory = Inventory::default();
    if !is_commit(repository, revision) {
        inventory.gaps.push(coverage_gap(
            CoverageGapKind::MissingIntegrityEvidence,
            None,
            &format!("the explicit Git {side} object is unavailable or is not a local commit"),
        ));
        return inventory;
    }
    let Ok(mut batch) = Batch::start(repository) else {
        inventory.gaps.push(coverage_gap(
            CoverageGapKind::MissingIntegrityEvidence,
            None,
            &format!("could not open the local Git object reader for the {side} commit"),
        ));
        return inventory;
    };

    let mut remaining_entries = options.limits.max_entries;
    for root in &options.roots {
        if let Err(gap) = collect_root(
            repository,
            revision,
            root,
            &mut remaining_entries,
            options,
            &mut batch,
            &mut inventory,
        ) {
            inventory
                .gaps
                .push(coverage_gap(gap.kind, Some(root), gap.message));
            if gap.kind == CoverageGapKind::InputLimitExceeded {
                break;
            }
        }
    }
    inventory
}

struct RootGap {
    kind: CoverageGapKind,
    message: &'static str,
}

fn collect_root(
    repository: &Path,
    revision: &str,
    root: &RepoPath,
    remaining_entries: &mut usize,
    options: &DiscoveryOptions,
    batch: &mut Batch,
    inventory: &mut Inventory,
) -> Result<(), RootGap> {
    let failed = || RootGap {
        kind: CoverageGapKind::MissingIntegrityEvidence,
        message: "could not read the migration root in the local Git commit",
    };
    let root_entry = capture(
        repository,
        [
            "ls-tree",
            "-z",
            "--full-tree",
            revision,
            "--",
            root.as_str(),
        ],
        MAX_TREE_ENTRY,
    )
    .map_err(|_| failed())?;
    if root_entry.is_empty() {
        // A Git commit cannot track an empty directory.
        return Ok(());
    }
    let root_entry = root_entry
        .strip_suffix(b"\0")
        .and_then(TreeEntry::parse)
        .ok_or_else(failed)?;
    if root_entry.kind != EntryKind::Tree || root_entry.path != root.as_str().as_bytes() {
        return Err(RootGap {
            kind: CoverageGapKind::InvalidPath,
            message: "the Git migration root must be an ordinary tree",
        });
    }

    let listing_limit = (*remaining_entries + 1)
        .saturating_mul(MAX_TREE_ENTRY)
        .min(32 * 1024 * 1024);
    let listing = capture(
        repository,
        ["ls-tree", "-z", "--full-tree", root_entry.oid],
        listing_limit,
    )
    .map_err(|error| {
        if error.kind() == io::ErrorKind::InvalidData {
            RootGap {
                kind: CoverageGapKind::InputLimitExceeded,
                message: "Git migration directory listing exceeded its byte limit",
            }
        } else {
            failed()
        }
    })?;
    let mut entries = Vec::new();
    for record in listing.split_inclusive(|byte| *byte == 0) {
        let Some(entry) = record.strip_suffix(b"\0").and_then(TreeEntry::parse) else {
            return Err(failed());
        };
        if entries.len() == *remaining_entries {
            return Err(RootGap {
                kind: CoverageGapKind::InputLimitExceeded,
                message: "migration directory entry limit exceeded",
            });
        }
        entries.push(entry);
    }
    *remaining_entries -= entries.len();
    entries.sort_unstable_by(|left, right| left.path.cmp(right.path));
    for entry in entries {
        collect_file(entry, root, options, batch, inventory);
    }
    Ok(())
}

fn collect_file(
    entry: TreeEntry<'_>,
    root: &RepoPath,
    options: &DiscoveryOptions,
    batch: &mut Batch,
    inventory: &mut Inventory,
) {
    if entry.kind == EntryKind::Tree {
        return;
    }
    let Some(filename) = std::str::from_utf8(entry.path)
        .ok()
        .filter(|filename| !filename.contains('/'))
    else {
        inventory.gaps.push(coverage_gap(
            CoverageGapKind::InvalidPath,
            Some(root),
            "Git tree contains a nonportable migration filename",
        ));
        return;
    };
    let Some(path) = RepoPath::new(format!("{root}/{filename}"))
        .ok()
        .filter(valid_path)
    else {
        inventory.gaps.push(coverage_gap(
            CoverageGapKind::InvalidPath,
            Some(root),
            "Git tree contains a nonportable migration filename",
        ));
        return;
    };
    if entry.kind == EntryKind::Submodule {
        inventory.gaps.push(coverage_gap(
            CoverageGapKind::UnsupportedInput,
            Some(&path),
            "Git submodules are outside the migration inventory",
        ));
        return;
    }
    let Some(candidate) = record_candidate(inventory, options, &path, filename) else {
        return;
    };
    if entry.kind != EntryKind::File {
        inventory.gaps.push(coverage_gap(
            CoverageGapKind::InvalidPath,
            Some(&path),
            "Git migration input must be an ordinary blob",
        ));
        return;
    }
    match batch.read_blob(entry.oid, budget_for_file(inventory, options)) {
        Ok(Some(bytes)) => inventory.add(&options.adapter, candidate, path, bytes),
        Ok(None) => inventory.gaps.push(coverage_gap(
            CoverageGapKind::InputLimitExceeded,
            Some(&path),
            "migration input exceeds the available byte budget",
        )),
        Err(_) => inventory.gaps.push(coverage_gap(
            CoverageGapKind::MissingIntegrityEvidence,
            Some(&path),
            "migration blob is unavailable in the local Git object store",
        )),
    }
}

#[derive(Eq, PartialEq)]
enum EntryKind {
    Tree,
    File,
    Symlink,
    Submodule,
}

struct TreeEntry<'a> {
    kind: EntryKind,
    oid: &'a str,
    path: &'a [u8],
}

impl<'a> TreeEntry<'a> {
    fn parse(record: &'a [u8]) -> Option<Self> {
        let tab = record.iter().position(|byte| *byte == b'\t')?;
        let (fields, path) = record.split_at(tab);
        let fields = std::str::from_utf8(fields).ok()?;
        let (mode, fields) = fields.split_once(' ')?;
        let (object_type, oid) = fields.split_once(' ')?;
        if !valid_oid(oid) {
            return None;
        }
        let kind = match (mode, object_type) {
            ("040000", "tree") => EntryKind::Tree,
            ("100644" | "100755", "blob") => EntryKind::File,
            ("120000", "blob") => EntryKind::Symlink,
            ("160000", "commit") => EntryKind::Submodule,
            _ => return None,
        };
        Some(Self {
            kind,
            oid,
            path: &path[1..],
        })
    }
}

fn is_commit(repository: &Path, revision: &str) -> bool {
    if !capture(repository, ["rev-parse", "--show-prefix"], 1024)
        .is_ok_and(|output| output == b"\n" || output == b"\r\n")
    {
        return false;
    }
    let object = format!("{revision}^{{commit}}");
    capture(
        repository,
        ["rev-parse", "--verify", "--end-of-options", &object],
        128,
    )
    .ok()
    .and_then(|output| String::from_utf8(output).ok())
    .is_some_and(|output| output.trim() == revision)
}

fn local_git(repository: &Path) -> Command {
    let mut command = Command::new("git");
    for (name, _) in std::env::vars_os() {
        if name
            .to_string_lossy()
            .to_ascii_uppercase()
            .starts_with("GIT_")
        {
            command.env_remove(name);
        }
    }
    command
        .current_dir(repository)
        .env("GIT_ALLOW_PROTOCOL", "")
        .env("GIT_NO_LAZY_FETCH", "1")
        .env("GIT_OPTIONAL_LOCKS", "0")
        .env("GIT_TERMINAL_PROMPT", "0")
        .args(["--no-replace-objects", "--literal-pathspecs"])
        .stdin(Stdio::null())
        .stderr(Stdio::null());
    command
}

fn capture<I, S>(repository: &Path, args: I, limit: usize) -> io::Result<Vec<u8>>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    let deadline = Instant::now() + GIT_TIMEOUT;
    let mut child = local_git(repository)
        .args(args)
        .stdout(Stdio::piped())
        .spawn()?;
    let stdout = child.stdout.take().expect("Git stdout is piped");
    let (send, receive) = mpsc::sync_channel(1);
    let reader = thread::Builder::new().spawn(move || {
        let mut bytes = Vec::new();
        let read = stdout.take(limit as u64 + 1).read_to_end(&mut bytes);
        let result = read.and_then(|_| {
            if bytes.len() > limit {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "Git output limit exceeded",
                ));
            }
            Ok(bytes)
        });
        let _ = send.send(result);
    });
    if let Err(error) = reader {
        stop(&mut child);
        return Err(error);
    }
    // A descendant holding a pipe cannot extend the command's deadline.
    let bytes = receive
        .recv_timeout(deadline.saturating_duration_since(Instant::now()))
        .map_err(git_receive_error)
        .and_then(std::convert::identity);
    if bytes.is_err() {
        stop(&mut child);
        return bytes;
    }
    if !wait_for_git(&mut child, deadline)?.success() {
        return Err(io::Error::other("Git could not read the local object"));
    }
    bytes
}

fn git_receive_error(error: RecvTimeoutError) -> io::Error {
    match error {
        RecvTimeoutError::Timeout => io::Error::new(
            io::ErrorKind::TimedOut,
            "local Git operation did not finish",
        ),
        RecvTimeoutError::Disconnected => io::Error::other("local Git reader stopped"),
    }
}

fn wait_for_git(child: &mut Child, deadline: Instant) -> io::Result<std::process::ExitStatus> {
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return Ok(status),
            Ok(None) if Instant::now() < deadline => thread::sleep(Duration::from_millis(2)),
            _ => {
                stop(child);
                return Err(git_receive_error(RecvTimeoutError::Timeout));
            }
        }
    }
}

fn stop(child: &mut Child) {
    let _ = child.kill();
    let _ = child.wait();
}

type BlobResult = io::Result<Option<Vec<u8>>>;

struct BlobRequest {
    oid: String,
    budget: usize,
    reply: Sender<BlobResult>,
}

struct Batch {
    child: Child,
    requests: Sender<BlobRequest>,
    deadline: Instant,
    timed_out: bool,
}

impl Batch {
    fn start(repository: &Path) -> io::Result<Self> {
        let mut child = local_git(repository)
            .args(["cat-file", "--batch-command"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()?;
        let mut input = child.stdin.take().expect("Git stdin is piped");
        let mut output = BufReader::new(child.stdout.take().expect("Git stdout is piped"));
        let (requests, receive) = mpsc::channel::<BlobRequest>();
        if let Err(error) = thread::Builder::new().spawn(move || {
            for request in receive {
                let result = read_blob(&mut input, &mut output, &request.oid, request.budget);
                if request.reply.send(result).is_err() {
                    break;
                }
            }
        }) {
            stop(&mut child);
            return Err(error);
        }
        Ok(Self {
            child,
            requests,
            deadline: Instant::now() + GIT_TIMEOUT,
            timed_out: false,
        })
    }

    fn read_blob(&mut self, oid: &str, budget: usize) -> BlobResult {
        if self.timed_out || Instant::now() >= self.deadline {
            return Err(git_receive_error(RecvTimeoutError::Timeout));
        }
        let (reply, response) = mpsc::channel();
        self.requests
            .send(BlobRequest {
                oid: oid.into(),
                budget,
                reply,
            })
            .map_err(|_| git_receive_error(RecvTimeoutError::Disconnected))?;
        match response.recv_timeout(self.deadline.saturating_duration_since(Instant::now())) {
            Ok(result) => result,
            Err(error) => {
                self.timed_out = true;
                stop(&mut self.child);
                Err(git_receive_error(error))
            }
        }
    }
}

impl Drop for Batch {
    fn drop(&mut self) {
        stop(&mut self.child);
    }
}

fn read_blob(
    input: &mut ChildStdin,
    output: &mut BufReader<ChildStdout>,
    oid: &str,
    budget: usize,
) -> BlobResult {
    let size = object_header(input, output, "info", oid)?;
    if size > budget {
        return Ok(None);
    }
    if object_header(input, output, "contents", oid)? != size {
        return Err(io::Error::other("Git blob size changed"));
    }
    let mut bytes = vec![0; size];
    output.read_exact(&mut bytes)?;
    let mut end = [0];
    output.read_exact(&mut end)?;
    if end != *b"\n" {
        return Err(io::Error::other("missing Git blob delimiter"));
    }
    Ok(Some(bytes))
}

fn object_header(
    input: &mut ChildStdin,
    output: &mut BufReader<ChildStdout>,
    request: &str,
    oid: &str,
) -> io::Result<usize> {
    writeln!(input, "{request} {oid}")?;
    let mut header = String::new();
    output.by_ref().take(128).read_line(&mut header)?;
    let size = header
        .strip_suffix('\n')
        .and_then(|line| line.strip_prefix(oid))
        .and_then(|rest| rest.strip_prefix(" blob "))
        .and_then(|size| size.parse().ok());
    size.ok_or_else(|| io::Error::other("unavailable or invalid Git blob"))
}
