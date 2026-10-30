//! Bounded byte-verification facts, never file handles or access decisions.
use opencut_contracts::EditorError;
use std::{collections::VecDeque, fs::Metadata, time::SystemTime};

pub const MAX_VERIFICATION_FACTS: usize = 128;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileIdentity {
    length: u64,
    modified: SystemTime,
    #[cfg(unix)]
    inode: [i128; 8],
}
impl FileIdentity {
    pub fn modified(&self) -> SystemTime {
        self.modified
    }
    pub fn of(metadata: Metadata) -> Result<Self, EditorError> {
        let changed = || EditorError::new("ASSET_INTEGRITY_ERROR", "File identity unavailable");
        if !metadata.is_file() {
            return Err(changed());
        }
        #[cfg(unix)]
        let inode = {
            use std::os::unix::fs::MetadataExt;
            [
                metadata.dev() as i128,
                metadata.ino() as i128,
                metadata.len() as i128,
                metadata.mtime() as i128,
                metadata.mtime_nsec() as i128,
                metadata.ctime() as i128,
                metadata.ctime_nsec() as i128,
                metadata.nlink() as i128,
            ]
        };
        Ok(Self {
            length: metadata.len(),
            modified: metadata.modified().map_err(|_| changed())?,
            #[cfg(unix)]
            inode,
        })
    }
}

/// Keys must contain the exact expected content and its storage namespace.
/// Callers open the current path and authorize every use independently.
/// Platforms without a change-time identity always perform full verification.
pub struct VerificationFacts<K>(VecDeque<(K, FileIdentity)>);
impl<K> Default for VerificationFacts<K> {
    fn default() -> Self {
        Self(VecDeque::new())
    }
}
impl<K: PartialEq> VerificationFacts<K> {
    pub fn contains(&mut self, key: &K, identity: &FileIdentity) -> bool {
        if !cfg!(unix) {
            return false;
        }
        let Some(index) = self.0.iter().position(|(k, i)| k == key && i == identity) else {
            return false;
        };
        let fact = self.0.remove(index).expect("located verification fact");
        self.0.push_back(fact);
        true
    }
    pub fn remember(&mut self, key: K, identity: FileIdentity) {
        if cfg!(unix) && !self.contains(&key, &identity) {
            if self.0.len() >= MAX_VERIFICATION_FACTS {
                self.0.pop_front();
            }
            self.0.push_back((key, identity));
        }
    }
    pub fn len(&self) -> usize {
        self.0.len()
    }
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}
