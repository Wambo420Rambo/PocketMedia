//! Content hashes for duplicate detection. The app uses SHA-256 through Windows
//! CNG (`sha256.rs`); the import code takes any `Digest`, so its unit
//! tests can run anywhere.

/// A running hash over file contents.
pub trait Digest {
    fn update(&mut self, data: &[u8]);
    /// Lowercase hex of the result.
    fn finish(self: Box<Self>) -> String;
}

/// Makes a fresh digest (one per file).
pub type NewDigest = fn() -> Result<Box<dyn Digest>, String>;

pub fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

#[cfg(test)]
pub mod testing {
    use super::Digest;

    /// For tests that only need equal content to give equal hashes.
    pub struct Fnv(u64, u64);

    impl Digest for Fnv {
        fn update(&mut self, data: &[u8]) {
            for b in data {
                self.0 = (self.0 ^ *b as u64).wrapping_mul(0x100000001b3);
            }
            self.1 += data.len() as u64;
        }
        fn finish(self: Box<Self>) -> String {
            format!("{:016x}{:x}", self.0, self.1)
        }
    }

    pub fn fnv() -> Result<Box<dyn Digest>, String> {
        Ok(Box::new(Fnv(0xcbf29ce484222325, 0)))
    }
}
