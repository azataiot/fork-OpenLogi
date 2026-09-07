use thiserror::Error;

use super::{ProfileData, ProfileDescription, crc_ccitt};
use crate::feature::report_rate::ReportRateList;

/// An assignment supported by the validated format-1 editor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProfileAssignment {
    /// Primary mouse click.
    MousePrimary,
    /// Secondary mouse click.
    MouseSecondary,
    /// Middle mouse click.
    MouseMiddle,
    /// Back mouse button.
    MouseBack,
    /// Forward mouse button.
    MouseForward,
    /// Select the next DPI stage.
    DpiNext,
    /// Select the previous DPI stage.
    DpiPrevious,
    /// Hold the shifted DPI stage.
    DpiShift,
}

impl ProfileAssignment {
    /// Encode one format-1 assignment.
    #[must_use]
    pub fn encode(self) -> [u8; 4] {
        match self {
            Self::MousePrimary => [0x80, 1, 0, 1],
            Self::MouseSecondary => [0x80, 1, 0, 2],
            Self::MouseMiddle => [0x80, 1, 0, 4],
            Self::MouseBack => [0x80, 1, 0, 8],
            Self::MouseForward => [0x80, 1, 0, 16],
            Self::DpiNext => [0x90, 3, 0xff, 0xff],
            Self::DpiPrevious => [0x90, 4, 0xff, 0xff],
            Self::DpiShift => [0x90, 7, 0xff, 0xff],
        }
    }

    /// Recognize an exact supported assignment without normalizing unknown bytes.
    #[must_use]
    pub fn decode(bytes: [u8; 4]) -> Option<Self> {
        match bytes {
            [0x80, 1, 0, 1] => Some(Self::MousePrimary),
            [0x80, 1, 0, 2] => Some(Self::MouseSecondary),
            [0x80, 1, 0, 4] => Some(Self::MouseMiddle),
            [0x80, 1, 0, 8] => Some(Self::MouseBack),
            [0x80, 1, 0, 16] => Some(Self::MouseForward),
            [0x90, 3, 0xff, 0xff] => Some(Self::DpiNext),
            [0x90, 4, 0xff, 0xff] => Some(Self::DpiPrevious),
            [0x90, 7, 0xff, 0xff] => Some(Self::DpiShift),
            _ => None,
        }
    }
}

/// One primary assignment selected for replacement.
#[derive(Debug, Clone, Copy)]
pub struct AssignmentEdit {
    /// Zero-based physical button index.
    pub index: u8,
    /// Replacement action.
    pub assignment: ProfileAssignment,
}

/// Explicit changes to a stored profile, leaving all other fields untouched.
#[derive(Debug, Clone, Default)]
pub struct ProfileEdit {
    /// A device-advertised report interval, or no change.
    pub report_interval_ms: Option<u8>,
    /// Primary assignments to replace, with no duplicate indices.
    pub assignments: Vec<AssignmentEdit>,
}

/// An edit rejected before any hardware write.
#[derive(Debug, Error)]
pub enum ProfileEditError {
    /// The descriptor or source bytes do not form a supported writable sector.
    #[error("unsupported profile layout or invalid source checksum")]
    InvalidSource,
    /// The requested interval is absent from the advertised capability list.
    #[error("report interval {0} ms is not advertised")]
    InvalidInterval(u8),
    /// The physical index does not exist in the descriptor.
    #[error("button index {0} is outside the profile")]
    InvalidButton(u8),
    /// Multiple edits target the same physical button.
    #[error("button index {0} appears more than once")]
    DuplicateButton(u8),
}

/// A byte-preserving edit and its exact original, suitable for explicit restoration.
#[derive(Debug, Clone)]
pub struct ProfileChange {
    pub(super) description: ProfileDescription,
    before: Vec<u8>,
    after: ProfileData,
}

impl ProfileChange {
    /// Validate the source and encode only the requested fields.
    pub fn new(
        original: &ProfileData,
        description: ProfileDescription,
        edit: &ProfileEdit,
        supported_intervals: ReportRateList,
    ) -> Result<Self, ProfileEditError> {
        if !description.sector_size.is_multiple_of(16) {
            return Err(ProfileEditError::InvalidSource);
        }
        let before = ProfileData::decode(original.raw.clone(), &description)
            .map_err(|_| ProfileEditError::InvalidSource)?;
        let mut raw = before.raw.clone();
        if let Some(interval) = edit.report_interval_ms {
            if !(1..=8).contains(&interval)
                || supported_intervals.bits() & (1 << (interval - 1)) == 0
            {
                return Err(ProfileEditError::InvalidInterval(interval));
            }
            raw[0] = interval;
        }
        let mut seen = 0_u16;
        for assignment in &edit.assignments {
            if assignment.index >= description.button_count {
                return Err(ProfileEditError::InvalidButton(assignment.index));
            }
            let bit = 1 << assignment.index;
            if seen & bit != 0 {
                return Err(ProfileEditError::DuplicateButton(assignment.index));
            }
            seen |= bit;
            let offset =
                super::ASSIGNMENTS_OFFSET + usize::from(assignment.index) * super::ASSIGNMENT_SIZE;
            raw[offset..offset + super::ASSIGNMENT_SIZE]
                .copy_from_slice(&assignment.assignment.encode());
        }
        let checksum_offset = raw.len() - 2;
        let checksum = crc_ccitt(&raw[..checksum_offset]);
        raw[checksum_offset..].copy_from_slice(&checksum.to_be_bytes());
        let after =
            ProfileData::decode(raw, &description).map_err(|_| ProfileEditError::InvalidSource)?;
        Ok(Self {
            description,
            before: before.raw,
            after,
        })
    }

    /// The complete original sector, including unknown fields.
    #[must_use]
    pub fn before(&self) -> &[u8] {
        &self.before
    }

    /// The complete replacement sector with its checksum.
    #[must_use]
    pub fn after(&self) -> &ProfileData {
        &self.after
    }

    /// Reverse this exact change without reconstructing unknown original fields.
    pub fn restoration(&self) -> Result<Self, ProfileEditError> {
        Self::for_restore(&self.after.raw, &self.before, self.description)
    }

    /// Restore validated backup bytes against an exact fresh read, even if that read has a bad checksum.
    pub fn for_restore(
        current: &[u8],
        original: &[u8],
        description: ProfileDescription,
    ) -> Result<Self, ProfileEditError> {
        if !description.sector_size.is_multiple_of(16)
            || current.len() != usize::from(description.sector_size)
        {
            return Err(ProfileEditError::InvalidSource);
        }
        let after = ProfileData::decode(original.to_vec(), &description)
            .map_err(|_| ProfileEditError::InvalidSource)?;
        Ok(Self {
            description,
            before: current.to_vec(),
            after,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::feature::onboard_profiles::{ProfileData, ProfileDescription, crc_ccitt};
    use crate::feature::report_rate::ReportRateList;

    fn original() -> (ProfileDescription, ProfileData) {
        let description =
            ProfileDescription::decode(&[1, 1, 1, 1, 1, 8, 3, 4, 0, 10, 1, 0, 0, 0, 0, 0]).unwrap();
        let mut bytes: Vec<u8> = (0..1024)
            .map(|index| u8::try_from(index % 251).unwrap())
            .collect();
        bytes[0] = 1;
        let crc = crc_ccitt(&bytes[..1022]);
        bytes[1022..].copy_from_slice(&crc.to_be_bytes());
        (
            description,
            ProfileData::decode(bytes, &description).unwrap(),
        )
    }

    #[test]
    fn profile_edit_preserves_every_unrelated_byte_and_restores_the_original() {
        let (description, profile) = original();
        let change = ProfileChange::new(
            &profile,
            description,
            &ProfileEdit {
                report_interval_ms: Some(2),
                assignments: vec![AssignmentEdit {
                    index: 4,
                    assignment: ProfileAssignment::MouseBack,
                }],
            },
            ReportRateList::MS_1 | ReportRateList::MS_2,
        )
        .unwrap();
        let actual = &change.after().raw;
        assert_eq!(actual[0], 2);
        assert_eq!(&actual[48..52], &[0x80, 1, 0, 8]);
        for (index, byte) in actual.iter().enumerate().take(1022) {
            if index != 0 && !(48..52).contains(&index) {
                assert_eq!(*byte, profile.raw[index], "byte {index} changed");
            }
        }
        ProfileData::decode(actual.clone(), &description).unwrap();
        assert_eq!(change.restoration().unwrap().after().raw, profile.raw);
    }

    #[test]
    fn profile_edit_rejects_invalid_intervals_indices_and_duplicate_assignments() {
        let (description, profile) = original();
        for interval in [0, 2, 9, 255] {
            ProfileChange::new(
                &profile,
                description,
                &ProfileEdit {
                    report_interval_ms: Some(interval),
                    assignments: vec![],
                },
                ReportRateList::MS_1 | ReportRateList::MS_8,
            )
            .unwrap_err();
        }
        for indices in [vec![8], vec![255], vec![0, 0]] {
            ProfileChange::new(
                &profile,
                description,
                &ProfileEdit {
                    report_interval_ms: None,
                    assignments: indices
                        .into_iter()
                        .map(|index| AssignmentEdit {
                            index,
                            assignment: ProfileAssignment::DpiShift,
                        })
                        .collect(),
                },
                ReportRateList::MS_1,
            )
            .unwrap_err();
        }
        let mut bad = description;
        bad.profile_format = 2;
        ProfileChange::new(&profile, bad, &ProfileEdit::default(), ReportRateList::MS_1)
            .unwrap_err();
        bad = description;
        bad.sector_size = 1025;
        ProfileChange::new(&profile, bad, &ProfileEdit::default(), ReportRateList::MS_1)
            .unwrap_err();
        let mut corrupt = profile;
        corrupt.raw[400] ^= 1;
        ProfileChange::new(
            &corrupt,
            description,
            &ProfileEdit::default(),
            ReportRateList::MS_1,
        )
        .unwrap_err();
    }

    #[test]
    fn observed_assignment_encodings_round_trip_and_unknown_values_stay_unknown() {
        for (assignment, bytes) in [
            (ProfileAssignment::MousePrimary, [0x80, 1, 0, 1]),
            (ProfileAssignment::MouseSecondary, [0x80, 1, 0, 2]),
            (ProfileAssignment::MouseMiddle, [0x80, 1, 0, 4]),
            (ProfileAssignment::MouseBack, [0x80, 1, 0, 8]),
            (ProfileAssignment::MouseForward, [0x80, 1, 0, 16]),
            (ProfileAssignment::DpiNext, [0x90, 3, 0xff, 0xff]),
            (ProfileAssignment::DpiPrevious, [0x90, 4, 0xff, 0xff]),
            (ProfileAssignment::DpiShift, [0x90, 7, 0xff, 0xff]),
        ] {
            assert_eq!(assignment.encode(), bytes);
            assert_eq!(ProfileAssignment::decode(bytes), Some(assignment));
        }
        assert_eq!(ProfileAssignment::decode([0x80, 2, 0, 4]), None);
        assert_eq!(ProfileAssignment::decode([0x90, 3, 0, 0]), None);
    }
}
