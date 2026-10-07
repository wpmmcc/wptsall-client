use anyhow::{ensure, Context, Result};
use std::fs::{File, OpenOptions};
use std::path::{Path, PathBuf};

pub(crate) fn identity(file: &File) -> Result<[u64; 2]> {
    ensure!(file.metadata()?.is_file(), "native lease is not a regular file");
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let metadata = file.metadata()?;
        Ok([metadata.dev(), metadata.ino()])
    }
    #[cfg(windows)]
    {
        use std::ffi::{c_int, c_void};
        use std::os::windows::io::AsRawHandle;
        #[repr(C)]
        struct Information {
            attributes: u32,
            created: [u32; 2],
            accessed: [u32; 2],
            modified: [u32; 2],
            volume: u32,
            size_high: u32,
            size_low: u32,
            links: u32,
            index_high: u32,
            index_low: u32,
        }
        #[link(name = "kernel32")]
        extern "system" {
            fn GetFileInformationByHandle(handle: *mut c_void, result: *mut Information) -> c_int;
        }
        let mut information = std::mem::MaybeUninit::<Information>::uninit();
        ensure!(
            unsafe { GetFileInformationByHandle(file.as_raw_handle(), information.as_mut_ptr()) }
                != 0,
            "inspect native lease identity"
        );
        let information = unsafe { information.assume_init() };
        Ok([
            information.volume as u64,
            ((information.index_high as u64) << 32) | information.index_low as u64,
        ])
    }
    #[cfg(not(any(unix, windows)))]
    anyhow::bail!("native lease identity is unavailable")
}

pub(crate) struct NativeLease {
    file: File,
    path: PathBuf,
    identity: [u64; 2],
}

impl NativeLease {
    pub(crate) fn acquire(path: &Path, busy: &'static str) -> Result<Self> {
        match path.symlink_metadata() {
            Ok(metadata) => ensure!(
                metadata.file_type().is_file(),
                "native lease path is not a regular file; retained"
            ),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error).context("inspect native lease"),
        }
        let mut options = OpenOptions::new();
        options.read(true).write(true).create(true).truncate(false);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600).custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC);
        }
        #[cfg(windows)]
        {
            use std::os::windows::fs::OpenOptionsExt;
            options.custom_flags(0x0020_0000); // FILE_FLAG_OPEN_REPARSE_POINT
        }
        let file = options.open(path).context("open native lease")?;
        #[cfg(windows)]
        {
            use std::os::windows::fs::MetadataExt;
            ensure!(file.metadata()?.file_attributes() & 0x400 == 0,
                "native lease cannot be a reparse point");
        }
        match file.try_lock() {
            Ok(()) => {}
            Err(std::fs::TryLockError::WouldBlock) => anyhow::bail!("{busy}"),
            Err(std::fs::TryLockError::Error(error)) => return Err(error).context("lock native lease"),
        }
        let lease = Self {
            identity: identity(&file)?,
            file,
            path: path.to_path_buf(),
        };
        lease.assert_owner()?;
        Ok(lease)
    }

    pub(crate) fn assert_owner(&self) -> Result<()> {
        let mut options = OpenOptions::new();
        options.read(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC);
        }
        #[cfg(windows)]
        {
            use std::os::windows::fs::OpenOptionsExt;
            options.custom_flags(0x0020_0000);
        }
        ensure!(
            identity(&self.file)? == self.identity
                && self.path.symlink_metadata()?.file_type().is_file()
                && identity(&options.open(&self.path)?)? == self.identity
                && self.path.symlink_metadata()?.file_type().is_file(),
            "native lease path changed; original evidence retained"
        );
        Ok(())
    }
}
