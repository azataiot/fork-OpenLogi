use anyhow::{Context, Result};
use clap::Args;

use super::select_device;

#[derive(Debug, Args)]
pub struct MonochromeArgs {
    #[arg(long, value_name = "NAME", help = "Select a device by name")]
    pub device: Option<String>,
}

pub async fn run(args: MonochromeArgs) -> Result<()> {
    let (route, name) = select_device(args.device.as_deref(), &[0x1300]).await?;
    println!("device: {name} ({route})");
    let snapshot = openlogi_hid::dump_monochrome_leds(&route)
        .await
        .context("read monochrome LED descriptors and state")?;
    println!("  software control: {}", snapshot.software_control);
    for (info, state) in snapshot.leds {
        println!("  descriptor: {info:?}");
        println!("  state: {state:?}");
    }
    Ok(())
}
