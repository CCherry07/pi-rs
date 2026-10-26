use std::fs::{File, Metadata, OpenOptions, Permissions};
use std::io::{self, Write};
use std::path::Path;
use std::time::SystemTime;

/// One shared journal writer. Reading a store does not acquire write access.
pub(super) struct JournalWriter {
    file: Option<File>,
    stamp: FileStamp,
    #[cfg(test)]
    pub(super) append_opens: usize,
    #[cfg(test)]
    pub(super) fail_sync: bool,
}

#[derive(PartialEq, Eq)]
struct FileStamp {
    len: u64,
    modified: Option<SystemTime>,
    permissions: Permissions,
    #[cfg(unix)]
    identity: (u64, u64),
    #[cfg(unix)]
    owner: (u32, u32),
}

// ctime is deliberately excluded: moving the same journal away and back
// changes ctime even when its identity, contents and access mode are unchanged.

impl FileStamp {
    fn read(metadata: &Metadata) -> Self {
        #[cfg(unix)]
        use std::os::unix::fs::MetadataExt;

        Self {
            len: metadata.len(),
            modified: metadata.modified().ok(),
            permissions: metadata.permissions(),
            #[cfg(unix)]
            identity: (metadata.dev(), metadata.ino()),
            #[cfg(unix)]
            owner: (metadata.uid(), metadata.gid()),
        }
    }
}

impl JournalWriter {
    pub(super) fn unopened(path: &Path) -> io::Result<Self> {
        Ok(Self::new(None, &std::fs::metadata(path)?))
    }

    /// The supplied file must have been opened in append mode.
    pub(super) fn created(file: File) -> io::Result<Self> {
        let metadata = file.metadata()?;
        Ok(Self::new(Some(file), &metadata))
    }

    fn new(file: Option<File>, metadata: &Metadata) -> Self {
        Self {
            file,
            stamp: FileStamp::read(metadata),
            #[cfg(test)]
            append_opens: 0,
            #[cfg(test)]
            fail_sync: false,
        }
    }

    pub(super) fn append(&mut self, path: &Path, encoded: &[u8]) -> io::Result<()> {
        let result = self.append_checked(path, encoded);
        if result.is_err() {
            // A failed write/sync may have left a prefix on disk. Never retry
            // that batch internally or keep using an unchecked descriptor.
            self.file = None;
        }
        result
    }

    fn append_checked(&mut self, path: &Path, encoded: &[u8]) -> io::Result<()> {
        if FileStamp::read(&std::fs::metadata(path)?) != self.stamp {
            return Err(changed_file());
        }

        // Unix metadata gives a stable device/inode identity. Other platforms
        // retain the existing reopen-on-append behavior until an equivalent
        // identity check can be made without opening another handle.
        #[cfg(not(unix))]
        {
            self.file = None;
        }
        if self.file.is_none() {
            let file = OpenOptions::new().append(true).open(path)?;
            #[cfg(test)]
            {
                self.append_opens += 1;
            }
            if FileStamp::read(&file.metadata()?) != self.stamp {
                return Err(changed_file());
            }
            self.file = Some(file);
        }

        let file = self.file.as_mut().expect("journal writer was opened");
        file.write_all(encoded)?;
        #[cfg(test)]
        if std::mem::take(&mut self.fail_sync) {
            return Err(io::Error::other("injected journal sync failure"));
        }
        file.sync_data()?;
        let next = FileStamp::read(&file.metadata()?);
        if next.len != self.stamp.len.saturating_add(encoded.len() as u64)
            || FileStamp::read(&std::fs::metadata(path)?) != next
        {
            return Err(changed_file());
        }
        self.stamp = next;
        Ok(())
    }

    #[cfg(test)]
    pub(super) fn has_handle(&self) -> bool {
        self.file.is_some()
    }
}

fn changed_file() -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        "session journal changed outside this store; reopen it before appending",
    )
}
