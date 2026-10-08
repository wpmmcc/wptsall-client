//! Forward the bundled native VFS while admitting actual persistent growth.
//! No SQL, logging, or database calls are made inside a SQLite callback.
use anyhow::{ensure, Context, Result};
use rusqlite::{ffi, Connection, OpenFlags};
use std::ffi::{c_char, c_int, c_void, CStr};
use std::path::{Path, PathBuf};
use std::ptr;
use std::sync::OnceLock;

const NAME: &str = "wptsall-capacity-v1";
const C_NAME: &[u8] = b"wptsall-capacity-v1\0";
static REGISTERED: OnceLock<std::result::Result<(), String>> = OnceLock::new();



#[repr(C)]
struct File {
    base: ffi::sqlite3_file,
    state: *mut FileState,
}

struct FileState {
    native: *mut ffi::sqlite3_file,
    methods: ffi::sqlite3_io_methods,
    delegate: *mut ffi::sqlite3_vfs,
    path: PathBuf,
    _generated_name: Option<OwnedFilename>,
    identity: [u64; 2],
    delete_on_close: bool,
    chunk_size: u64,
}

fn protect<T>(fallback: T, body: impl FnOnce() -> T) -> T {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(body)).unwrap_or(fallback)
}

fn capacity_code(error: &anyhow::Error) -> c_int {
    if error.is::<crate::storage_capacity::RootCapacityExhausted>()
        || error.to_string().starts_with("STORAGE_CAPACITY_EXHAUSTED:")
    {
        ffi::SQLITE_FULL
    } else {
        ffi::SQLITE_IOERR
    }
}

fn path_from_name(name: *const c_char) -> Result<PathBuf> {
    ensure!(!name.is_null(), "SQLite filename missing");
    // SQLite owns a valid NUL-terminated filename for the callback's duration.
    let bytes = unsafe { CStr::from_ptr(name) }.to_bytes();
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        Ok(PathBuf::from(std::ffi::OsStr::from_bytes(bytes)))
    }
    #[cfg(not(unix))]
    {
        Ok(PathBuf::from(std::str::from_utf8(bytes)?))
    }
}

fn native_name(path: &Path) -> Result<Vec<u8>> {
    #[cfg(unix)]
    let bytes = {
        use std::os::unix::ffi::OsStrExt;
        path.as_os_str().as_bytes().to_vec()
    };
    #[cfg(not(unix))]
    let bytes = path
        .to_str()
        .context("SQLite path is not UTF-8")?
        .as_bytes()
        .to_vec();
    ensure!(!bytes.contains(&0), "SQLite path contains NUL");
    // SQLite URI helpers may read the empty parameter list after the name.
    Ok([bytes.as_slice(), &[0, 0, 0]].concat())
}

// Native URI helpers inspect four bytes BEFORE a filename. A plain CString
// or Vec with a trailing NUL is not the SQLite filename ABI.
struct OwnedFilename(*const c_char);

impl OwnedFilename {
    fn new(path: &Path) -> Result<Self> {
        let name = native_name(path)?;
        let pointer = unsafe {
            ffi::sqlite3_create_filename(
                name.as_ptr().cast(),
                b"\0".as_ptr().cast(),
                b"\0".as_ptr().cast(),
                0,
                ptr::null_mut(),
            )
        };
        ensure!(!pointer.is_null(), "SQLite filename allocation failed");
        Ok(Self(pointer))
    }
}

impl Drop for OwnedFilename {
    fn drop(&mut self) {
        unsafe { ffi::sqlite3_free_filename(self.0) }
    }
}

fn file_identity(path: &Path) -> Result<[u64; 2]> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let metadata = std::fs::metadata(path)?;
        ensure!(metadata.is_file(), "SQLite file authority changed");
        Ok([metadata.dev(), metadata.ino()])
    }
    #[cfg(windows)]
    {
        use std::os::windows::io::AsRawHandle;
        #[repr(C)]
        struct Information {
            attributes: u32,
            times: [u32; 6],
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
        let file = std::fs::File::open(path)?;
        let mut info = std::mem::MaybeUninit::<Information>::uninit();
        ensure!(
            unsafe { GetFileInformationByHandle(file.as_raw_handle(), info.as_mut_ptr()) } != 0,
            "SQLite file identity unavailable"
        );
        let info = unsafe { info.assume_init() };
        Ok([
            u64::from(info.volume),
            (u64::from(info.index_high) << 32) | u64::from(info.index_low),
        ])
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = path;
        anyhow::bail!("SQLite file identity unsupported")
    }
}

unsafe fn check_authority(file: &FileState) -> std::result::Result<(), c_int> {
    if file_identity(&file.path).ok() != Some(file.identity) {
        return Err(ffi::SQLITE_IOERR);
    }
    if let Some(control) = (*(*file.native).pMethods).xFileControl {
        let mut moved = 0;
        let code = control(
            file.native,
            ffi::SQLITE_FCNTL_HAS_MOVED,
            (&mut moved as *mut c_int).cast(),
        );
        if (code == ffi::SQLITE_OK && moved != 0)
            || (code != ffi::SQLITE_OK && code != ffi::SQLITE_NOTFOUND)
        {
            return Err(ffi::SQLITE_IOERR);
        }
    }
    Ok(())
}

unsafe fn state<'a>(file: *mut ffi::sqlite3_file) -> &'a mut FileState {
    &mut *(*(file.cast::<File>())).state
}

unsafe fn delegate(vfs: *mut ffi::sqlite3_vfs) -> *mut ffi::sqlite3_vfs {
    (*vfs).pAppData.cast()
}

unsafe fn size(file: &FileState) -> std::result::Result<u64, c_int> {
    let Some(call) = (*(*file.native).pMethods).xFileSize else {
        return Err(ffi::SQLITE_IOERR_FSTAT);
    };
    let mut value = 0;
    let code = call(file.native, &mut value);
    if code != ffi::SQLITE_OK {
        return Err(code);
    }
    u64::try_from(value).map_err(|_| ffi::SQLITE_IOERR_FSTAT)
}

fn rounded(value: u64, chunk: u64) -> Option<u64> {
    if chunk == 0 {
        Some(value)
    } else {
        value
            .checked_add(chunk - 1)
            .map(|value| value / chunk * chunk)
    }
}

fn shm_peak(page: c_int, page_size: c_int) -> Option<u64> {
    let page = u64::try_from(page).ok()?;
    let page_size = u64::try_from(page_size).ok().filter(|size| *size > 0)?;
    #[cfg(unix)]
    let group = {
        extern "C" {
            fn getpagesize() -> c_int;
        }
        // The bundled Unix VFS maps in groups of 32 KiB regions on systems
        // with larger native pages (notably 64 KiB aarch64 installations).
        u64::try_from(unsafe { getpagesize() })
            .ok()?
            .saturating_div(32768)
            .max(1)
    };
    #[cfg(not(unix))]
    let group = 1;
    rounded(page.checked_add(1)?, group)?
        .checked_mul(page_size)
        .filter(|bytes| *bytes <= c_int::MAX as u64)
}

unsafe fn growth(file: &FileState, end: u64, call: impl FnOnce() -> c_int) -> c_int {
    if let Err(code) = check_authority(file) {
        return code;
    }
    let before = match size(file) {
        Ok(value) => value,
        Err(code) => return code,
    };
    if end <= before {
        return call();
    }
    let mut lease = match crate::storage_capacity::StorageLease::for_database_growth(
        &file.path,
        end - before,
    ) {
        Ok(value) => value,
        Err(error) => return capacity_code(&error),
    };
    if let Err(code) = check_authority(file) {
        return code;
    }
    if let Err(error) = lease.prepay_database_growth() {
        return capacity_code(&error);
    }
    let code = call();
    let after = match size(file) {
        Ok(value) => value,
        Err(_) => return ffi::SQLITE_IOERR_FSTAT,
    };
    if after.saturating_sub(before) > end - before {
        return ffi::SQLITE_IOERR;
    }
    code
}

pub(super) fn open(path: &str) -> Result<Connection> {
    open_with_flags(path, OpenFlags::default() & !OpenFlags::SQLITE_OPEN_URI)
}

pub(super) fn open_read_only(path: &str) -> Result<Connection> {
    open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)
}

fn open_with_flags(path: &str, flags: OpenFlags) -> Result<Connection> {
    ensure!(
        !path.starts_with("file:"),
        "SQLite URI overrides are not storage authority"
    );
    register()?;
    Ok(Connection::open_with_flags_and_vfs(path, flags, NAME)?)
}

fn register() -> Result<()> {
    let result = REGISTERED.get_or_init(|| unsafe {
        if ffi::sqlite3_initialize() != ffi::SQLITE_OK {
            return Err("SQLite initialization failed".into());
        }
        let base = ffi::sqlite3_vfs_find(ptr::null());
        if base.is_null() || (*base).xOpen.is_none() || (*base).szOsFile <= 0 {
            return Err("SQLite native VFS unavailable".into());
        }
        let mut vfs = Box::new(*base);
        vfs.szOsFile = std::mem::size_of::<File>() as c_int;
        vfs.zName = C_NAME.as_ptr().cast();
        vfs.pNext = ptr::null_mut();
        vfs.pAppData = base.cast();
        vfs.xOpen = Some(open_file);
        vfs.xDelete = Some(delete);
        vfs.xAccess = Some(access);
        vfs.xFullPathname = Some(full_path);
        vfs.xDlOpen = Some(dl_open);
        vfs.xDlError = Some(dl_error);
        vfs.xDlSym = Some(dl_sym);
        vfs.xDlClose = Some(dl_close);
        vfs.xRandomness = Some(randomness);
        vfs.xSleep = Some(sleep);
        vfs.xCurrentTime = Some(current_time);
        vfs.xGetLastError = Some(last_error);
        vfs.xCurrentTimeInt64 = (*base).xCurrentTimeInt64.map(|_| current_time_int64 as _);
        vfs.xSetSystemCall = (*base).xSetSystemCall.map(|_| set_system_call as _);
        vfs.xGetSystemCall = (*base).xGetSystemCall.map(|_| get_system_call as _);
        vfs.xNextSystemCall = (*base).xNextSystemCall.map(|_| next_system_call as _);
        let raw = Box::into_raw(vfs);
        let code = ffi::sqlite3_vfs_register(raw, 0);
        if code != ffi::SQLITE_OK {
            drop(Box::from_raw(raw));
            return Err("SQLite capacity VFS registration failed".into());
        }
        // Registered callbacks require this small table for the process lifetime.
        Ok(())
    });
    result
        .as_ref()
        .map(|_| ())
        .map_err(|message| anyhow::anyhow!("{message}"))
}

unsafe extern "C" fn open_file(
    vfs: *mut ffi::sqlite3_vfs,
    name: *const c_char,
    file: *mut ffi::sqlite3_file,
    flags: c_int,
    out_flags: *mut c_int,
) -> c_int {
    protect(ffi::SQLITE_IOERR, || unsafe {
        (*file).pMethods = ptr::null();
        (*(file.cast::<File>())).state = ptr::null_mut();
        let base = delegate(vfs);
        let temporary = name.is_null()
            || flags
                & (ffi::SQLITE_OPEN_DELETEONCLOSE
                    | ffi::SQLITE_OPEN_TEMP_DB
                    | ffi::SQLITE_OPEN_TEMP_JOURNAL
                    | ffi::SQLITE_OPEN_SUBJOURNAL
                    | ffi::SQLITE_OPEN_TRANSIENT_DB)
                != 0;
        let path = if temporary {
            crate::storage_capacity::sqlite_temp_path()
        } else {
            match path_from_name(name) {
                Ok(path) => path,
                Err(_) => return ffi::SQLITE_CANTOPEN,
            }
        };
        let lease = if flags & ffi::SQLITE_OPEN_CREATE != 0 && !path.exists() {
            match crate::storage_capacity::StorageLease::for_database_growth(&path, 0) {
                Ok(mut value) => {
                    if let Err(error) = value.prepay_database_growth() {
                        return capacity_code(&error);
                    }
                    Some(value)
                }
                Err(error) => return capacity_code(&error),
            }
        } else {
            None
        };
        let generated = if temporary {
            match OwnedFilename::new(&path) {
                Ok(value) => Some(value),
                Err(_) => return ffi::SQLITE_CANTOPEN,
            }
        } else {
            None
        };
        let actual_name = generated.as_ref().map_or(name, |value| value.0);
        let native = ffi::sqlite3_malloc64((*base).szOsFile as u64).cast::<ffi::sqlite3_file>();
        if native.is_null() {
            return ffi::SQLITE_NOMEM;
        }
        ptr::write_bytes(native.cast::<u8>(), 0, (*base).szOsFile as usize);
        // Keep temp inodes visible to the inventory until their owned close.
        let actual_flags = flags & !ffi::SQLITE_OPEN_DELETEONCLOSE;
        let code = (*base).xOpen.unwrap()(base, actual_name, native, actual_flags, out_flags);
        if code != ffi::SQLITE_OK {
            if !(*native).pMethods.is_null() {
                if let Some(close) = (*(*native).pMethods).xClose {
                    close(native);
                }
            }
            ffi::sqlite3_free(native.cast());
            return code;
        }
        #[cfg(unix)]
        if lease.is_some() {
            use std::os::unix::fs::PermissionsExt;
            if std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).is_err() {
                (*(*native).pMethods).xClose.unwrap()(native);
                ffi::sqlite3_free(native.cast());
                return ffi::SQLITE_IOERR;
            }
        }
        let identity = match file_identity(&path) {
            Ok(value) => value,
            Err(_) => {
                (*(*native).pMethods).xClose.unwrap()(native);
                ffi::sqlite3_free(native.cast());
                return ffi::SQLITE_IOERR;
            }
        };
        if let Some(lease) = lease {
            if lease.finish().is_err() {
                (*(*native).pMethods).xClose.unwrap()(native);
                ffi::sqlite3_free(native.cast());
                return ffi::SQLITE_IOERR;
            }
        }
        let original = &*(*native).pMethods;
        let mut methods = *original;
        methods.xClose = Some(close_file);
        methods.xRead = Some(read);
        methods.xWrite = Some(write);
        methods.xTruncate = Some(truncate);
        methods.xSync = Some(sync);
        methods.xFileSize = Some(file_size);
        methods.xLock = Some(lock);
        methods.xUnlock = Some(unlock);
        methods.xCheckReservedLock = Some(check_reserved);
        methods.xFileControl = Some(file_control);
        methods.xSectorSize = Some(sector_size);
        methods.xDeviceCharacteristics = Some(device_characteristics);
        if original.iVersion >= 2 {
            methods.xShmMap = original.xShmMap.map(|_| shm_map as _);
            methods.xShmLock = original.xShmLock.map(|_| shm_lock as _);
            methods.xShmBarrier = original.xShmBarrier.map(|_| shm_barrier as _);
            methods.xShmUnmap = original.xShmUnmap.map(|_| shm_unmap as _);
        }
        if original.iVersion >= 3 {
            methods.xFetch = original.xFetch.map(|_| fetch as _);
            methods.xUnfetch = original.xUnfetch.map(|_| unfetch as _);
        }
        let state = Box::into_raw(Box::new(FileState {
            native,
            methods,
            delegate: base,
            path,
            _generated_name: generated,
            identity,
            delete_on_close: flags & ffi::SQLITE_OPEN_DELETEONCLOSE != 0,
            chunk_size: 0,
        }));
        (*(file.cast::<File>())).state = state;
        (*file).pMethods = &(*state).methods;
        ffi::SQLITE_OK
    })
}

unsafe extern "C" fn close_file(file: *mut ffi::sqlite3_file) -> c_int {
    protect(ffi::SQLITE_IOERR_CLOSE, || unsafe {
        let raw = (*(file.cast::<File>())).state;
        let state = Box::from_raw(raw);
        (*file).pMethods = ptr::null();
        (*(file.cast::<File>())).state = ptr::null_mut();
        let code = (*(*state.native).pMethods).xClose.unwrap()(state.native);
        ffi::sqlite3_free(state.native.cast());
        if code == ffi::SQLITE_OK && state.delete_on_close {
            if file_identity(&state.path).ok() != Some(state.identity) {
                return ffi::SQLITE_IOERR_DELETE;
            }
            if let Ok(name) = native_name(&state.path) {
                let deleted =
                    (*state.delegate).xDelete.unwrap()(state.delegate, name.as_ptr().cast(), 0);
                if deleted != ffi::SQLITE_OK {
                    return deleted;
                }
            } else {
                return ffi::SQLITE_IOERR_DELETE;
            }
        }
        code
    })
}

unsafe extern "C" fn write(
    file: *mut ffi::sqlite3_file,
    bytes: *const c_void,
    amount: c_int,
    offset: i64,
) -> c_int {
    protect(ffi::SQLITE_IOERR_WRITE, || unsafe {
        let state = state(file);
        if let Err(code) = check_authority(state) {
            return code;
        }
        let Some(end) = u64::try_from(offset).ok().and_then(|value| {
            u64::try_from(amount)
                .ok()
                .and_then(|amount| value.checked_add(amount))
        }) else {
            return ffi::SQLITE_IOERR_WRITE;
        };
        growth(state, end, || {
            (*(*state.native).pMethods).xWrite.unwrap()(state.native, bytes, amount, offset)
        })
    })
}

unsafe extern "C" fn truncate(file: *mut ffi::sqlite3_file, end: i64) -> c_int {
    protect(ffi::SQLITE_IOERR_TRUNCATE, || unsafe {
        let state = state(file);
        let Some(peak) = u64::try_from(end)
            .ok()
            .and_then(|end| rounded(end, state.chunk_size))
        else {
            return ffi::SQLITE_IOERR_TRUNCATE;
        };
        growth(state, peak, || {
            (*(*state.native).pMethods).xTruncate.unwrap()(state.native, end)
        })
    })
}

unsafe extern "C" fn file_control(
    file: *mut ffi::sqlite3_file,
    op: c_int,
    argument: *mut c_void,
) -> c_int {
    protect(ffi::SQLITE_IOERR, || unsafe {
        let state = state(file);
        let Some(call) = (*(*state.native).pMethods).xFileControl else {
            return ffi::SQLITE_NOTFOUND;
        };
        if matches!(
            op,
            ffi::SQLITE_FCNTL_CHUNK_SIZE | ffi::SQLITE_FCNTL_SIZE_HINT
        ) && argument.is_null()
        {
            return ffi::SQLITE_MISUSE;
        }
        if op == ffi::SQLITE_FCNTL_SIZE_HINT {
            if argument.is_null() {
                return ffi::SQLITE_MISUSE;
            }
            let Some(peak) = u64::try_from(*argument.cast::<i64>())
                .ok()
                .and_then(|end| rounded(end, state.chunk_size))
            else {
                return ffi::SQLITE_IOERR;
            };
            return growth(state, peak, || call(state.native, op, argument));
        }
        let code = call(state.native, op, argument);
        if code == ffi::SQLITE_OK && op == ffi::SQLITE_FCNTL_CHUNK_SIZE && !argument.is_null() {
            state.chunk_size = u64::try_from(*argument.cast::<c_int>()).unwrap_or(0);
        }
        code
    })
}

unsafe extern "C" fn shm_map(
    file: *mut ffi::sqlite3_file,
    page: c_int,
    page_size: c_int,
    extend: c_int,
    output: *mut *mut c_void,
) -> c_int {
    protect(ffi::SQLITE_IOERR_SHMMAP, || unsafe {
        let state = state(file);
        if output.is_null() {
            return ffi::SQLITE_MISUSE;
        }
        *output = ptr::null_mut();
        if let Err(code) = check_authority(state) {
            return code;
        }
        let Some(requested_peak) = shm_peak(page, page_size) else {
            return ffi::SQLITE_IOERR_SHMMAP;
        };
        // Native Unix attachment initializes a new/short SHM to three bytes
        // before checking extend. A nonextending lookup is not a no-write proof.
        #[cfg(unix)]
        let peak = if extend != 0 {
            requested_peak.max(3)
        } else {
            3
        };
        #[cfg(not(unix))]
        let peak = requested_peak;
        let mut path = state.path.as_os_str().to_os_string();
        path.push("-shm");
        let path = PathBuf::from(path);
        let mut before = match std::fs::metadata(&path) {
            Ok(value) => value.len(),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => 0,
            Err(_) => return ffi::SQLITE_IOERR_SHMSIZE,
        };
        let lease = if peak > before {
            match crate::storage_capacity::StorageLease::for_database_peak(&path, peak) {
                Ok((mut value, locked_before)) => {
                    // Other native connections may have attached while this
                    // one waited. Debit and readback use the same locked size.
                    before = locked_before;
                    if let Err(error) = value.prepay_database_growth() {
                        return capacity_code(&error);
                    }
                    Some(value)
                }
                Err(error) => return capacity_code(&error),
            }
        } else {
            None
        };
        if let Err(code) = check_authority(state) {
            return code;
        }
        let code = (*(*state.native).pMethods).xShmMap.unwrap()(
            state.native,
            page,
            page_size,
            extend,
            output,
        );
        if let Some(lease) = lease {
            let after = match std::fs::metadata(&path) {
                Ok(value) => value.len(),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => 0,
                Err(_) => return ffi::SQLITE_IOERR_SHMSIZE,
            };
            if after.saturating_sub(before) > peak.saturating_sub(before) {
                return ffi::SQLITE_IOERR_SHMSIZE;
            }
            drop(lease);
        }
        code
    })
}

macro_rules! forward_file {
    ($name:ident, $field:ident, $fallback:expr $(, $argument:ident : $kind:ty)*) => {
        unsafe extern "C" fn $name(file: *mut ffi::sqlite3_file, $($argument: $kind),*) -> c_int {
            protect($fallback, || unsafe {
                let state = state(file);
                (*(*state.native).pMethods).$field
                    .map_or($fallback, |call| call(state.native, $($argument),*))
            })
        }
    };
}
forward_file!(read, xRead, ffi::SQLITE_IOERR_READ, buffer: *mut c_void, amount: c_int, offset: i64);
forward_file!(sync, xSync, ffi::SQLITE_IOERR_FSYNC, flags: c_int);
forward_file!(file_size, xFileSize, ffi::SQLITE_IOERR_FSTAT, size: *mut i64);
forward_file!(lock, xLock, ffi::SQLITE_IOERR_LOCK, kind: c_int);
forward_file!(unlock, xUnlock, ffi::SQLITE_IOERR_UNLOCK, kind: c_int);
forward_file!(check_reserved, xCheckReservedLock, ffi::SQLITE_IOERR_CHECKRESERVEDLOCK, output: *mut c_int);
forward_file!(sector_size, xSectorSize, 4096);
forward_file!(device_characteristics, xDeviceCharacteristics, 0);
forward_file!(shm_lock, xShmLock, ffi::SQLITE_IOERR_SHMLOCK, offset: c_int, count: c_int, flags: c_int);
forward_file!(shm_unmap, xShmUnmap, ffi::SQLITE_IOERR_SHMMAP, delete: c_int);
forward_file!(fetch, xFetch, ffi::SQLITE_IOERR, offset: i64, amount: c_int, output: *mut *mut c_void);
forward_file!(unfetch, xUnfetch, ffi::SQLITE_IOERR, offset: i64, mapped: *mut c_void);

unsafe extern "C" fn shm_barrier(file: *mut ffi::sqlite3_file) {
    protect((), || unsafe {
        let state = state(file);
        if let Some(call) = (*(*state.native).pMethods).xShmBarrier {
            call(state.native);
        }
    })
}

macro_rules! forward_vfs {
    ($name:ident, $field:ident, $result:ty, $fallback:expr $(, $argument:ident : $kind:ty)*) => {
        unsafe extern "C" fn $name(vfs: *mut ffi::sqlite3_vfs, $($argument: $kind),*) -> $result {
            protect($fallback, || unsafe {
                let base = delegate(vfs);
                (*base).$field.map_or($fallback, |call| call(base, $($argument),*))
            })
        }
    };
}
forward_vfs!(delete, xDelete, c_int, ffi::SQLITE_IOERR_DELETE, name: *const c_char, sync: c_int);
forward_vfs!(access, xAccess, c_int, ffi::SQLITE_IOERR_ACCESS, name: *const c_char, flags: c_int, output: *mut c_int);
forward_vfs!(full_path, xFullPathname, c_int, ffi::SQLITE_CANTOPEN, name: *const c_char, size: c_int, output: *mut c_char);
forward_vfs!(dl_open, xDlOpen, *mut c_void, ptr::null_mut(), name: *const c_char);
forward_vfs!(dl_error, xDlError, (), (), size: c_int, output: *mut c_char);
type Symbol = Option<unsafe extern "C" fn(*mut ffi::sqlite3_vfs, *mut c_void, *const c_char)>;
forward_vfs!(dl_sym, xDlSym, Symbol, None, handle: *mut c_void, name: *const c_char);
forward_vfs!(dl_close, xDlClose, (), (), handle: *mut c_void);
forward_vfs!(randomness, xRandomness, c_int, 0, size: c_int, output: *mut c_char);
forward_vfs!(sleep, xSleep, c_int, 0, microseconds: c_int);
forward_vfs!(current_time, xCurrentTime, c_int, ffi::SQLITE_ERROR, output: *mut f64);
forward_vfs!(last_error, xGetLastError, c_int, 0, size: c_int, output: *mut c_char);
forward_vfs!(current_time_int64, xCurrentTimeInt64, c_int, ffi::SQLITE_ERROR, output: *mut i64);
forward_vfs!(set_system_call, xSetSystemCall, c_int, ffi::SQLITE_NOTFOUND, name: *const c_char, call: ffi::sqlite3_syscall_ptr);
forward_vfs!(get_system_call, xGetSystemCall, ffi::sqlite3_syscall_ptr, None, name: *const c_char);
forward_vfs!(next_system_call, xNextSystemCall, *const c_char, ptr::null(), name: *const c_char);
