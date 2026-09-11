//! The DIMSE command set a C-STORE travels as (PS3.7 section 9.1.1): group
//! 0000 elements in implicit VR little endian — the SOP class and instance
//! affected, the command field, the message id, the priority, whether a
//! data set follows, and in the response the status.
//!
//! The data set that follows is the Stream, as the sender encoded it; this
//! layer neither reads nor rewrites it. What it does is cut both into
//! presentation data values no longer than the PDU the far end reads.

use transport::error::{Result, protocol_error};

use crate::pdu::Pdv;

/// C-STORE-RQ.
pub const C_STORE_RQ: u16 = 0x0001;
/// C-STORE-RSP.
pub const C_STORE_RSP: u16 = 0x8001;
/// The status of a store that succeeded.
pub const SUCCESS: u16 = 0x0000;
/// The status for a command this side does not perform.
pub const UNRECOGNIZED_OPERATION: u16 = 0x0211;
/// The data set type that says no data set follows.
const NO_DATA_SET: u16 = 0x0101;
const HAS_DATA_SET: u16 = 0x0102;
const MEDIUM_PRIORITY: u16 = 0x0000;

/// One command as its elements describe it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Command {
    pub field: u16,
    pub sop_class: String,
    pub sop_instance: String,
    /// The message id, or in a response the id responded to.
    pub message_id: u16,
    pub status: u16,
    pub has_data_set: bool,
}

impl Command {
    /// A C-STORE-RQ for `sop_instance` of `sop_class`, a data set to
    /// follow.
    #[must_use]
    pub fn store(sop_class: &str, sop_instance: &str, message_id: u16) -> Self {
        Self {
            field: C_STORE_RQ,
            sop_class: sop_class.to_string(),
            sop_instance: sop_instance.to_string(),
            message_id,
            status: SUCCESS,
            has_data_set: true,
        }
    }

    /// The response to this request with `status`, nothing following.
    #[must_use]
    pub fn response(&self, status: u16) -> Self {
        Self {
            field: self.field | 0x8000,
            status,
            has_data_set: false,
            ..self.clone()
        }
    }

    /// The command set, its group length first.
    #[must_use]
    pub fn to_bytes(&self) -> Vec<u8> {
        let response = self.field & 0x8000 != 0;
        let mut body = Vec::new();
        element(&mut body, 0x0002, &ui(&self.sop_class));
        element(&mut body, 0x0100, &self.field.to_le_bytes());
        let id_tag = if response { 0x0120 } else { 0x0110 };
        element(&mut body, id_tag, &self.message_id.to_le_bytes());
        if !response {
            element(&mut body, 0x0700, &MEDIUM_PRIORITY.to_le_bytes());
        }
        let data_set = if self.has_data_set {
            HAS_DATA_SET
        } else {
            NO_DATA_SET
        };
        element(&mut body, 0x0800, &data_set.to_le_bytes());
        if response {
            element(&mut body, 0x0900, &self.status.to_le_bytes());
        }
        element(&mut body, 0x1000, &ui(&self.sop_instance));
        let length = u32::try_from(body.len()).unwrap_or(u32::MAX);
        let mut command = Vec::with_capacity(body.len() + 12);
        element(&mut command, 0x0000, &length.to_le_bytes());
        command.extend(body);
        command
    }

    /// The command a command set carries.
    ///
    /// # Errors
    /// Where an element runs past the end, or there is no command field.
    pub fn from_bytes(bytes: &[u8]) -> Result<Self> {
        let mut command = Self {
            field: 0,
            sop_class: String::new(),
            sop_instance: String::new(),
            message_id: 0,
            status: SUCCESS,
            has_data_set: false,
        };
        let mut field = None;
        let mut rest = bytes;
        while rest.len() >= 8 {
            let group = u16::from_le_bytes([rest[0], rest[1]]);
            let tag = u16::from_le_bytes([rest[2], rest[3]]);
            let length = u32::from_le_bytes([rest[4], rest[5], rest[6], rest[7]]) as usize;
            let after = &rest[8..];
            if after.len() < length {
                return Err(protocol_error("a command element that runs past its set"));
            }
            let (value, next) = after.split_at(length);
            let short = || {
                u16::from_le_bytes([
                    value.first().copied().unwrap_or(0),
                    value.get(1).copied().unwrap_or(0),
                ])
            };
            if group == 0 {
                match tag {
                    0x0002 => command.sop_class = uid(value),
                    0x0100 => field = Some(short()),
                    0x0110 | 0x0120 => command.message_id = short(),
                    0x0800 => command.has_data_set = short() != NO_DATA_SET,
                    0x0900 => command.status = short(),
                    0x1000 => command.sop_instance = uid(value),
                    _ => {}
                }
            }
            rest = next;
        }
        command.field =
            field.ok_or_else(|| protocol_error("a command set with no command field"))?;
        Ok(command)
    }
}

/// `bytes` — a command set or a data set — as the P-DATA-TF bodies that
/// carry it on `context_id` in PDUs no longer than `max_pdu`; an empty
/// data set is one empty value marked last.
#[must_use]
pub fn fragments(bytes: &[u8], context_id: u8, command: bool, max_pdu: usize) -> Vec<Vec<u8>> {
    let room = max_pdu.saturating_sub(6).max(1);
    let count = bytes.len().div_ceil(room).max(1);
    (0..count)
        .map(|n| {
            let fragment = bytes.get(n * room..((n + 1) * room).min(bytes.len()));
            Pdv {
                context_id,
                command,
                last: n + 1 == count,
                bytes: fragment.unwrap_or_default().to_vec(),
            }
            .data_transfer()
        })
        .collect()
}

/// One element of group 0000, implicit VR, little endian.
fn element(out: &mut Vec<u8>, tag: u16, value: &[u8]) {
    out.extend_from_slice(&0u16.to_le_bytes());
    out.extend_from_slice(&tag.to_le_bytes());
    out.extend_from_slice(&u32::try_from(value.len()).unwrap_or(u32::MAX).to_le_bytes());
    out.extend_from_slice(value);
}

/// A UID as a value: padded with one NUL to an even length.
fn ui(uid: &str) -> Vec<u8> {
    let mut value = uid.as_bytes().to_vec();
    if value.len() % 2 == 1 {
        value.push(0);
    }
    value
}

/// A UID value as text, its padding off.
fn uid(value: &[u8]) -> String {
    String::from_utf8_lossy(value)
        .trim_end_matches(['\0', ' '])
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pdu::pdvs;

    #[test]
    fn a_store_and_its_response_read_back_as_they_were_written() {
        let request = Command::store("1.2.840.10008.5.1.4.1.1.7", "2.25.1", 7);
        let bytes = request.to_bytes();
        assert_eq!(&bytes[..4], &[0, 0, 0, 0]);
        assert_eq!(bytes.len() % 2, 0);
        assert_eq!(Command::from_bytes(&bytes).expect("read"), request);
        let response = request.response(SUCCESS);
        assert_eq!(response.field, C_STORE_RSP);
        assert!(!response.has_data_set);
        assert_eq!(
            Command::from_bytes(&response.to_bytes()).expect("read"),
            response
        );
        assert!(Command::from_bytes(&[0, 0, 2, 0, 9, 0, 0, 0, b'1']).is_err());
        assert!(Command::from_bytes(&[]).is_err());
    }

    #[test]
    fn a_data_set_is_cut_to_the_pdu_and_the_last_fragment_is_marked() {
        let data: Vec<u8> = (0..=255).collect();
        let bodies = fragments(&data, 1, false, 106);
        assert_eq!(bodies.len(), 3);
        let mut back = Vec::new();
        for (n, body) in bodies.iter().enumerate() {
            assert!(body.len() <= 106);
            let value = &pdvs(body).expect("values")[0];
            assert_eq!(value.last, n == 2);
            assert!(!value.command);
            back.extend_from_slice(&value.bytes);
        }
        assert_eq!(back, data);
        let empty = fragments(&[], 1, false, 106);
        assert_eq!(empty.len(), 1);
        let value = &pdvs(&empty[0]).expect("values")[0];
        assert!(value.last && value.bytes.is_empty());
    }
}
