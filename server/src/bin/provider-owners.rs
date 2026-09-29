//! Deliberately separate from server startup; production execution needs approval.
use std::{io::Read, path::Path};
use viptv_server::migration_v2;

fn run() -> Result<serde_json::Value, &'static str> {
    let args: Vec<_> = std::env::args().skip(1).collect();
    match args.iter().map(String::as_str).collect::<Vec<_>>().as_slice() {
        ["inspect", database] => migration_v2::inspect(Path::new(database)),
        ["encrypt", database, backup, export, revision, "--confirm-encryption"] => migration_v2::encrypt(Path::new(database),Path::new(backup),Path::new(export),revision),
        ["apply", database, owners, backup, export, revision, "--confirm-ownership"] => {
            let metadata = std::fs::metadata(owners).map_err(|_| "owner_map_unavailable")?;
            if !metadata.is_file() { return Err("owner_map_must_be_a_regular_file"); }
            if metadata.len() > 1024 * 1024 { return Err("owner_map_too_large"); }
            let mut data = Vec::new();
            std::fs::File::open(owners).map_err(|_| "owner_map_unavailable")?.take(1024 * 1024 + 1).read_to_end(&mut data).map_err(|_| "owner_map_unavailable")?;
            let owners = migration_v2::parse_owner_map(&data)?;
            migration_v2::apply(Path::new(database), &owners, Path::new(backup), Path::new(export), revision)
        },
        _ => Err("usage: provider-owners inspect DATABASE | apply DATABASE OWNERS_JSON NEW_BACKUP NEW_EXPORT SOURCE_SHA --confirm-ownership | encrypt DATABASE NEW_BACKUP NEW_EXPORT SOURCE_SHA --confirm-encryption"),
    }
}
fn main() {
    match run() {
        Ok(report) => println!("{report}"),
        Err(code) => {
            eprintln!("Provider migration stopped: {code}. No automatic restore was attempted.");
            std::process::exit(1);
        }
    }
}
