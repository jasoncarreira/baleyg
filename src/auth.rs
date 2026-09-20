//! Local bearer-token storage. Token values must never enter diagnostics.
use anyhow::{Result, ensure};
use std::{
    fs::OpenOptions,
    io::{Read, Write},
    path::Path,
};

pub fn valid_token(token: &str) -> bool {
    token.len() == 64
        && token
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}
pub fn load_or_create_token(path: &Path) -> Result<String> {
    #[cfg(not(unix))]
    anyhow::bail!("secure token storage requires Unix permissions");
    #[cfg(unix)]
    {
        use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
        let mut options = OpenOptions::new();
        options
            .write(true)
            .create_new(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW);
        match options.open(path) {
            Ok(mut file) => {
                let bytes: [u8; 32] = rand::random();
                let token = hex::encode(bytes);
                file.write_all(token.as_bytes())?;
                file.sync_all()?;
                Ok(token)
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                let mut file = OpenOptions::new()
                    .read(true)
                    .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
                    .open(path)?;
                let meta = file.metadata()?;
                ensure!(
                    meta.is_file()
                        && meta.mode() & 0o777 == 0o600
                        && meta.uid() == unsafe { libc::geteuid() }
                        && meta.nlink() == 1,
                    "insecure token file"
                );
                let mut token = String::new();
                std::io::Read::by_ref(&mut file)
                    .take(65)
                    .read_to_string(&mut token)?;
                ensure!(valid_token(&token), "invalid token file");
                Ok(token)
            }
            Err(e) => Err(e.into()),
        }
    }
}
