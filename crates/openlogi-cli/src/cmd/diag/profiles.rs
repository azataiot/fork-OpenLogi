use anyhow::{Context, Result};
use clap::Args;

use super::select_device;

#[derive(Debug, Args)]
pub struct ProfilesArgs {
    #[arg(long, value_name = "NAME", help = "Select a device by name")]
    pub device: Option<String>,
}

pub async fn run(args: ProfilesArgs) -> Result<()> {
    let (route, name) = select_device(args.device.as_deref(), &[0x8100]).await?;
    println!("device: {name} ({route})");
    let snapshot = openlogi_hid::dump_onboard_profiles(&route)
        .await
        .context("read onboard profiles")?;
    println!("  descriptor: {:?}", snapshot.description);
    println!(
        "  mode: {:?}, active sector: {}",
        snapshot.mode, snapshot.active_sector
    );
    println!("  {} user profiles read", snapshot.profiles.len());
    for (entry, profile) in snapshot.profiles {
        println!("  sector {} enabled={}", entry.sector, entry.enabled);
        println!("    report interval: {} ms", profile.report_interval_ms);
        println!(
            "    DPI stages: {:?}, default index: {}, shift index: {}",
            profile.dpi_stages, profile.default_dpi_index, profile.shift_dpi_index
        );
        for (index, assignment) in profile.assignments.iter().enumerate() {
            println!("    button {}: {assignment:02x?}", index + 1);
        }
        println!("    raw sector (checksum verified):");
        for chunk in profile.raw.chunks(16) {
            println!("      {chunk:02x?}");
        }
    }
    Ok(())
}
