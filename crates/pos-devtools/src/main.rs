//! `pos-dev`: reset or seed a till database (E00-T04). Run it through xtask:
//!
//! ```text
//! cargo xtask db-reset [--dir DIR] --yes
//! cargo xtask seed --profile demo20|shop5k|bench50k [--seed N] [--dir DIR] [--yes]
//! ```
//!
//! `DIR` defaults to the desktop app's own data folder on this computer. Close the app first.
//! Deleting an existing database always needs `--yes`.

use anyhow::{Result, bail};
use pos_devtools::{Profile, STAFF, app_data_dir, database_exists, delete, reset, seed};
use std::path::PathBuf;

struct Args {
    command: String,
    dir: Option<PathBuf>,
    profile: Option<String>,
    seed: u64,
    yes: bool,
}

fn args() -> Result<Args> {
    let mut it = std::env::args().skip(1);
    let mut a = Args {
        command: it.next().unwrap_or_default(),
        dir: None,
        profile: None,
        seed: 1,
        yes: false,
    };
    while let Some(arg) = it.next() {
        let mut value = || it.next().ok_or_else(|| anyhow::anyhow!("{arg} needs a value"));
        match arg.as_str() {
            "--dir" => a.dir = Some(PathBuf::from(value()?)),
            "--profile" => a.profile = Some(value()?),
            "--seed" => a.seed = value()?.parse()?,
            "--yes" => a.yes = true,
            other => bail!("unknown argument {other}"),
        }
    }
    Ok(a)
}

#[tokio::main]
async fn main() -> Result<()> {
    let a = args()?;
    let dir = match a.dir {
        Some(d) => d,
        None => app_data_dir()?,
    };
    let exists = database_exists(&dir);
    if exists && !a.yes {
        bail!(
            "{} already has a till database. Close the app, then run again with --yes to delete it \
             (its backups folder is kept).",
            dir.display()
        );
    }
    match a.command.as_str() {
        "reset" => {
            let path = reset(&dir).await?;
            println!(
                "empty database at {} (the app opens the setup wizard)",
                path.display()
            );
        }
        "seed" => {
            let Some(profile) = a.profile else {
                bail!("seed needs --profile demo20|shop5k|bench50k");
            };
            let profile = Profile::parse(&profile)?;
            if exists {
                delete(&dir)?;
            }
            let s = seed(&dir, profile, a.seed).await?;
            println!(
                "seeded {} ({profile:?}, seed {}): {} products, {} variants, {} barcodes in {:.1} s",
                dir.display(),
                a.seed,
                s.products,
                s.variants,
                s.barcodes,
                s.elapsed.as_secs_f64()
            );
            println!("sign in as:");
            for (name, role, pin) in STAFF {
                println!("  {name:<8} {role:?}, PIN {pin}");
            }
        }
        other => {
            bail!("usage: pos-dev reset|seed [--dir DIR] [--profile P] [--seed N] [--yes] (got {other:?})")
        }
    }
    Ok(())
}
