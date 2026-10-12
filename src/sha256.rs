//! SHA-256 through Windows CNG (BCrypt).

use windows::Win32::Security::Cryptography::{
    BCRYPT_ALG_HANDLE, BCRYPT_HASH_HANDLE, BCRYPT_OPEN_ALGORITHM_PROVIDER_FLAGS,
    BCRYPT_SHA256_ALGORITHM, BCryptCloseAlgorithmProvider, BCryptCreateHash, BCryptDestroyHash,
    BCryptFinishHash, BCryptHashData, BCryptOpenAlgorithmProvider,
};

use crate::digest::{Digest, hex};

/// Owns the CNG algorithm and hash handles; both are released on drop.
pub struct Sha256 {
    alg: BCRYPT_ALG_HANDLE,
    hash: BCRYPT_HASH_HANDLE,
}

impl Sha256 {
    fn create() -> Result<Self, String> {
        let mut alg = BCRYPT_ALG_HANDLE::default();
        // SAFETY: out-pointer to a local handle; a built-in algorithm id.
        unsafe {
            BCryptOpenAlgorithmProvider(
                &mut alg,
                BCRYPT_SHA256_ALGORITHM,
                None,
                BCRYPT_OPEN_ALGORITHM_PROVIDER_FLAGS(0),
            )
        }
        .ok()
        .map_err(|e| format!("SHA-256 is not available: {e}"))?;
        let mut hash = BCRYPT_HASH_HANDLE::default();
        // SAFETY: alg is open; CNG allocates the hash object itself (no buffer given).
        let created = unsafe { BCryptCreateHash(alg, &mut hash, None, None, 0) }.ok();
        if let Err(e) = created {
            // SAFETY: alg was opened above and is not used again.
            let _ = unsafe { BCryptCloseAlgorithmProvider(alg, 0) };
            return Err(format!("SHA-256 is not available: {e}"));
        }
        Ok(Self { alg, hash })
    }
}

impl Digest for Sha256 {
    fn update(&mut self, data: &[u8]) {
        // SAFETY: the hash handle is live; data is a valid slice for the call.
        let _ = unsafe { BCryptHashData(self.hash, data, 0) };
    }

    fn finish(self: Box<Self>) -> String {
        let mut out = [0u8; 32];
        // SAFETY: the hash handle is live; out is exactly SHA-256's 32 bytes.
        let ok = unsafe { BCryptFinishHash(self.hash, &mut out, 0) }.is_ok();
        if ok { hex(&out) } else { String::new() }
    }
}

impl Drop for Sha256 {
    fn drop(&mut self) {
        // SAFETY: both handles were created in `create` and are released once here.
        unsafe {
            let _ = BCryptDestroyHash(self.hash);
            let _ = BCryptCloseAlgorithmProvider(self.alg, 0);
        }
    }
}

/// A fresh SHA-256 digest (a `digest::NewDigest`).
pub fn new() -> Result<Box<dyn Digest>, String> {
    Ok(Box::new(Sha256::create()?))
}

#[cfg(test)]
mod tests {
    #[test]
    fn matches_the_standard_test_vector() {
        let mut d = super::new().unwrap();
        d.update(b"ab");
        d.update(b"c");
        assert_eq!(
            d.finish(),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }
}
