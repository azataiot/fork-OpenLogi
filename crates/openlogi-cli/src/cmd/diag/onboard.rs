use std::time::{Duration, Instant};

use anyhow::{Context as _, Result, bail};
use clap::{Args, Subcommand};
use openlogi_core::hid::onboard_profile::{
    OnboardApplyResult, OnboardAssignment, OnboardAssignmentEdit, OnboardProfileEdit, ProfileEditId,
};
use openlogi_hid::{DeviceRoute, WriteError};
use openlogi_ipc::{ClientKind, PROTOCOL_VERSION, client};
use tarpc::context;

#[derive(Debug, Args)]
pub struct OnboardArgs {
    #[arg(long)]
    pub device: String,
    #[command(subcommand)]
    pub command: Option<OnboardCommand>,
}

#[derive(Debug, Subcommand)]
pub enum OnboardCommand {
    /// Read the active profile and issue a single-use preparation identifier.
    Read,
    /// Back up and write explicitly selected fields from a prior read.
    Apply {
        #[arg(long, value_parser = parse_id)]
        id: ProfileEditId,
        #[arg(long, value_parser = clap::value_parser!(u8).range(1..=8))]
        interval: Option<u8>,
        #[arg(long = "button", value_name = "NUMBER=ACTION", value_parser = parse_assignment)]
        assignments: Vec<OnboardAssignmentEdit>,
    },
    /// Restore exact bytes from an available backup through a fresh read.
    Restore {
        #[arg(long, value_parser = parse_id)]
        id: ProfileEditId,
        #[arg(long, value_parser = parse_id)]
        backup: ProfileEditId,
    },
}

pub async fn run(args: OnboardArgs) -> Result<()> {
    let connection = tokio::time::timeout(Duration::from_secs(5), client::connect())
        .await
        .context("agent connection timed out")??;
    if connection.version != PROTOCOL_VERSION {
        bail!(
            "agent protocol is {}, expected {PROTOCOL_VERSION}; start the matching agent",
            connection.version
        );
    }
    let client = connection.client;
    client
        .declare_client(context::current(), ClientKind::Cli)
        .await?;
    let snapshot = client.snapshot(context::current()).await?;
    let query = args.device.to_lowercase();
    let candidates: Vec<_> = snapshot
        .inventory
        .iter()
        .flat_map(|inventory| {
            inventory
                .paired
                .iter()
                .filter(|paired| paired.online)
                .filter_map(|paired| {
                    let name = paired.codename.as_deref()?;
                    if !name.to_lowercase().contains(&query) {
                        return None;
                    }
                    Some((
                        DeviceRoute::device_route_for(inventory, paired.slot).unwrap_or(
                            DeviceRoute::Direct {
                                vendor_id: inventory.receiver.vendor_id,
                                product_id: inventory.receiver.product_id,
                            },
                        ),
                        name,
                    ))
                })
        })
        .collect();
    let [(route, name)] = candidates.as_slice() else {
        bail!(
            "expected one online device matching {:?}, found {}",
            args.device,
            candidates.len()
        );
    };
    println!("device: {name} ({route})");
    let mut ctx = context::current();
    ctx.deadline = Instant::now() + Duration::from_secs(120);
    match args.command.unwrap_or(OnboardCommand::Read) {
        OnboardCommand::Read => {
            let view = client.read_onboard_profile(ctx, route.clone()).await??;
            println!("preparation: {}", view.edit_id);
            println!(
                "sector: {}, descriptor: {:?}",
                view.sector, view.description
            );
            println!("supported intervals: {:?} ms", view.supported_intervals_ms);
            println!("active interval: {:?} ms", view.active_report_interval_ms);
            println!("contents: {:?}", view.contents);
            for backup in view.backups {
                println!(
                    "backup: {} (UTC seconds {})",
                    backup.id, backup.created_unix_seconds
                );
            }
        }
        OnboardCommand::Apply {
            id,
            interval,
            assignments,
        } => {
            let result = client
                .apply_onboard_profile(
                    ctx,
                    route.clone(),
                    id,
                    OnboardProfileEdit {
                        report_interval_ms: interval,
                        assignments,
                    },
                )
                .await
                .context("profile request interrupted; read again before any further write")?
                .map_err(profile_error)?;
            show_apply_result(&result);
        }
        OnboardCommand::Restore { id, backup } => {
            let result = client
                .restore_onboard_profile(ctx, route.clone(), id, backup)
                .await
                .context("restore request interrupted; read again before any further write")?
                .map_err(profile_error)?;
            show_apply_result(&result);
        }
    }
    Ok(())
}

fn show_apply_result(result: &OnboardApplyResult) {
    match result {
        OnboardApplyResult::Unchanged => println!("unchanged; no flash write sent"),
        OnboardApplyResult::Written { backup_id } => {
            println!("full sector readback matched; backup: {backup_id}");
        }
    }
}

fn profile_error(error: WriteError) -> anyhow::Error {
    let backup = match &error {
        WriteError::OnboardProfile { backup_id, .. } => *backup_id,
        _ => None,
    };
    let error = anyhow::Error::new(error);
    match backup {
        Some(id) => error.context(format!("backup: {id}")),
        None => error,
    }
}

fn parse_id(text: &str) -> Result<ProfileEditId, String> {
    let (run, sequence) = text
        .split_once('-')
        .ok_or("expected the preparation identifier from a read")?;
    if run.len() != 16 || sequence.len() != 16 {
        return Err("expected two 16-digit hexadecimal identifiers".into());
    }
    Ok(ProfileEditId {
        run: u64::from_str_radix(run, 16).map_err(|error| error.to_string())?,
        sequence: u64::from_str_radix(sequence, 16).map_err(|error| error.to_string())?,
    })
}

fn parse_assignment(text: &str) -> Result<OnboardAssignmentEdit, String> {
    let (index, action) = text.split_once('=').ok_or("expected NUMBER=ACTION")?;
    let index = index
        .parse::<u8>()
        .ok()
        .filter(|index| (1..=16).contains(index))
        .ok_or("button number must be between 1 and 16")?
        - 1;
    let assignment = match action {
        "primary" => OnboardAssignment::MousePrimary,
        "secondary" => OnboardAssignment::MouseSecondary,
        "middle" => OnboardAssignment::MouseMiddle,
        "back" => OnboardAssignment::MouseBack,
        "forward" => OnboardAssignment::MouseForward,
        "dpi-next" => OnboardAssignment::DpiNext,
        "dpi-previous" => OnboardAssignment::DpiPrevious,
        "dpi-shift" => OnboardAssignment::DpiShift,
        _ => return Err("action must be primary, secondary, middle, back, forward, dpi-next, dpi-previous, or dpi-shift".into()),
    };
    Ok(OnboardAssignmentEdit { index, assignment })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uncertain_write_reports_both_failure_and_backup() {
        let error = profile_error(WriteError::OnboardProfile {
            kind: openlogi_core::hid::onboard_profile::OnboardProfileFailure::Uncertain,
            message: "readback failed".into(),
            backup_id: Some(ProfileEditId {
                run: 1,
                sequence: 2,
            }),
        });
        let message = format!("{error:#}");
        assert!(message.contains("0000000000000001-0000000000000002"));
        assert!(message.contains("readback failed"));
    }

    #[test]
    fn onboard_arguments_preserve_identity_and_validate_physical_buttons() {
        let id = ProfileEditId {
            run: 42,
            sequence: 7,
        };
        assert_eq!(parse_id(&id.to_string()).unwrap(), id);
        for invalid in ["../file", "1-2", "000000000000000g-0000000000000001"] {
            assert!(parse_id(invalid).is_err(), "{invalid}");
        }
        assert_eq!(
            parse_assignment("8=dpi-shift").unwrap(),
            OnboardAssignmentEdit {
                index: 7,
                assignment: OnboardAssignment::DpiShift
            }
        );
        for invalid in ["0=primary", "17=back", "1=unknown", "1"] {
            assert!(parse_assignment(invalid).is_err(), "{invalid}");
        }
    }
}
