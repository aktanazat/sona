use sha2::{Digest, Sha256};
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};

const BUFFER_BYTES: usize = 64 * 1024;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct VerifiedCopy {
    pub bytes: u64,
    pub sha256: [u8; 32],
}

pub(crate) fn copy_verified(source: &Path, destination: &Path) -> io::Result<VerifiedCopy> {
    let parent = destination
        .parent()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "missing destination parent"))?;
    fs::create_dir_all(parent)?;

    let temporary = destination.with_extension("copying");
    let copied = copy_to_temporary(source, &temporary)
        .and_then(|copy| fs::rename(&temporary, destination).map(|()| copy));
    let copy = match copied {
        Ok(copy) => copy,
        Err(error) => {
            let _ = fs::remove_file(&temporary);
            return Err(error);
        }
    };
    if file_hash(destination)? != copy.sha256 {
        let _ = fs::remove_file(destination);
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "copy hash mismatch",
        ));
    }
    Ok(copy)
}

/// The temporary name is fixed per destination, so one left by a copy that
/// died part way belongs to an earlier attempt and is replaced.
fn copy_to_temporary(source: &Path, temporary: &Path) -> io::Result<VerifiedCopy> {
    let mut input = File::open(source)?;
    match fs::remove_file(temporary) {
        Err(error) if error.kind() != io::ErrorKind::NotFound => return Err(error),
        _ => {}
    }
    let mut output = OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(temporary)?;
    let mut source_hash = Sha256::new();
    let mut copied = 0_u64;
    let mut buffer = vec![0_u8; BUFFER_BYTES];

    loop {
        let count = input.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        source_hash.update(&buffer[..count]);
        output.write_all(&buffer[..count])?;
        copied = copied.saturating_add(u64::try_from(count).unwrap_or(u64::MAX));
    }
    output.sync_all()?;
    drop(output);

    let source_len = fs::metadata(source)?.len();
    if copied != source_len || fs::metadata(temporary)?.len() != source_len {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "copy size mismatch",
        ));
    }
    Ok(VerifiedCopy {
        bytes: source_len,
        sha256: source_hash.finalize().into(),
    })
}

pub(crate) fn files_equal(left: &Path, right: &Path) -> io::Result<bool> {
    if fs::metadata(left)?.len() != fs::metadata(right)?.len() {
        return Ok(false);
    }
    Ok(file_hash(left)? == file_hash(right)?)
}

pub(crate) fn file_hash(path: &Path) -> io::Result<[u8; 32]> {
    let mut file = File::open(path)?;
    let mut digest = Sha256::new();
    let mut buffer = vec![0_u8; BUFFER_BYTES];
    loop {
        let count = file.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        digest.update(&buffer[..count]);
    }
    Ok(digest.finalize().into())
}

pub(crate) fn write_private_file(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "missing destination parent"))?;
    fs::create_dir_all(parent)?;
    // The app and a command-line run of the core write the same files.
    let mut temporary = path.as_os_str().to_owned();
    temporary.push(format!(".{}.tmp", std::process::id()));
    let temporary = PathBuf::from(temporary);
    let mut options = OpenOptions::new();
    options.create(true).truncate(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(&temporary)?;
    let written = file
        .write_all(bytes)
        .and_then(|()| file.sync_all())
        .and_then(|()| fs::rename(&temporary, path));
    if written.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    written
}

pub(crate) fn hex_digest(bytes: &[u8]) -> String {
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        use std::fmt::Write as _;
        let _ = write!(output, "{byte:02x}");
    }
    output
}
