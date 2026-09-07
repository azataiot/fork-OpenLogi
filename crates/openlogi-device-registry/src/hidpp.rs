//! Static identities for directly attached HID++ products.

use crate::LOGITECH_VENDOR_ID;

/// Model metadata independent of the firmware's HID++ model identifiers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DirectHidppDescriptor {
    /// Exact model identifier in the OpenLogi asset registry.
    pub registry_model_id: &'static str,
    /// Whether this product has no wireless transport.
    pub wired_only: bool,
}

/// Find metadata by the directly attached device's complete vendor/product identity.
#[must_use]
pub fn find_direct_hidpp(vendor_id: u16, product_id: u16) -> Option<DirectHidppDescriptor> {
    match (vendor_id, product_id) {
        (LOGITECH_VENDOR_ID, 0xc07e) => Some(DirectHidppDescriptor {
            registry_model_id: "aab4",
            wired_only: true,
        }),
        _ => None,
    }
}

/// One model-specific physical button in the asset catalog.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OnboardButton {
    /// Exact assignment identifier in the model asset metadata.
    pub slot_id: &'static str,
    /// Zero-based position in the stored assignment table.
    pub index: u8,
    /// Manifest image resource carrying the button marker.
    pub image_key: &'static str,
}

/// Physical button mappings for a supported registry model.
#[must_use]
pub fn onboard_buttons(registry_model_id: &str) -> &'static [OnboardButton] {
    const G402: [OnboardButton; 8] = [
        OnboardButton {
            slot_id: "g402_g1_m1",
            index: 0,
            image_key: "device_image",
        },
        OnboardButton {
            slot_id: "g402_g2_m1",
            index: 1,
            image_key: "device_image",
        },
        OnboardButton {
            slot_id: "g402_g3_m1",
            index: 2,
            image_key: "device_image",
        },
        OnboardButton {
            slot_id: "g402_g4_m1",
            index: 3,
            image_key: "device_side",
        },
        OnboardButton {
            slot_id: "g402_g5_m1",
            index: 4,
            image_key: "device_side",
        },
        OnboardButton {
            slot_id: "g402_g6_m1",
            index: 5,
            image_key: "device_side",
        },
        OnboardButton {
            slot_id: "g402_g7_m1",
            index: 6,
            image_key: "device_image",
        },
        OnboardButton {
            slot_id: "g402_g8_m1",
            index: 7,
            image_key: "device_image",
        },
    ];
    match registry_model_id {
        "aab4" => &G402,
        _ => &[],
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn onboard_slots_have_stable_indices_and_image_views() {
        let buttons = super::onboard_buttons("aab4");
        assert_eq!(buttons.len(), 8);
        for (index, button) in buttons.iter().enumerate() {
            assert_eq!(usize::from(button.index), index);
            assert_eq!(button.slot_id.as_bytes()[6], b'1' + button.index);
            assert_eq!(
                button.image_key,
                if (3..=5).contains(&index) {
                    "device_side"
                } else {
                    "device_image"
                }
            );
        }
        assert!(super::onboard_buttons("unknown").is_empty());
    }

    #[test]
    fn g402_requires_an_exact_vendor_and_product_match() {
        let device = super::find_direct_hidpp(0x046d, 0xc07e).expect("G402");
        assert_eq!(device.registry_model_id, "aab4");
        assert!(device.wired_only);
        assert!(super::find_direct_hidpp(0x1234, 0xc07e).is_none());
        assert!(super::find_direct_hidpp(0x046d, 0xc07f).is_none());
    }
}
