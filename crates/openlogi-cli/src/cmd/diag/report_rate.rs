use anyhow::{Context, Result};
use clap::Args;

use super::select_device;

#[derive(Debug, Args)]
pub struct ReportRateArgs {
    #[arg(long, value_name = "NAME", help = "Select a device by name")]
    pub device: Option<String>,
    #[arg(
        long,
        value_name = "MS",
        help = "Temporarily test this interval, then restore the original"
    )]
    pub test_interval: Option<u8>,
}

pub async fn run(args: ReportRateArgs) -> Result<()> {
    let (route, name) = select_device(args.device.as_deref(), &[0x8060]).await?;
    println!("device: {name} ({route})");
    let info = openlogi_hid::get_report_rate_info(&route)
        .await
        .context("read report rate")?;
    println!(
        "  supported intervals (ms): {:?}",
        info.supported_intervals_ms
    );
    println!("  current interval: {} ms", info.current_interval_ms);
    if let Some(target) = args.test_interval {
        anyhow::ensure!(
            info.supported_intervals_ms.contains(&target),
            "interval {target} ms is not advertised"
        );
        anyhow::ensure!(
            target != info.current_interval_ms,
            "test interval equals the current interval"
        );
        check_round_trip(
            info.current_interval_ms,
            target,
            async |value| {
                openlogi_hid::set_report_interval(&route, value)
                    .await
                    .context("set report interval")
            },
            async || {
                Ok(openlogi_hid::get_report_rate_info(&route)
                    .await?
                    .current_interval_ms)
            },
        )
        .await?;
    }
    Ok(())
}

async fn check_round_trip(
    before: u8,
    target: u8,
    set: impl AsyncFn(u8) -> Result<()>,
    get: impl AsyncFn() -> Result<u8>,
) -> Result<()> {
    let tested: Result<()> = async {
        println!("  writing interval: {target} ms");
        set(target).await?;
        let actual = get().await.context("read interval after write")?;
        anyhow::ensure!(actual == target, "expected {target} ms, read {actual} ms");
        println!("  read-back interval: {actual} ms");
        Ok(())
    }
    .await;
    let restored: Result<()> = async {
        println!("  restoring interval: {before} ms");
        let written = set(before).await;
        let actual = get().await.with_context(|| match &written {
            Ok(()) => "read interval after restoration".to_owned(),
            Err(error) => {
                format!("restore write failed: {error:#}; final interval read also failed")
            }
        })?;
        anyhow::ensure!(
            actual == before,
            "expected original {before} ms, read {actual} ms"
        );
        println!("  original interval confirmed: {actual} ms");
        written.context("restore write rejected, but original interval confirmed by readback")?;
        Ok(())
    }
    .await;
    match (tested, restored) {
        (Ok(()), Ok(())) => {
            println!("report-rate round-trip OK");
            Ok(())
        }
        (Err(test), Ok(())) => Err(test),
        (Ok(()), Err(restore)) => {
            Err(restore.context(format!("could not confirm original interval {before} ms")))
        }
        (Err(test), Err(restore)) => anyhow::bail!(
            "report-rate test failed: {test:#}; original interval {before} ms restoration failed: {restore:#}"
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::{Cell, RefCell};

    #[tokio::test]
    async fn rate_test_restores_after_failed_readback_and_checks_restoration() {
        for fail_test in [false, true] {
            for restored in [1, 2] {
                let writes = RefCell::new(Vec::new());
                let reads = Cell::new(0);
                let result = check_round_trip(
                    1,
                    2,
                    async |value| {
                        writes.borrow_mut().push(value);
                        Ok(())
                    },
                    async || {
                        let n = reads.get();
                        reads.set(n + 1);
                        if n == 0 && fail_test {
                            anyhow::bail!("read failed");
                        }
                        Ok(if n == 0 { 2 } else { restored })
                    },
                )
                .await;
                assert_eq!(*writes.borrow(), [2, 1]);
                assert_eq!(result.is_ok(), !fail_test && restored == 1);
            }
        }
    }
    #[tokio::test]
    async fn rejected_restore_write_still_reads_the_final_interval() {
        let reads = Cell::new(0);
        let error = check_round_trip(
            1,
            2,
            async |_| anyhow::bail!("write rejected"),
            async || {
                reads.set(reads.get() + 1);
                Ok(1)
            },
        )
        .await
        .unwrap_err();
        assert_eq!(reads.get(), 1);
        assert!(format!("{error:#}").contains("original interval confirmed"));
    }
}
