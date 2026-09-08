//! In-memory module loading (Phase 8). Linux first via memfd_create.
//!
//! Enable with feature `mem-module`. Downloads encrypted module, verifies
//! signature, decrypts in memory, loads via dlopen without touching disk.

use anyhow::{anyhow, Context, Result};
use std::path::Path;

use crate::signing;

/// Encrypted module package header (after signature verification).
#[derive(Debug, Clone)]
pub struct EncryptedModule {
    pub name: String,
    pub ciphertext: Vec<u8>,
    pub nonce: [u8; 12],
}

/// Verify module signature then decrypt with session-derived key.
pub fn verify_and_decrypt_module(
    encrypted_path: &Path,
    sig_path: &Path,
    decryption_key: &[u8; 32],
) -> Result<Vec<u8>> {
    signing::verify_release_artifact(
        &encrypted_path.display().to_string(),
        &sig_path.display().to_string(),
    )?;
    let raw = std::fs::read(encrypted_path).context("read encrypted module")?;
    if raw.len() < 12 {
        return Err(anyhow!("module payload too short"));
    }
    decrypt_aes_gcm(&raw, decryption_key)
}

fn decrypt_aes_gcm(payload: &[u8], key: &[u8; 32]) -> Result<Vec<u8>> {
    use aes_gcm::aead::{Aead, KeyInit};
    use aes_gcm::{Aes256Gcm, Nonce};
    if payload.len() < 12 {
        return Err(anyhow!("invalid ciphertext"));
    }
    let (nonce_bytes, ct) = payload.split_at(12);
    let mut nonce = [0u8; 12];
    nonce.copy_from_slice(nonce_bytes);
    let cipher = Aes256Gcm::new_from_slice(key).map_err(|_| anyhow!("cipher init"))?;
    cipher
        .decrypt(Nonce::from_slice(&nonce), ct)
        .map_err(|_| anyhow!("module decrypt failed"))
}

/// Load verified module bytes into memory (Linux: memfd + dlopen).
#[cfg(all(unix, feature = "mem-module"))]
pub fn load_module_in_memory(name: &str, elf_bytes: &[u8]) -> Result<*mut libc::c_void> {
    use std::ffi::CString;
    use std::ptr;

    unsafe {
        let c_name = CString::new(name).map_err(|_| anyhow!("invalid module name"))?;
        let fd = libc::memfd_create(c_name.as_ptr(), 0);
        if fd < 0 {
            return Err(anyhow!("memfd_create failed"));
        }
        let written = libc::write(fd, elf_bytes.as_ptr() as *const _, elf_bytes.len());
        if written < 0 || written as usize != elf_bytes.len() {
            libc::close(fd);
            return Err(anyhow!("memfd write failed"));
        }

        let path = format!("/proc/self/fd/{fd}");
        let c_path = CString::new(path).map_err(|_| anyhow!("invalid fd path"))?;
        let handle = libc::dlopen(c_path.as_ptr(), libc::RTLD_NOW | libc::RTLD_LOCAL);
        if handle.is_null() {
            let err = libc::dlerror();
            let msg = if err.is_null() {
                "unknown dlopen error".to_string()
            } else {
                std::ffi::CStr::from_ptr(err).to_string_lossy().into_owned()
            };
            libc::close(fd);
            return Err(anyhow!("dlopen failed: {msg}"));
        }
        Ok(handle)
    }
}

#[cfg(not(all(unix, feature = "mem-module")))]
pub fn load_module_in_memory(_name: &str, _elf_bytes: &[u8]) -> Result<()> {
    Err(anyhow!(
        "in-memory module loading requires unix + mem-module feature"
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decrypt_rejects_short_payload() {
        assert!(decrypt_aes_gcm(&[1, 2, 3], &[0u8; 32]).is_err());
    }
}
