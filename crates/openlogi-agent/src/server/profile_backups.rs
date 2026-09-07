use std::fs::{self, File, OpenOptions};
use std::io::{self, Read as _, Write as _};
use std::path::{Path, PathBuf};

use openlogi_core::hid::onboard_profile::{OnboardProfileBackup, ProfileEditId};

pub(super) struct ProfileBackups {
    directory: PathBuf,
}

impl ProfileBackups {
    pub(super) fn new(directory: PathBuf) -> Self {
        Self { directory }
    }

    pub(super) fn save(&self, id: ProfileEditId, backup: &OnboardProfileBackup) -> io::Result<()> {
        let text = backup.encode().map_err(io::Error::other)?;
        create_durable_directory(&self.directory)?;
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(self.path(id))?;
        file.write_all(text.as_bytes())?;
        file.sync_all()?;
        sync_directory(&self.directory)
    }

    pub(super) fn load(&self, id: ProfileEditId) -> io::Result<OnboardProfileBackup> {
        let mut text = String::new();
        File::open(self.path(id))?
            .take(1_000_001)
            .read_to_string(&mut text)?;
        OnboardProfileBackup::decode(&text).map_err(io::Error::other)
    }

    pub(super) fn list(&self) -> io::Result<Vec<(ProfileEditId, OnboardProfileBackup)>> {
        let entries = match fs::read_dir(&self.directory) {
            Ok(entries) => entries,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(error) => return Err(error),
        };
        let mut backups = Vec::new();
        for entry in entries {
            let entry = entry?;
            let Some(id) = entry.file_name().to_str().and_then(parse_name) else {
                continue;
            };
            if !entry.file_type()?.is_file() {
                continue;
            }
            match self.load(id) {
                Ok(backup) => backups.push((id, backup)),
                Err(error) => tracing::warn!(%id, %error, "profile backup is unreadable"),
            }
        }
        backups.sort_by_key(|(_, backup)| std::cmp::Reverse(backup.created_unix_seconds));
        Ok(backups)
    }

    fn path(&self, id: ProfileEditId) -> PathBuf {
        self.directory.join(format!("{id}.toml"))
    }
}

fn parse_name(name: &str) -> Option<ProfileEditId> {
    let (run, sequence) = name.strip_suffix(".toml")?.split_once('-')?;
    if run.len() != 16 || sequence.len() != 16 {
        return None;
    }
    let id = ProfileEditId {
        run: u64::from_str_radix(run, 16).ok()?,
        sequence: u64::from_str_radix(sequence, 16).ok()?,
    };
    (format!("{id}.toml") == name).then_some(id)
}

fn create_durable_directory(path: &Path) -> io::Result<()> {
    if !path.exists() {
        let parent = path
            .parent()
            .ok_or_else(|| io::Error::other("backup directory has no parent"))?;
        create_durable_directory(parent)?;
        match fs::create_dir(path) {
            Ok(()) => (),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => (),
            Err(error) => return Err(error),
        }
        sync_directory(parent)?;
    }
    sync_directory(path)
}

fn sync_directory(path: &Path) -> io::Result<()> {
    #[cfg(unix)]
    {
        File::open(path)?.sync_all()
    }
    #[cfg(not(unix))]
    {
        let _ = path;
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "durable profile backups are unavailable on this platform",
        ))
    }
}

#[cfg(test)]
#[cfg(unix)]
mod tests {
    use super::*;
    use openlogi_core::hid::{DeviceRoute, onboard_profile::OnboardProfileDescriptor};

    #[test]
    fn backup_is_durable_immutable_and_survives_store_restart() {
        let temporary = tempfile::tempdir().unwrap();
        let path = temporary.path().join("state/profiles");
        let store = ProfileBackups::new(path.clone());
        let id = ProfileEditId {
            run: 1,
            sequence: 2,
        };
        let backup = OnboardProfileBackup {
            format_version: 1,
            created_unix_seconds: 3,
            route: DeviceRoute::Direct {
                vendor_id: 0xff00,
                product_id: 0xabcd,
            },
            description: OnboardProfileDescriptor {
                memory_model: 1,
                profile_format: 1,
                macro_format: 1,
                profile_count: 1,
                rom_profile_count: 1,
                button_count: 3,
                sector_count: 2,
                sector_size: 256,
                mechanical_layout: 0,
                various_info: 0,
            },
            sector: 1,
            original: vec![0x5a; 256],
            updated: vec![0xa5; 256],
        };
        store.save(id, &backup).unwrap();
        assert_eq!(
            store.save(id, &backup).unwrap_err().kind(),
            io::ErrorKind::AlreadyExists
        );
        let reopened = ProfileBackups::new(path);
        assert_eq!(reopened.load(id).unwrap(), backup);
        assert_eq!(reopened.list().unwrap(), vec![(id, backup)]);
        assert_eq!(parse_name("../../elsewhere.toml"), None);
        assert_eq!(
            parse_name("0000000000000001-0000000000000002.toml"),
            Some(id)
        );
    }
}
