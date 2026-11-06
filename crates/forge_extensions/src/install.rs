//! Unpacking extension archives into `<root>/installed/<id>/`.
//!
//! An archive is untrusted input. Only regular files and directories are
//! unpacked, nothing may land outside the target directory, and the id in the
//! archive's `extension.toml` must be the one that was asked for.

use crate::manifest::{Manifest, valid_id};
use crate::Error;
use flate2::read::GzDecoder;
use std::fs;
use std::path::{Path, PathBuf};
use tar::{Archive, EntryType};

/// Cap on the unpacked size, against archives that expand enormously.
const MAX_UNPACKED_BYTES: u64 = 512 * 1024 * 1024;

pub struct Installer {
    installed: PathBuf,
    staging: PathBuf,
}

impl Installer {
    /// Extensions live in `<root>/installed`; downloads are unpacked in `<root>/.staging` first.
    pub fn new(root: impl AsRef<Path>) -> Self {
        let root = root.as_ref();
        Installer { installed: root.join("installed"), staging: root.join(".staging") }
    }

    /// The directory an installed extension lives in.
    pub fn dir(&self, id: &str) -> PathBuf {
        self.installed.join(id)
    }

    /// Unpacks `archive` as extension `expected_id`, replacing any installed version.
    /// The old version is kept until the new one is in place.
    pub fn install(&self, expected_id: &str, archive: &[u8]) -> Result<Manifest, Error> {
        if !valid_id(expected_id) {
            return Err(Error::InvalidId(expected_id.to_owned()));
        }
        fs::create_dir_all(&self.staging)?;
        fs::create_dir_all(&self.installed)?;
        let work = tempfile::tempdir_in(&self.staging)?;
        let fresh = work.path().join("new");
        fs::create_dir(&fresh)?;
        unpack(archive, &fresh)?;

        let text = fs::read_to_string(fresh.join("extension.toml")).map_err(|_| Error::Archive("no extension.toml in the archive".into()))?;
        let manifest = Manifest::parse(&text).map_err(Error::Archive)?;
        if manifest.id != expected_id {
            return Err(Error::Archive(format!("the archive holds {:?}, not {expected_id:?}", manifest.id)));
        }

        let target = self.dir(expected_id);
        let backup = work.path().join("old");
        let had_old = target.exists();
        if had_old {
            fs::rename(&target, &backup)?;
        }
        if let Err(e) = fs::rename(&fresh, &target) {
            if had_old {
                let _ = fs::rename(&backup, &target);
            }
            return Err(e.into());
        }
        Ok(manifest)
    }

    /// Removes an installed extension. `false` if it was not installed.
    pub fn uninstall(&self, id: &str) -> Result<bool, Error> {
        if !valid_id(id) {
            return Err(Error::InvalidId(id.to_owned()));
        }
        let dir = self.dir(id);
        if !dir.is_dir() {
            return Ok(false);
        }
        fs::remove_dir_all(dir)?;
        Ok(true)
    }

    /// The manifest of one installed extension.
    pub fn manifest(&self, id: &str) -> Option<Manifest> {
        if !valid_id(id) {
            return None;
        }
        Manifest::parse(&fs::read_to_string(self.dir(id).join("extension.toml")).ok()?).ok()
    }

    /// Every installed extension, by id. Directories without a readable manifest are skipped.
    pub fn installed(&self) -> Vec<Manifest> {
        let Ok(entries) = fs::read_dir(&self.installed) else { return Vec::new() };
        let mut all: Vec<Manifest> = entries
            .flatten()
            .filter_map(|e| e.file_name().to_str().and_then(|id| self.manifest(id)))
            .collect();
        all.sort_by(|a, b| a.id.cmp(&b.id));
        all
    }
}

fn unpack(archive: &[u8], dest: &Path) -> Result<(), Error> {
    let mut archive = Archive::new(GzDecoder::new(archive));
    let mut total = 0u64;
    for entry in archive.entries().map_err(|e| Error::Archive(format!("not a .tar.gz archive: {e}")))? {
        let mut entry = entry.map_err(|e| Error::Archive(e.to_string()))?;
        match entry.header().entry_type() {
            EntryType::Regular | EntryType::Directory => {}
            // The archive's own root directory entry and PAX headers are harmless.
            EntryType::XGlobalHeader | EntryType::XHeader => continue,
            other => return Err(Error::Archive(format!("unsupported entry {:?} of type {other:?}", entry.path().unwrap_or_default()))),
        }
        total += entry.size();
        if total > MAX_UNPACKED_BYTES {
            return Err(Error::Archive("the archive unpacks to more than 512 MiB".into()));
        }
        if !entry.unpack_in(dest).map_err(|e| Error::Archive(e.to_string()))? {
            return Err(Error::Archive(format!("entry {:?} points outside the extension", entry.path().unwrap_or_default())));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use flate2::{Compression, write::GzEncoder};

    fn archive(files: &[(&str, &str)]) -> Vec<u8> {
        let mut tar = tar::Builder::new(GzEncoder::new(Vec::new(), Compression::fast()));
        for (path, body) in files {
            let mut header = tar::Header::new_gnu();
            header.set_size(body.len() as u64);
            header.set_mode(0o644);
            tar.append_data(&mut header, path, body.as_bytes()).unwrap();
        }
        tar.into_inner().unwrap().finish().unwrap()
    }

    fn manifest(id: &str, version: &str) -> String {
        format!("id = \"{id}\"\nname = \"N\"\nversion = \"{version}\"\nthemes = [\"themes/a.json\"]\n")
    }

    fn installer() -> (tempfile::TempDir, Installer) {
        let dir = tempfile::tempdir().unwrap();
        let installer = Installer::new(dir.path());
        (dir, installer)
    }

    #[test]
    fn installing_unpacks_the_archive_and_lists_it() {
        let (_d, inst) = installer();
        let m = inst.install("pretty", &archive(&[("extension.toml", &manifest("pretty", "1.0.0")), ("themes/a.json", "{}")])).unwrap();
        assert_eq!(m.version, "1.0.0");
        assert_eq!(fs::read_to_string(inst.dir("pretty").join("themes/a.json")).unwrap(), "{}");
        assert_eq!(inst.installed().iter().map(|m| m.id.as_str()).collect::<Vec<_>>(), ["pretty"]);
        assert_eq!(inst.manifest("pretty").unwrap().version, "1.0.0");
    }

    #[test]
    fn installing_again_replaces_the_files_of_the_old_version() {
        let (_d, inst) = installer();
        inst.install("pretty", &archive(&[("extension.toml", &manifest("pretty", "1.0.0")), ("old.txt", "x")])).unwrap();
        inst.install("pretty", &archive(&[("extension.toml", &manifest("pretty", "2.0.0")), ("themes/a.json", "{}")])).unwrap();
        assert_eq!(inst.manifest("pretty").unwrap().version, "2.0.0");
        assert!(!inst.dir("pretty").join("old.txt").exists());
    }

    #[test]
    fn a_bad_archive_leaves_the_installed_version_alone() {
        let (_d, inst) = installer();
        inst.install("pretty", &archive(&[("extension.toml", &manifest("pretty", "1.0.0"))])).unwrap();
        // Wrong id inside, no manifest, and not an archive at all.
        assert!(matches!(inst.install("pretty", &archive(&[("extension.toml", &manifest("other", "9.0.0"))])), Err(Error::Archive(_))));
        assert!(matches!(inst.install("pretty", &archive(&[("themes/a.json", "{}")])), Err(Error::Archive(_))));
        assert!(matches!(inst.install("pretty", b"not gzip"), Err(Error::Archive(_))));
        assert_eq!(inst.manifest("pretty").unwrap().version, "1.0.0");
    }

    #[test]
    fn paths_that_escape_and_links_are_refused() {
        let (dir, inst) = installer();
        // `append_data` refuses `..`, so write the name straight into the header.
        let mut tar = tar::Builder::new(GzEncoder::new(Vec::new(), Compression::fast()));
        let mut header = tar::Header::new_gnu();
        header.set_size(1);
        header.set_mode(0o644);
        let name = b"../escaped.txt";
        header.as_old_mut().name[..name.len()].copy_from_slice(name);
        header.set_cksum();
        tar.append(&header, &b"x"[..]).unwrap();
        let evil = tar.into_inner().unwrap().finish().unwrap();
        assert!(matches!(inst.install("pretty", &evil), Err(Error::Archive(_))));
        assert!(!dir.path().join("escaped.txt").exists() && !dir.path().join(".staging/escaped.txt").exists());

        let mut tar = tar::Builder::new(GzEncoder::new(Vec::new(), Compression::fast()));
        let mut header = tar::Header::new_gnu();
        header.set_entry_type(EntryType::Symlink);
        header.set_size(0);
        tar.append_link(&mut header, "link", "/etc/passwd").unwrap();
        let linked = tar.into_inner().unwrap().finish().unwrap();
        assert!(matches!(inst.install("pretty", &linked), Err(Error::Archive(_))));
    }

    #[test]
    fn uninstalling_removes_the_directory() {
        let (_d, inst) = installer();
        inst.install("pretty", &archive(&[("extension.toml", &manifest("pretty", "1.0.0"))])).unwrap();
        assert!(inst.uninstall("pretty").unwrap());
        assert!(!inst.uninstall("pretty").unwrap());
        assert!(inst.installed().is_empty());
        assert!(matches!(inst.uninstall("../x"), Err(Error::InvalidId(_))));
    }
}
