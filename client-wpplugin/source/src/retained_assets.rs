//! Authenticated, bounded asset files. Legacy paid files are read, never migrated.
use aes_gcm::{
    aead::{Aead, AeadInPlace, KeyInit, Payload},
    Aes256Gcm, Nonce,
};
use anyhow::{anyhow, ensure, Context, Result};
use sha2::{Digest, Sha256};
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

const MAGIC: &[u8; 8] = b"WPTSAS01";
// Old owned files start with a UUID; this namespace cannot collide with them.
pub(crate) const NAME_PREFIX: &str = "wpa1";
pub(crate) const MAX_BYTES: u64 = 2 * 1024 * 1024 * 1024;
pub(crate) const CHUNK_BYTES: usize = 1024 * 1024;
const TAG_BYTES: usize = 16;
const META_BYTES: usize = 8 + 32 + 4 + 8;
const HEADER_BYTES: usize = 8 + 16 + META_BYTES + TAG_BYTES;



struct Encrypted {
    cipher: Aes256Gcm,
    aad: Vec<u8>,
    sha256: [u8; 32],
}

pub(crate) struct AssetReader {
    file: File,
    size: u64,
    physical_size: u64,
    encrypted: Option<Encrypted>,
}

pub(crate) fn directory(path: &Path) -> Result<PathBuf> {
    let mut builder = fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder
        .create(path)
        .context("create asset directory failed")?;
    let meta = fs::symlink_metadata(path).context("stat asset directory failed")?;
    ensure!(
        meta.is_dir() && !meta.file_type().is_symlink(),
        "asset directory is not a regular directory; retained"
    );
    // Existing directories are not chmod'ed or migrated.
    fs::canonicalize(path).context("resolve asset directory failed")
}

fn cipher(id: &[u8; 16]) -> Result<Aes256Gcm> {
    let key = crate::bindings::derive_retained_asset_key(id)?;
    Aes256Gcm::new_from_slice(&key).map_err(|_| anyhow!("init retained asset cipher failed"))
}

fn aad(path: &Path, id: &[u8; 16]) -> Result<Vec<u8>> {
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .context("retained asset filename is not UTF-8")?;
    let mut aad = b"wptsall-retained-asset-v1\0".to_vec();
    aad.extend_from_slice(MAGIC);
    aad.extend_from_slice(id);
    aad.extend_from_slice(name.as_bytes());
    Ok(aad)
}

fn nonce(index: u64) -> [u8; 12] {
    let mut nonce = [0; 12];
    nonce[4..].copy_from_slice(&index.to_be_bytes());
    nonce
}

fn chunk_aad(header_aad: &[u8], size: u64, index: u64, length: usize) -> Vec<u8> {
    let mut aad = header_aad.to_vec();
    aad.extend_from_slice(&size.to_be_bytes());
    aad.extend_from_slice(&index.to_be_bytes());
    aad.extend_from_slice(&(length as u64).to_be_bytes());
    aad
}

fn encoded_size(size: u64) -> Result<u64> {
    size.checked_add(
        size.div_ceil(CHUNK_BYTES as u64)
            .checked_mul(TAG_BYTES as u64)
            .context("retained asset size overflow")?,
    )
    .and_then(|size| size.checked_add(HEADER_BYTES as u64))
    .context("retained asset size overflow")
}

fn allocated(length: usize) -> Result<Vec<u8>> {
    let mut bytes = Vec::new();
    bytes
        .try_reserve_exact(length)
        .map_err(|_| anyhow!("retained asset allocation refused"))?;
    bytes.resize(length, 0);
    Ok(bytes)
}

impl AssetReader {
    pub(crate) fn open(path: &Path, max_bytes: u64) -> Result<Self> {
        let before = fs::symlink_metadata(path).context("stat retained asset failed")?;
        let expected_encrypted = is_encrypted_path(path);
        ensure!(
            before.is_file() || (!expected_encrypted && before.file_type().is_symlink()),
            "retained asset is not a regular file"
        );
        let mut file = File::open(path).context("open retained asset failed")?;
        let meta = file
            .metadata()
            .context("stat opened retained asset failed")?;
        ensure!(meta.is_file(), "retained asset is not a regular file");
        let mut magic = [0; 8];
        file.read_exact(&mut magic[..usize::try_from(meta.len().min(8))?])
            .context("read retained asset prefix failed")?;
        let encrypted = expected_encrypted;
        ensure!(
            !expected_encrypted || &magic == MAGIC,
            "damaged encrypted asset header; retained"
        );
        if !encrypted {
            ensure!(meta.len() <= max_bytes, "retained asset exceeds byte limit");
            return Ok(Self {
                size: meta.len(),
                physical_size: meta.len(),
                file,
                encrypted: None,
            });
        }
        ensure!(
            before.is_file() && !before.file_type().is_symlink(),
            "encrypted asset is not a private regular file; retained"
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::{MetadataExt, PermissionsExt};
            let after = fs::symlink_metadata(path).context("retained asset disappeared")?;
            ensure!(
                meta.nlink() == 1
                    && meta.permissions().mode() & 0o077 == 0
                    && before.dev() == meta.dev()
                    && before.ino() == meta.ino()
                    && after.dev() == meta.dev()
                    && after.ino() == meta.ino()
                    && !after.file_type().is_symlink(),
                "encrypted asset is shared or changed during open; retained"
            );
        }
        ensure!(
            meta.len() >= HEADER_BYTES as u64 && meta.len() <= encoded_size(MAX_BYTES)?,
            "encrypted asset physical bounds invalid; retained"
        );
        let mut id = [0; 16];
        file.read_exact(&mut id)
            .context("read asset identity failed")?;
        ensure!(
            !uuid::Uuid::from_bytes(id).is_nil(),
            "encrypted asset identity is missing; retained"
        );
        let aad = aad(path, &id)?;
        let cipher = cipher(&id)?;
        let mut encrypted_meta = [0; META_BYTES + TAG_BYTES];
        file.read_exact(&mut encrypted_meta)
            .context("read encrypted asset header failed")?;
        let metadata = cipher
            .decrypt(
                Nonce::from_slice(&nonce(0)),
                Payload {
                    msg: &encrypted_meta,
                    aad: &aad,
                },
            )
            .map_err(|_| anyhow!("encrypted asset authentication failed; retained"))?;
        ensure!(
            metadata.len() == META_BYTES,
            "encrypted asset metadata invalid; retained"
        );
        let size = u64::from_be_bytes(metadata[..8].try_into()?);
        let sha256 = metadata[8..40].try_into()?;
        let chunk_size = u32::from_be_bytes(metadata[40..44].try_into()?);
        let chunks = u64::from_be_bytes(metadata[44..52].try_into()?);
        ensure!(
            size <= MAX_BYTES
                && size <= max_bytes
                && chunk_size as usize == CHUNK_BYTES
                && chunks == size.div_ceil(CHUNK_BYTES as u64)
                && meta.len() == encoded_size(size)?,
            "encrypted asset bounds or record count invalid; retained"
        );
        Ok(Self {
            file,
            size,
            physical_size: meta.len(),
            encrypted: Some(Encrypted {
                cipher,
                aad,
                sha256,
            }),
        })
    }

    pub(crate) fn len(&self) -> u64 {
        self.size
    }

    fn chunk(&mut self, index: u64) -> Result<Vec<u8>> {
        ensure!(
            self.file.metadata()?.len() == self.physical_size,
            "retained asset changed size; retained"
        );
        let start = index
            .checked_mul(CHUNK_BYTES as u64)
            .context("retained asset chunk offset overflow")?;
        ensure!(start < self.size, "retained asset chunk is out of range");
        let length = usize::try_from((self.size - start).min(CHUNK_BYTES as u64))?;
        let Some(encrypted) = &self.encrypted else {
            self.file.seek(SeekFrom::Start(start))?;
            let mut bytes = allocated(length)?;
            self.file
                .read_exact(&mut bytes)
                .context("legacy retained asset truncated; retained")?;
            return Ok(bytes);
        };
        let offset = index
            .checked_mul((CHUNK_BYTES + TAG_BYTES) as u64)
            .and_then(|offset| offset.checked_add(HEADER_BYTES as u64))
            .context("encrypted asset chunk offset overflow")?;
        self.file.seek(SeekFrom::Start(offset))?;
        let mut bytes = allocated(length + TAG_BYTES)?;
        self.file
            .read_exact(&mut bytes)
            .context("encrypted asset record truncated; retained")?;
        encrypted
            .cipher
            .decrypt_in_place(
                Nonce::from_slice(&nonce(index + 1)),
                &chunk_aad(&encrypted.aad, self.size, index, length),
                &mut bytes,
            )
            .map_err(|_| anyhow!("encrypted asset record authentication failed; retained"))?;
        ensure!(
            bytes.len() == length,
            "encrypted asset record length invalid"
        );
        Ok(bytes)
    }

    pub(crate) fn read_range(&mut self, offset: u64, length: usize) -> Result<Vec<u8>> {
        ensure!(
            length > 0 && length <= CHUNK_BYTES * 5,
            "invalid asset range length"
        );
        ensure!(offset <= self.size, "retained asset range is out of bounds");
        let length = usize::try_from((self.size - offset).min(length as u64))?;
        let mut output = allocated(length)?;
        let mut copied = 0;
        while copied < length {
            let position = offset + copied as u64;
            let chunk = self.chunk(position / CHUNK_BYTES as u64)?;
            let within = usize::try_from(position % CHUNK_BYTES as u64)?;
            let n = (length - copied).min(chunk.len() - within);
            output[copied..copied + n].copy_from_slice(&chunk[within..within + n]);
            copied += n;
        }
        Ok(output)
    }

    fn check_digest(&self, digest: &[u8; 32]) -> Result<()> {
        ensure!(
            self.file.metadata()?.len() == self.physical_size,
            "retained asset changed size; retained"
        );
        ensure!(
            self.encrypted
                .as_ref()
                .is_none_or(|value| &value.sha256 == digest),
            "encrypted asset content digest changed; retained"
        );
        Ok(())
    }

    pub(crate) fn digest(&mut self) -> Result<(String, u64)> {
        let mut hash = Sha256::new();
        for index in 0..self.size.div_ceil(CHUNK_BYTES as u64) {
            hash.update(self.chunk(index)?);
        }
        let digest: [u8; 32] = hash.finalize().into();
        self.check_digest(&digest)?;
        Ok((hex_digest(&digest), self.size))
    }

    pub(crate) fn read_all(&mut self) -> Result<Vec<u8>> {
        let mut output = Vec::new();
        output
            .try_reserve_exact(usize::try_from(self.size)?)
            .map_err(|_| anyhow!("retained asset allocation refused"))?;
        let mut hash = Sha256::new();
        for index in 0..self.size.div_ceil(CHUNK_BYTES as u64) {
            let bytes = self.chunk(index)?;
            hash.update(&bytes);
            output.extend_from_slice(&bytes);
        }
        let digest = hash.finalize().into();
        self.check_digest(&digest)?;
        Ok(output)
    }
}

fn hex_digest(digest: &[u8; 32]) -> String {
    digest.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn write_chunks(
    path: &Path,
    size: u64,
    mut chunk: impl FnMut(u64) -> Result<Vec<u8>>,
    expected_digest: Option<[u8; 32]>,
) -> Result<()> {
    ensure!(
        is_encrypted_path(path) && size <= MAX_BYTES,
        "invalid encrypted asset destination or byte limit"
    );
    let id = *uuid::Uuid::new_v4().as_bytes();
    let cipher = cipher(&id)?; // No plaintext fallback and no file created without a key.
    let aad = aad(path, &id)?;
    let physical_bytes = size
        .checked_add(HEADER_BYTES as u64)
        .and_then(|bytes| bytes.checked_add(size.div_ceil(CHUNK_BYTES as u64) * TAG_BYTES as u64))
        .context("STORAGE_CAPACITY_INVALID: encrypted asset size overflow")?;
    let _storage = crate::storage_capacity::StorageLease::for_write(path, physical_bytes)?;
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options
        .open(path)
        .context("create encrypted asset failed")?;
    file.write_all(&[0; HEADER_BYTES])?;
    let chunks = size.div_ceil(CHUNK_BYTES as u64);
    let mut hash = Sha256::new();
    for index in 0..chunks {
        let mut bytes = chunk(index)?;
        let length = usize::try_from((size - index * CHUNK_BYTES as u64).min(CHUNK_BYTES as u64))?;
        ensure!(
            bytes.len() == length,
            "asset source size changed during staging"
        );
        hash.update(&bytes);
        bytes
            .try_reserve_exact(TAG_BYTES)
            .map_err(|_| anyhow!("retained asset allocation refused"))?;
        cipher
            .encrypt_in_place(
                Nonce::from_slice(&nonce(index + 1)),
                &chunk_aad(&aad, size, index, length),
                &mut bytes,
            )
            .map_err(|_| anyhow!("encrypt retained asset record failed"))?;
        file.write_all(&bytes)?;
    }
    let digest: [u8; 32] = hash.finalize().into();
    ensure!(
        expected_digest.is_none_or(|expected| expected == digest),
        "encrypted source asset content changed; retained"
    );
    let mut metadata = Vec::with_capacity(META_BYTES);
    metadata.extend_from_slice(&size.to_be_bytes());
    metadata.extend_from_slice(&digest);
    metadata.extend_from_slice(&(CHUNK_BYTES as u32).to_be_bytes());
    metadata.extend_from_slice(&chunks.to_be_bytes());
    let metadata = cipher
        .encrypt(
            Nonce::from_slice(&nonce(0)),
            Payload {
                msg: &metadata,
                aad: &aad,
            },
        )
        .map_err(|_| anyhow!("encrypt retained asset header failed"))?;
    file.seek(SeekFrom::Start(0))?;
    file.write_all(MAGIC)?;
    file.write_all(&id)?;
    file.write_all(&metadata)?;
    file.flush()?;
    file.sync_all().context("sync encrypted asset failed")?;
    #[cfg(unix)]
    File::open(path.parent().context("asset directory missing")?)?
        .sync_all()
        .context("sync encrypted asset directory failed")?;
    _storage.finish()?;
    Ok(())
}

pub(crate) fn write_bytes(path: &Path, bytes: &[u8]) -> Result<()> {
    write_chunks(
        path,
        u64::try_from(bytes.len())?,
        |index| {
            let offset = usize::try_from(index)? * CHUNK_BYTES;
            let end = bytes.len().min(offset + CHUNK_BYTES);
            let mut chunk = allocated(end - offset)?;
            chunk.copy_from_slice(&bytes[offset..end]);
            Ok(chunk)
        },
        None,
    )
}

pub(crate) fn copy(path: &Path, source: &mut AssetReader) -> Result<()> {
    let expected = source.encrypted.as_ref().map(|value| value.sha256);
    write_chunks(path, source.len(), |index| source.chunk(index), expected)?;
    ensure!(
        source.file.metadata()?.len() == source.physical_size,
        "source asset changed size during staging; retained"
    );
    Ok(())
}

pub(crate) fn read(path: &Path, max_bytes: u64) -> Result<Vec<u8>> {
    AssetReader::open(path, max_bytes)?.read_all()
}

pub(crate) fn filename(path: &Path) -> Option<&str> {
    let name = path.file_name()?.to_str()?;
    Some(name.strip_prefix(NAME_PREFIX).unwrap_or(name))
}

fn is_encrypted_path(path: &Path) -> bool {
    path.file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name.starts_with(NAME_PREFIX))
}

pub(crate) async fn read_async(path: &str, max_bytes: u64) -> Result<Vec<u8>> {
    let path = PathBuf::from(path);
    tokio::task::spawn_blocking(move || read(&path, max_bytes))
        .await
        .context("retained asset reader worker failed")?
}

pub(crate) async fn range_async(
    path: &str,
    offset: u64,
    length: usize,
    max_bytes: u64,
) -> Result<Vec<u8>> {
    let path = PathBuf::from(path);
    tokio::task::spawn_blocking(move || {
        AssetReader::open(&path, max_bytes)?.read_range(offset, length)
    })
    .await
    .context("retained asset reader worker failed")?
}
