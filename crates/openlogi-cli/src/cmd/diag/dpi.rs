//! `openlogi diag dpi` — DPI write round-trip.

use std::fmt;

use anyhow::{Context, Result};
use clap::Args;
use openlogi_hid::{Dpi, DpiCapabilities, DpiInfo};

use crate::cmd::diag::select_device;

#[derive(Debug, Args)]
pub struct DpiArgs {
    /// DPI to set during the test. Must be one of the values reported by the
    /// device's HID++ AdjustableDpi feature.
    #[arg(long)]
    pub target: Option<u16>,

    /// Run against the device whose name contains this string
    /// (case-insensitive) instead of auto-selecting. Useful when several
    /// devices are paired (e.g. a mouse and a keyboard over Bluetooth).
    #[arg(long, value_name = "NAME")]
    pub device: Option<String>,
}

pub async fn run(args: DpiArgs) -> Result<()> {
    // 0x2201 AdjustableDpi / 0x2202 ExtendedAdjustableDpi — auto-skip devices
    // (keyboards) that expose neither. Newer mice ship only 0x2202.
    let (route, name) = select_device(args.device.as_deref(), &[0x2201, 0x2202]).await?;
    println!("device: {name} ({route})");

    let info = openlogi_hid::get_dpi_info(&route)
        .await
        .context("read DPI capabilities")?;
    let before = info.current;
    println!("  current DPI: {before}");
    println!("  supported DPI: {}", DpiSummaryDisplay(&info.capabilities));

    let target = match args.target {
        Some(target) => {
            let target = target.into();
            if !info.capabilities.contains(target) {
                anyhow::bail!(
                    "target {target} is not in the device-reported DPI list ({})",
                    DpiSummaryDisplay(&info.capabilities)
                );
            }
            target
        }
        None => info
            .capabilities
            .adjacent_test_target(before)
            .context("device reports fewer than two DPI values; pass --target to choose one")?,
    };
    if target == before {
        println!(
            "  target {target} equals current — pick a different --target to exercise the write"
        );
        return Ok(());
    }

    check_dpi_round_trip(
        &info,
        target,
        async |dpi| openlogi_hid::set_dpi(&route, dpi).await.context("set DPI"),
        async || openlogi_hid::get_dpi(&route).await.context("get DPI"),
    )
    .await
}

async fn check_dpi_round_trip(
    info: &DpiInfo,
    target: Dpi,
    set: impl AsyncFn(Dpi) -> Result<()>,
    get: impl AsyncFn() -> Result<Dpi>,
) -> Result<()> {
    let before = info.current;
    let tested: Result<()> = async {
        println!("  writing DPI: {target}");
        set(target).await.context("write DPI")?;

        let after = get().await.context("read DPI after write")?;
        println!("  read-back DPI: {after}");

        // `target` is always a device-reported value, so a mismatch means the
        // device adjusted it — fine if it landed on another supported value, but a
        // no-op write (`after == before`) or an off-list read-back is a real fault.
        // (`target != before` is guaranteed by the early return above.)
        if after == before {
            anyhow::bail!("DPI write failed: requested {target}, device still reports {before}");
        }
        if after != target {
            if info.capabilities.contains(after) {
                println!("  note: device snapped {target} → {after}");
            } else {
                anyhow::bail!(
                    "DPI write failed: requested {target}, device reports {after} \
                 which is not in its supported list"
                );
            }
        }

        Ok(())
    }
    .await;

    let restored: Result<()> = async {
        println!("  restoring DPI: {before}");
        set(before).await.context("restore DPI")?;
        let actual = get().await.context("read DPI after restoration")?;
        anyhow::ensure!(
            actual == before,
            "restoration failed: expected {before}, got {actual}"
        );
        println!("  restored DPI: {actual}");
        Ok(())
    }
    .await;
    match (tested, restored) {
        (Ok(()), Ok(())) => {
            println!("✓ DPI round-trip OK");
            Ok(())
        }
        (Err(error), Ok(())) => Err(error),
        (Ok(()), Err(error)) => {
            Err(error.context(format!("could not confirm original DPI {before}")))
        }
        (Err(test), Err(restore)) => anyhow::bail!(
            "DPI test failed: {test:#}; original DPI {before} restoration failed: {restore:#}"
        ),
    }
}

struct DpiSummaryDisplay<'a>(&'a DpiCapabilities);

impl fmt::Display for DpiSummaryDisplay<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let capabilities = self.0;
        let values = capabilities.values();
        if values.len() <= 12 {
            let mut separator = "";
            for value in values {
                write!(f, "{separator}{value}")?;
                separator = ", ";
            }
            return Ok(());
        }
        write!(
            f,
            "{}..{} (step ≈ {}, {} values)",
            capabilities.min(),
            capabilities.max(),
            capabilities.step_hint(),
            values.len()
        )
    }
}

#[cfg(test)]
mod summarize_dpi_tests {
    use openlogi_hid::DpiCapabilities;

    use super::DpiSummaryDisplay;

    #[test]
    fn lists_values_verbatim_at_the_twelve_value_boundary() {
        let values: Vec<u16> = (1..=12).map(|n| n * 100).collect();
        let caps = DpiCapabilities::new(values).expect("non-empty");

        assert_eq!(
            DpiSummaryDisplay(&caps).to_string(),
            "100, 200, 300, 400, 500, 600, 700, 800, 900, 1000, 1100, 1200"
        );
    }

    #[test]
    fn switches_to_a_range_summary_past_twelve_values() {
        let values: Vec<u16> = (1..=13).map(|n| n * 100).collect();
        let caps = DpiCapabilities::new(values).expect("non-empty");

        assert_eq!(
            DpiSummaryDisplay(&caps).to_string(),
            "100..1300 (step ≈ 100, 13 values)"
        );
    }
}

#[cfg(test)]
mod round_trip_tests {
    use super::*;

    #[tokio::test]
    async fn success_requires_exact_restoration_readback() {
        for restored in [800, 1200] {
            let reads = Cell::new(0);
            let info = DpiInfo {
                current: 800.into(),
                capabilities: DpiCapabilities::new(vec![800, 1200]).unwrap(),
            };
            let result = check_dpi_round_trip(
                &info,
                1200.into(),
                async |_| Ok(()),
                async || {
                    reads.set(reads.get() + 1);
                    Ok(if reads.get() == 1 { 1200 } else { restored }.into())
                },
            )
            .await;
            assert_eq!(result.is_ok(), restored == 800);
            assert_eq!(reads.get(), 2);
        }
    }

    #[tokio::test]
    async fn failed_write_still_attempts_restoration_and_reports_both_errors() {
        let writes = Cell::new(0);
        let info = DpiInfo {
            current: 800.into(),
            capabilities: DpiCapabilities::new(vec![800, 1200]).unwrap(),
        };
        let error = check_dpi_round_trip(
            &info,
            1200.into(),
            async |dpi| {
                writes.set(writes.get() + 1);
                anyhow::bail!("write {dpi} failed")
            },
            async || panic!("a failed write must not proceed to readback"),
        )
        .await
        .unwrap_err()
        .to_string();
        assert_eq!(writes.get(), 2);
        assert!(error.contains("write 1200 failed"));
        assert!(error.contains("write 800 failed"));
    }
    use std::cell::{Cell, RefCell};

    #[tokio::test]
    async fn failed_readback_still_restores_and_confirms_original_dpi() {
        let writes = RefCell::new(Vec::new());
        let reads = Cell::new(0);
        let info = DpiInfo {
            current: 800.into(),
            capabilities: DpiCapabilities::new(vec![800, 1200]).unwrap(),
        };
        let result = check_dpi_round_trip(
            &info,
            1200.into(),
            async |dpi| {
                writes.borrow_mut().push(dpi);
                Ok(())
            },
            async || {
                reads.set(reads.get() + 1);
                if reads.get() == 1 {
                    anyhow::bail!("read failed");
                }
                Ok(800.into())
            },
        )
        .await;
        assert!(result.is_err(), "failed readback must fail the diagnostic");
        assert_eq!(*writes.borrow(), vec![Dpi::from(1200), Dpi::from(800)]);
        assert_eq!(reads.get(), 2);
    }
}
