//! What a Location of this technology is configured with: its declaration,
//! which `Configured` reads, built with the crate's `declaration` feature
//! alone and the code behind the default (ADR-0064, amendments 2026-10-22
//! and 2026-11-26), for the library the surfaces load to carry (open
//! problem 48).

use xcore::settings::{Applies, Fixed, Kind, Presence, Setting, Settings};

/// Secondary Capture Image Storage, the SOP class a store of opaque bytes
/// goes under until a Location's `sop_class` names another.
pub const SECONDARY_CAPTURE: &str = "1.2.840.10008.5.1.4.1.1.7";

/// The largest PDU this side reads unless told otherwise.
pub const DEFAULT_MAX_PDU: u32 = 16_384;

/// Every setting a Location takes beyond its address, which is where a
/// Receive Location listens as the SCP — `0.0.0.0:104` the standard port;
/// a Send Location calls the SCP its target gives.
pub const SETTINGS: &Settings = &Settings {
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
            presence: Presence::Default(Fixed::Integer(DEFAULT_MAX_PDU as i64)),
            meaning: "The longest PDU read, in bytes, and said so when associating.",
            applies: Applies::Both,
        },
        transport::configured::timeout(
            Applies::Both,
            None,
            "How long a peer that stops mid-association is waited on; unbounded \
             when left out.",
        ),
    ],
};

/// The schemes a Location's address is entered with to choose this technology.
pub const SCHEMES: &[&str] = &["dicom"];
