use super::*;
use rusqlite::backup::{Backup, StepResult};
use std::{
    fs::{self, OpenOptions},
    path::Path,
};
const FAILED: &str = "stremio_backup_failed";

// No in-memory production bypass. Tests supply an isolated directory and still perform
// the actual SQLite online backup, integrity verification and durable file synchronization.
pub(super) fn create(db: &Connection, service: &Service) -> Result<(), Error> {
    let database = db.path().filter(|p| !p.is_empty());
    let directory = database
        .and_then(|p| Path::new(p).parent())
        .map(|p| p.join("stremio-import-backups"));
    #[cfg(test)]
    let directory = service.backup_directory.clone().or(directory);
    #[cfg(not(test))]
    let _ = service;
    let directory = directory.ok_or(FAILED)?;
    private_directory(&directory)?;
    let path = directory.join(format!("before-stremio-{}.sqlite", Uuid::new_v4().simple()));
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let file = options.open(&path).map_err(|_| FAILED)?;
    let mut destination = Connection::open(&path).map_err(|_| FAILED)?;
    {
        let backup = Backup::new(db, &mut destination).map_err(|_| FAILED)?;
        let started = Instant::now();
        loop {
            if matches!(backup.step(256).map_err(|_| FAILED)?, StepResult::Done) {
                break;
            }
            if started.elapsed() > Duration::from_secs(120) {
                return Err(FAILED.into());
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }
    let integrity: String = destination
        .query_row("PRAGMA quick_check", [], |row| row.get(0))
        .map_err(|_| FAILED)?;
    if integrity != "ok" {
        return Err(FAILED.into());
    }
    destination
        .execute_batch("PRAGMA journal_mode=DELETE")
        .map_err(|_| FAILED)?;
    destination.close().map_err(|_| FAILED)?;
    file.sync_all().map_err(|_| FAILED)?;
    #[cfg(unix)]
    fs::File::open(&directory)
        .and_then(|f| f.sync_all())
        .map_err(|_| FAILED)?;
    Ok(())
}
fn private_directory(path: &Path) -> Result<(), Error> {
    if !path.exists() {
        #[cfg(unix)]
        let mut builder = fs::DirBuilder::new();
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            builder.mode(0o700);
        }
        #[cfg(not(unix))]
        let builder = fs::DirBuilder::new();
        builder.create(path).map_err(|_| FAILED)?;
        #[cfg(windows)]
        {
            // Assign an owner-only protected DACL before any database bytes are written.
            // Path is passed through the environment, never interpolated into shell code.
            let status = std::process::Command::new("powershell.exe")
                .args(["-NoProfile", "-NonInteractive", "-Command", "$ErrorActionPreference='Stop'; $p=$env:VIPTV_IMPORT_BACKUP_DIRECTORY; $sid=[System.Security.Principal.WindowsIdentity]::GetCurrent().User; $acl=New-Object System.Security.AccessControl.DirectorySecurity; $acl.SetOwner($sid); $acl.SetAccessRuleProtection($true,$false); $rule=New-Object System.Security.AccessControl.FileSystemAccessRule($sid,'FullControl','ContainerInherit,ObjectInherit','None','Allow'); $acl.AddAccessRule($rule); Set-Acl -LiteralPath $p -AclObject $acl"])
                .env("VIPTV_IMPORT_BACKUP_DIRECTORY", path)
                .stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null())
                .status().map_err(|_| FAILED)?;
            if !status.success() {
                return Err(FAILED.into());
            }
        }
    }
    let metadata = fs::symlink_metadata(path).map_err(|_| FAILED)?;
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        return Err(FAILED.into());
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        if metadata.permissions().mode() & 0o077 != 0
            || metadata.uid() != unsafe { libc::geteuid() }
        {
            return Err(FAILED.into());
        }
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        if metadata.file_attributes() & 0x400 != 0 {
            return Err(FAILED.into());
        }
        let status = std::process::Command::new("powershell.exe")
            .args(["-NoProfile", "-NonInteractive", "-Command", "$ErrorActionPreference='Stop'; $acl=Get-Acl -LiteralPath $env:VIPTV_IMPORT_BACKUP_DIRECTORY; $sid=[System.Security.Principal.WindowsIdentity]::GetCurrent().User.Value; if (!$acl.AreAccessRulesProtected -or $acl.GetOwner([System.Security.Principal.SecurityIdentifier]).Value -ne $sid) {exit 1}; foreach ($rule in $acl.GetAccessRules($true,$true,[System.Security.Principal.SecurityIdentifier])) {if ($rule.IdentityReference.Value -ne $sid -or $rule.AccessControlType -ne 'Allow') {exit 1}}; if ($acl.Access.Count -eq 0) {exit 1}"])
            .env("VIPTV_IMPORT_BACKUP_DIRECTORY", path)
            .stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null())
            .status().map_err(|_| FAILED)?;
        if !status.success() {
            return Err(FAILED.into());
        }
    }
    #[cfg(not(any(unix, windows)))]
    return Err(FAILED.into());
    Ok(())
}
