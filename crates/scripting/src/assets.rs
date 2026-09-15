//! Image asset resolution, and storage under SHA256 content addresses.
//!
//! The implementation of Phase 2-2 of the development plan.
//!
//! # Why content addressing
//!
//! - **Deduplication**: capturing the same-looking button in two scripts still
//!   stores one file
//! - **Git friendliness**: the file name does not change unless the content
//!   does, so a diff appears only when an image was recaptured
//! - **Stable references**: recapturing `ok_button.png` changes what it holds,
//!   whereas a hash reference records which appearance was meant
//!
//! Relative path references work alongside this. Paths read better when writing
//! by hand, so the intended workflow is for the IDE to swap them for hashes.
//!
//! # Reference resolution rules
//!
//! See [`AssetStore::resolve`]. The key point is that **no implicit locations
//! are searched**: the string you wrote determines the location.

use std::collections::HashMap;
use std::fmt;
use std::path::{Path, PathBuf};

use mekiki_core::{Mekiki, Pattern};
use sha2::{Digest, Sha256};

/// The prefix of a content-address reference.
pub const HASH_PREFIX: &str = "sha256:";

#[derive(Debug)]
pub enum AssetError {
    NotFound {
        reference: String,
        tried: Vec<PathBuf>,
    },
    Io(String),
    Decode(String),
    BadHash(String),
}

impl fmt::Display for AssetError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotFound { reference, tried } => {
                write!(f, "image '{reference}' not found. Searched:")?;
                for p in tried {
                    write!(f, "\n    {}", p.display())?;
                }
                Ok(())
            }
            Self::Io(e) => write!(f, "I/O error: {e}"),
            Self::Decode(e) => write!(f, "cannot read the image: {e}"),
            Self::BadHash(h) => write!(f, "malformed hash: {h}"),
        }
    }
}

impl std::error::Error for AssetError {}

/// Where image assets live, and how references resolve.
pub struct AssetStore {
    /// The base for relative path references, normally the script's directory.
    base: PathBuf,
    /// The content-addressed store.
    store: PathBuf,
    /// A cache of resolved patterns.
    ///
    /// Building a pattern builds a pyramid, which is not cheap. Calling
    /// `target("image:ok.png")` inside a loop is a natural thing to write, and
    /// without this cache it would rebuild the pattern every iteration.
    cache: HashMap<PathBuf, Pattern>,
}

impl AssetStore {
    /// `base` is where relative references start. The store is
    /// `base/.mekiki/images`.
    pub fn new(base: impl Into<PathBuf>) -> Self {
        let base = base.into();
        let store = base.join(".mekiki").join("images");
        Self {
            base,
            store,
            cache: HashMap::new(),
        }
    }

    pub fn base(&self) -> &Path {
        &self.base
    }

    pub fn store_dir(&self) -> &Path {
        &self.store
    }

    /// Resolve an image reference to a real file.
    ///
    /// There are only three rules.
    ///
    /// 1. `sha256:<hex>` — look in the store (`base/.mekiki/images`)
    /// 2. An absolute path — used as-is
    /// 3. Anything else — **a path relative to the base**. `ok.png`,
    ///    `parts/ok.png` and `../shared/ok.png` all work
    ///
    /// # Why no implicit locations are searched
    ///
    /// This used to quietly look in `images/` and `assets/` subdirectories too.
    /// That was dropped because **the same reference could point at different
    /// files depending on layout**, which invites accidents. Writing `ok.png`
    /// when a file of that name exists both directly under the base and in
    /// `images/` leaves the choice to search order, an implementation detail.
    ///
    /// To collect images in another directory, write `parts/ok.png`; that points
    /// where it reads as pointing.
    pub fn resolve(&self, reference: &str) -> Result<PathBuf, AssetError> {
        if let Some(hex) = reference.strip_prefix(HASH_PREFIX) {
            let path = self.path_for_hash(hex)?;
            if path.is_file() {
                return Ok(path);
            }
            return Err(AssetError::NotFound {
                reference: reference.to_string(),
                tried: vec![path],
            });
        }

        // An absolute path is used as-is. Joining it to the base gives the same
        // result, but separating it here keeps the "searched" list sensible when
        // nothing is found.
        let as_is = PathBuf::from(reference);
        if as_is.is_absolute() {
            return if as_is.is_file() {
                Ok(as_is)
            } else {
                Err(AssetError::NotFound {
                    reference: reference.to_string(),
                    tried: vec![as_is],
                })
            };
        }

        let direct = self.base.join(reference);
        if direct.is_file() {
            return Ok(direct);
        }

        Err(AssetError::NotFound {
            reference: reference.to_string(),
            tried: vec![direct],
        })
    }

    /// Get a pattern from a reference. Later calls return the cached one.
    pub fn pattern(&mut self, mekiki: &Mekiki, reference: &str) -> Result<Pattern, AssetError> {
        let path = self.resolve(reference)?;
        if let Some(p) = self.cache.get(&path) {
            return Ok(p.clone());
        }

        let pattern = mekiki
            .pattern_from_file(&path)
            .map_err(|e| AssetError::Decode(e.to_string()))?;
        self.cache.insert(path, pattern.clone());
        Ok(pattern)
    }

    /// Drop the cache. Used after recapturing an image.
    pub fn clear_cache(&mut self) {
        self.cache.clear();
    }

    /// Import a file into the store and return a content-address reference.
    ///
    /// If identical content already exists, nothing is written — that is the
    /// deduplication.
    pub fn import(&self, path: impl AsRef<Path>) -> Result<String, AssetError> {
        let path = path.as_ref();
        let bytes =
            std::fs::read(path).map_err(|e| AssetError::Io(format!("{}: {e}", path.display())))?;
        self.import_bytes(&bytes)
    }

    /// Import bytes into the store.
    pub fn import_bytes(&self, bytes: &[u8]) -> Result<String, AssetError> {
        let hex = hash_hex(bytes);
        let dest = self.path_for_hash(&hex)?;

        if dest.is_file() {
            // Identical content need not be rewritten. This is the deduplication.
            return Ok(format!("{HASH_PREFIX}{hex}"));
        }

        if let Some(parent) = dest.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| AssetError::Io(format!("{}: {e}", parent.display())))?;
        }
        std::fs::write(&dest, bytes)
            .map_err(|e| AssetError::Io(format!("{}: {e}", dest.display())))?;

        Ok(format!("{HASH_PREFIX}{hex}"))
    }

    /// Build the in-store path from a hash.
    ///
    /// The first two characters become a directory so that tens of thousands of
    /// files do not end up in one directory — the same reasoning as Git's object
    /// store.
    fn path_for_hash(&self, hex: &str) -> Result<PathBuf, AssetError> {
        if hex.len() != 64 || !hex.chars().all(|c| c.is_ascii_hexdigit()) {
            return Err(AssetError::BadHash(hex.to_string()));
        }
        Ok(self
            .store
            .join(&hex[..2])
            .join(format!("{}.png", &hex[2..])))
    }
}

/// The SHA256 of some bytes, as lowercase hex.
pub fn hash_hex(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    let mut out = String::with_capacity(64);
    for b in digest {
        out.push_str(&format!("{b:02x}"));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("mekiki-assets-test-{name}"));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn hash_is_stable_and_64_hex() {
        let h = hash_hex(b"hello");
        assert_eq!(h.len(), 64);
        assert!(h.chars().all(|c| c.is_ascii_hexdigit()));
        // The known SHA256("hello").
        assert_eq!(
            h,
            "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824"
        );
    }

    #[test]
    fn import_is_idempotent_and_deduplicates() {
        let dir = temp_dir("dedup");
        let store = AssetStore::new(&dir);

        let a = store.import_bytes(b"same content").unwrap();
        let b = store.import_bytes(b"same content").unwrap();
        assert_eq!(a, b, "identical content should give an identical reference");

        let c = store.import_bytes(b"different").unwrap();
        assert_ne!(a, c);

        // The store should hold exactly two files.
        let count = walk_count(store.store_dir());
        assert_eq!(count, 2, "deduplication is not working");
    }

    fn walk_count(dir: &Path) -> usize {
        let mut n = 0;
        if let Ok(entries) = std::fs::read_dir(dir) {
            for e in entries.flatten() {
                let p = e.path();
                if p.is_dir() {
                    n += walk_count(&p);
                } else {
                    n += 1;
                }
            }
        }
        n
    }

    #[test]
    fn imported_content_can_be_resolved_back() {
        let dir = temp_dir("roundtrip");
        let store = AssetStore::new(&dir);
        let reference = store.import_bytes(b"payload").unwrap();
        let path = store.resolve(&reference).unwrap();
        assert_eq!(std::fs::read(path).unwrap(), b"payload");
    }

    /// References are relative to the base, and subdirectories work if written.
    #[test]
    fn relative_paths_resolve_from_base() {
        let dir = temp_dir("relative");
        std::fs::write(dir.join("a.png"), b"x").unwrap();
        std::fs::create_dir_all(dir.join("parts")).unwrap();
        std::fs::write(dir.join("parts").join("b.png"), b"y").unwrap();

        let store = AssetStore::new(&dir);
        assert!(store.resolve("a.png").is_ok());
        assert!(store.resolve("parts/b.png").is_ok());
    }

    /// No implicit locations are searched.
    ///
    /// This used to quietly look in `images/` and `assets/`. That was dropped
    /// because the same reference could point at different files depending on
    /// layout, so this pins **only going where it was written to go**.
    #[test]
    fn conventional_subdirectories_are_not_searched() {
        let dir = temp_dir("no-implicit-dirs");
        for sub in ["images", "assets"] {
            std::fs::create_dir_all(dir.join(sub)).unwrap();
            std::fs::write(dir.join(sub).join("b.png"), b"y").unwrap();
        }

        let store = AssetStore::new(&dir);
        assert!(
            store.resolve("b.png").is_err(),
            "an implicit location was searched"
        );
        // Stated explicitly, it resolves as expected.
        assert!(store.resolve("images/b.png").is_ok());
        assert!(store.resolve("assets/b.png").is_ok());
    }

    /// When nothing is found, report where it looked. Without that, there is no
    /// telling whether the location or the name is wrong.
    #[test]
    fn missing_asset_reports_the_searched_path() {
        let dir = temp_dir("missing");
        let store = AssetStore::new(&dir);
        let err = store.resolve("nope.png").unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("nope.png"), "{msg}");
        assert!(msg.contains("Searched"), "{msg}");
        assert!(
            msg.contains(&dir.display().to_string()),
            "the base is not shown: {msg}"
        );
    }

    /// Absolute paths are accepted as-is.
    #[test]
    fn absolute_paths_are_used_as_is() {
        let dir = temp_dir("absolute");
        let file = dir.join("abs.png");
        std::fs::write(&file, b"x").unwrap();

        // Put the base somewhere entirely different.
        let store = AssetStore::new(temp_dir("absolute-elsewhere"));
        assert_eq!(store.resolve(file.to_str().unwrap()).unwrap(), file);
    }

    #[test]
    fn malformed_hash_is_rejected() {
        let dir = temp_dir("badhash");
        let store = AssetStore::new(&dir);
        assert!(store.resolve("sha256:zzzz").is_err());
        assert!(store.resolve("sha256:abcd").is_err(), "too short");
    }
}
