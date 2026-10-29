//! Inode lifetime guard for private attempts, inherited by admitted writers.
use std::{
    ffi::OsStr,
    fs::File,
    io,
    path::{Component, Path},
    sync::Arc,
};
#[derive(Clone, Debug)]
pub struct AttemptLease(pub(crate) Arc<File>);
pub struct CleanupLease(File);
#[cfg(unix)]
fn open(path: &Path) -> io::Result<File> {
    use std::os::unix::fs::OpenOptionsExt;
    std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(path)
}
#[cfg(not(unix))]
fn open(_: &Path) -> io::Result<File> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "Attempt lifetime locking requires Unix",
    ))
}
impl AttemptLease {
    /// Create an admitted output at the held inode. The caller must reserve its
    /// complete bounded size before creating or extending this file.
    pub fn create_readwrite_child(&self, name: &OsStr) -> io::Result<File> {
        valid_child(name)?;
        #[cfg(unix)]
        {
            use rustix::fs::{Mode, OFlags, openat};
            let fd = openat(
                &*self.0,
                Path::new(name),
                OFlags::RDWR
                    | OFlags::CREATE
                    | OFlags::EXCL
                    | OFlags::NOFOLLOW
                    | OFlags::CLOEXEC
                    | OFlags::NONBLOCK,
                Mode::RUSR | Mode::WUSR,
            )?;
            Ok(fd.into())
        }
        #[cfg(not(unix))]
        {
            Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "Attempt child creation requires Unix",
            ))
        }
    }
    pub fn sync_directory(&self) -> io::Result<()> {
        self.0.sync_all()
    }
    pub fn still_names(&self, path: &Path) -> io::Result<bool> {
        same_directory(&self.0, path)
    }
    /// Check the canonical name and live inode without caching either fact.
    /// macOS can report the held directory's current name directly. Only a
    /// matching name and live inode shortcut the original lookup. Negative or
    /// unavailable observations retain its alias and lookup-error semantics.
    pub fn still_names_canonical(&self, path: &Path) -> io::Result<bool> {
        #[cfg(target_os = "macos")]
        {
            use std::{ffi::OsStr, os::unix::ffi::OsStrExt};
            if let Ok(current) = rustix::fs::getpath(&*self.0)
                && Path::new(OsStr::from_bytes(current.as_bytes())) == path
                && same_directory(&self.0, path)?
            {
                return Ok(true);
            }
        }
        if std::fs::canonicalize(path)? != path {
            return Ok(false);
        }
        same_directory(&self.0, path)
    }
    /// Open a direct child of the locked attempt inode, never following a
    /// replacement of the attempt path or a symbolic link at the file name.
    pub fn open_readonly_child(&self, name: &OsStr) -> io::Result<File> {
        open_child(&self.0, name)
    }
    pub fn spawn_readonly(
        self,
        command: std::process::Command,
    ) -> io::Result<crate::ReservedChild> {
        crate::child::spawn(None, command, Some(self))
    }
    pub fn acquire(path: &Path) -> io::Result<Self> {
        let file = open(path)?;
        fs2::FileExt::lock_shared(&file)?;
        Ok(Self(Arc::new(file)))
    }
    pub fn try_cleanup(path: &Path) -> io::Result<Option<CleanupLease>> {
        let file = match open(path) {
            Ok(file) => file,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error),
        };
        match fs2::FileExt::try_lock_exclusive(&file) {
            Ok(()) => Ok(Some(CleanupLease(file))),
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => Ok(None),
            Err(error) => Err(error),
        }
    }
}
impl CleanupLease {
    pub fn still_names(&self, path: &Path) -> io::Result<bool> {
        same_directory(&self.0, path)
    }
    pub fn open_readonly_child(&self, name: &OsStr) -> io::Result<File> {
        open_child(&self.0, name)
    }
    /// Unlink only a direct child of this locked directory, then make the
    /// directory entry removal durable before the collector records success.
    pub fn unlink_child_sync(&self, name: &OsStr) -> io::Result<()> {
        valid_child(name)?;
        #[cfg(unix)]
        {
            rustix::fs::unlinkat(&self.0, Path::new(name), rustix::fs::AtFlags::empty())?;
            self.0.sync_all()
        }
        #[cfg(not(unix))]
        {
            let _ = name;
            Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "Attempt child deletion requires Unix",
            ))
        }
    }
}
fn valid_child(name: &OsStr) -> io::Result<()> {
    let mut components = Path::new(name).components();
    if !matches!(components.next(), Some(Component::Normal(_))) || components.next().is_some() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "Invalid attempt child name",
        ));
    }
    Ok(())
}
fn open_child(directory: &File, name: &OsStr) -> io::Result<File> {
    valid_child(name)?;
    #[cfg(unix)]
    {
        use rustix::fs::{Mode, OFlags, openat};
        let fd = openat(
            directory,
            Path::new(name),
            OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC | OFlags::NONBLOCK,
            Mode::empty(),
        )?;
        Ok(fd.into())
    }
    #[cfg(not(unix))]
    {
        let _ = (directory, name);
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "Attempt child reads require Unix",
        ))
    }
}
fn same_directory(file: &File, path: &Path) -> io::Result<bool> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let opened = file.metadata()?;
        let named = match std::fs::symlink_metadata(path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(false),
            Err(error) => return Err(error),
        };
        Ok(named.is_dir() && opened.dev() == named.dev() && opened.ino() == named.ino())
    }
    #[cfg(not(unix))]
    {
        let _ = (file, path);
        Ok(false)
    }
}
