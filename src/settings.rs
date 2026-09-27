//! What a DICOM Location takes beyond its address, declared once and read
//! through (ADR-0064, amendment 2026-09-26).

use transport::Configured;
use transport::error::{Result, protocol_error};
use xcore::settings::{Applies, Fixed, Kind, Presence, Read, Setting, Settings};

use crate::{DicomTransport, SECONDARY_CAPTURE, pdu};

impl Configured for DicomTransport {
    /// The address is where a Receive Location listens as the SCP —
    /// `0.0.0.0:104` the standard port; a Send Location calls the SCP its
    /// target gives.
    const SETTINGS: &'static Settings = &Settings {
        technology: env!("CARGO_PKG_NAME"),
        settings: &[
            Setting {
                name: "title",
                kind: Kind::Text,
                presence: Presence::Optional,
                meaning: "This side's application entity title: the one a Receive Location \
                          answers to and a Send Location calls from; XMIP when left out.",
                applies: Applies::Both,
            },
            Setting {
                name: "called",
                kind: Kind::Text,
                presence: Presence::Optional,
                meaning: "The title a store calls where the target names none; ANY-SCP when \
                          left out.",
                applies: Applies::Send,
            },
            Setting {
                name: "sop_class",
                kind: Kind::Text,
                presence: Presence::Default(Fixed::Text(SECONDARY_CAPTURE)),
                meaning: "The SOP class UID a Stream is stored under.",
                applies: Applies::Send,
            },
            Setting {
                name: "max_pdu",
                kind: Kind::Integer {
                    minimum: 0,
                    maximum: u32::MAX as i64,
                },
                presence: Presence::Default(Fixed::Integer(pdu::DEFAULT_MAX_PDU as i64)),
                meaning: "The longest PDU read, in bytes, and said so when associating.",
                applies: Applies::Both,
            },
            Setting {
                name: "timeout",
                kind: Kind::Duration,
                presence: Presence::Optional,
                meaning: "How long a peer that stops mid-association is waited on; unbounded \
                          when left out.",
                applies: Applies::Both,
            },
        ],
    };

    fn configured(address: &str, settings: &Read) -> Result<Self> {
        let max_pdu = u32::try_from(settings.integer("max_pdu"))
            .map_err(|_| protocol_error("a maximum PDU length out of range"))?;
        let mut transport = Self::new(address).reading_pdus_of(max_pdu);
        let title = settings
            .optional_text("title")
            .unwrap_or(&transport.title)
            .to_string();
        let called = settings
            .optional_text("called")
            .unwrap_or(&transport.called)
            .to_string();
        transport = transport.titled(&title, &called);
        if let Some(sop_class) = settings.optional_text("sop_class") {
            transport = transport.storing(sop_class);
        }
        if let Some(timeout) = settings.optional_duration("timeout") {
            transport = transport.timing_out_after(timeout);
        }
        Ok(transport)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;
    use xcore::settings::Given;

    #[test]
    fn dicom_declares_its_settings_and_reads_through_them() {
        assert_eq!(DicomTransport::SETTINGS.problems(), Vec::<String>::new());
        let given = [
            ("title".to_string(), Given::Text("MODALITY".to_string())),
            ("called".to_string(), Given::Text("ARCHIVE".to_string())),
            ("timeout".to_string(), Given::Text("5s".to_string())),
        ];
        let built = DicomTransport::open("0.0.0.0:104", Applies::Send, &given).expect("built");
        assert_eq!(
            (built.title.as_str(), built.called.as_str()),
            ("MODALITY", "ARCHIVE")
        );
        assert_eq!(built.sop_class, SECONDARY_CAPTURE);
        assert_eq!(built.max_pdu, pdu::DEFAULT_MAX_PDU);
        assert_eq!(built.timeout, Some(Duration::from_secs(5)));
        let given = [("called".to_string(), Given::Text("ARCHIVE".to_string()))];
        let Err(refused) = DicomTransport::open("0.0.0.0:104", Applies::Receive, &given) else {
            panic!("called is a send setting");
        };
        assert!(
            refused.message.contains("\"called\""),
            "{}",
            refused.message
        );
    }
}
