//! The DICOM upper layer (PS3.8): seven PDUs over one TCP connection — the
//! association request, its acceptance or rejection, data transfer, the
//! release request and its reply, and abort — each a type byte, a reserved
//! byte, a big-endian length and a body.
//!
//! An association names both application entities and offers one
//! presentation context: an abstract syntax, the SOP class the SCU will
//! store, and the transfer syntaxes it can write it in. The acceptor
//! answers with the one it chose and the largest PDU it will read. Data
//! then travels as presentation data values, each marked command or data
//! set and last or not, so a message of any size crosses in PDUs of the
//! negotiated length.

use std::io::{Read, Write};

use transport::error::{Result, classify, protocol_error};

/// A-ASSOCIATE-RQ.
pub const ASSOCIATE_RQ: u8 = 0x01;
/// A-ASSOCIATE-AC.
pub const ASSOCIATE_AC: u8 = 0x02;
/// A-ASSOCIATE-RJ.
pub const ASSOCIATE_RJ: u8 = 0x03;
/// P-DATA-TF.
pub const P_DATA: u8 = 0x04;
/// A-RELEASE-RQ.
pub const RELEASE_RQ: u8 = 0x05;
/// A-RELEASE-RP.
pub const RELEASE_RP: u8 = 0x06;
/// A-ABORT.
pub const ABORT: u8 = 0x07;

/// The one application context there is.
pub const APPLICATION_CONTEXT: &str = "1.2.840.10008.3.1.1.1";
/// Implicit VR little endian, the transfer syntax every entity reads.
pub const IMPLICIT_VR_LE: &str = "1.2.840.10008.1.2";
/// What this implementation calls itself in the user information.
pub const IMPLEMENTATION_CLASS: &str = "2.25.314159265358979.1";
/// The largest PDU this side reads unless told otherwise.
pub const DEFAULT_MAX_PDU: u32 = 16_384;

/// The rejection reason for a called title this side does not answer to.
pub const CALLED_AE_NOT_RECOGNIZED: u8 = 7;

const APPLICATION_CONTEXT_ITEM: u8 = 0x10;
const PRESENTATION_CONTEXT_RQ: u8 = 0x20;
const PRESENTATION_CONTEXT_AC: u8 = 0x21;
const ABSTRACT_SYNTAX: u8 = 0x30;
const TRANSFER_SYNTAX: u8 = 0x40;
const USER_INFORMATION: u8 = 0x50;
const MAXIMUM_LENGTH: u8 = 0x51;
const IMPLEMENTATION_CLASS_UID: u8 = 0x52;

/// One PDU off the wire: its type and its body.
///
/// # Errors
/// Where the connection closed or broke, or the PDU is longer than `max`.
pub fn read(reader: &mut impl Read, max: usize) -> Result<(u8, Vec<u8>)> {
    let mut head = [0u8; 6];
    reader
        .read_exact(&mut head)
        .map_err(|e| classify("reading a PDU header", &e))?;
    let length = u32::from_be_bytes([head[2], head[3], head[4], head[5]]) as usize;
    if length > max {
        return Err(protocol_error(format!(
            "a PDU of {length} bytes, over the {max} this side reads"
        )));
    }
    let mut body = vec![0u8; length];
    reader
        .read_exact(&mut body)
        .map_err(|e| classify("reading a PDU body", &e))?;
    Ok((head[0], body))
}

/// `body` as a PDU of `kind`, written and flushed.
///
/// # Errors
/// Where the connection broke.
pub fn write(writer: &mut impl Write, kind: u8, body: &[u8]) -> Result<()> {
    let length = u32::try_from(body.len())
        .map_err(|_| protocol_error("a PDU longer than its length field can say"))?;
    let mut pdu = Vec::with_capacity(body.len() + 6);
    pdu.push(kind);
    pdu.push(0);
    pdu.extend_from_slice(&length.to_be_bytes());
    pdu.extend_from_slice(body);
    writer
        .write_all(&pdu)
        .and_then(|()| writer.flush())
        .map_err(|e| classify("writing a PDU", &e))
}

/// The name of a PDU type, for a message.
#[must_use]
pub fn name(kind: u8) -> &'static str {
    match kind {
        ASSOCIATE_RQ => "A-ASSOCIATE-RQ",
        ASSOCIATE_AC => "A-ASSOCIATE-AC",
        ASSOCIATE_RJ => "A-ASSOCIATE-RJ",
        P_DATA => "P-DATA-TF",
        RELEASE_RQ => "A-RELEASE-RQ",
        RELEASE_RP => "A-RELEASE-RP",
        ABORT => "A-ABORT",
        _ => "an unknown PDU",
    }
}

/// An association as requested or accepted: the two titles, the one
/// presentation context, and the largest PDU the sender of it reads.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Associate {
    pub called: String,
    pub calling: String,
    pub context_id: u8,
    /// The SOP class offered; empty in an acceptance, which does not
    /// repeat it.
    pub abstract_syntax: String,
    /// The first transfer syntax offered, or the one accepted.
    pub transfer_syntax: String,
    /// The acceptance's result for the context: 0 is accepted.
    pub result: u8,
    /// The largest PDU the sender reads; 0 where it did not say.
    pub max_pdu: u32,
}

impl Associate {
    /// The A-ASSOCIATE-RQ body.
    #[must_use]
    pub fn request(&self) -> Vec<u8> {
        let mut context = vec![self.context_id, 0, 0, 0];
        context.extend(item(ABSTRACT_SYNTAX, self.abstract_syntax.as_bytes()));
        context.extend(item(TRANSFER_SYNTAX, self.transfer_syntax.as_bytes()));
        self.body(item(PRESENTATION_CONTEXT_RQ, &context))
    }

    /// The A-ASSOCIATE-AC body.
    #[must_use]
    pub fn accept(&self) -> Vec<u8> {
        let mut context = vec![self.context_id, 0, self.result, 0];
        context.extend(item(TRANSFER_SYNTAX, self.transfer_syntax.as_bytes()));
        self.body(item(PRESENTATION_CONTEXT_AC, &context))
    }

    /// The association a request or acceptance body describes.
    ///
    /// # Errors
    /// Where the body is too short to be one, or an item runs past its end.
    pub fn from_bytes(body: &[u8]) -> Result<Self> {
        if body.len() < 68 {
            return Err(protocol_error(
                "an association PDU shorter than its fixed part",
            ));
        }
        let mut associate = Self {
            called: title(&body[4..20]),
            calling: title(&body[20..36]),
            context_id: 1,
            abstract_syntax: String::new(),
            transfer_syntax: String::new(),
            result: 0,
            max_pdu: 0,
        };
        for (kind, content) in items(&body[68..])? {
            match kind {
                PRESENTATION_CONTEXT_RQ | PRESENTATION_CONTEXT_AC if content.len() >= 4 => {
                    associate.context_id = content[0];
                    if kind == PRESENTATION_CONTEXT_AC {
                        associate.result = content[2];
                    }
                    for (sub, uid) in items(&content[4..])? {
                        if sub == ABSTRACT_SYNTAX {
                            associate.abstract_syntax = title(uid);
                        } else if sub == TRANSFER_SYNTAX && associate.transfer_syntax.is_empty() {
                            associate.transfer_syntax = title(uid);
                        }
                    }
                }
                USER_INFORMATION => {
                    for (sub, value) in items(content)? {
                        if sub == MAXIMUM_LENGTH && value.len() == 4 {
                            associate.max_pdu =
                                u32::from_be_bytes([value[0], value[1], value[2], value[3]]);
                        }
                    }
                }
                _ => {}
            }
        }
        Ok(associate)
    }

    /// The fixed part, the application context, `context` and the user
    /// information.
    fn body(&self, context: Vec<u8>) -> Vec<u8> {
        let mut body = vec![0, 1, 0, 0];
        body.extend_from_slice(&padded(&self.called));
        body.extend_from_slice(&padded(&self.calling));
        body.extend_from_slice(&[0u8; 32]);
        body.extend(item(
            APPLICATION_CONTEXT_ITEM,
            APPLICATION_CONTEXT.as_bytes(),
        ));
        body.extend(context);
        let mut user = item(MAXIMUM_LENGTH, &self.max_pdu.to_be_bytes());
        user.extend(item(
            IMPLEMENTATION_CLASS_UID,
            IMPLEMENTATION_CLASS.as_bytes(),
        ));
        body.extend(item(USER_INFORMATION, &user));
        body
    }
}

/// The A-ASSOCIATE-RJ body: permanent, from the service user, `reason`.
#[must_use]
pub fn reject(reason: u8) -> Vec<u8> {
    vec![0, 1, 1, reason]
}

/// Why an A-ASSOCIATE-RJ body said no, for a message.
#[must_use]
pub fn rejection(body: &[u8]) -> String {
    match body.get(3) {
        Some(1) => "no reason given".to_string(),
        Some(2) => "application context not supported".to_string(),
        Some(3) => "calling AE title not recognized".to_string(),
        Some(&CALLED_AE_NOT_RECOGNIZED) => "called AE title not recognized".to_string(),
        Some(0) | None => "a rejection with no reason in it".to_string(),
        Some(reason) => format!("reason {reason}"),
    }
}

/// The A-ABORT body: from the service user, no reason.
#[must_use]
pub fn abort() -> Vec<u8> {
    vec![0, 0, 0, 0]
}

/// One presentation data value: a fragment of a command set or a data
/// set on one presentation context, the last of its message or not.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Pdv {
    pub context_id: u8,
    pub command: bool,
    pub last: bool,
    pub bytes: Vec<u8>,
}

impl Pdv {
    /// This value alone as a P-DATA-TF body.
    #[must_use]
    pub fn data_transfer(&self) -> Vec<u8> {
        let length = u32::try_from(self.bytes.len() + 2).unwrap_or(u32::MAX);
        let mut body = Vec::with_capacity(self.bytes.len() + 6);
        body.extend_from_slice(&length.to_be_bytes());
        body.push(self.context_id);
        body.push(u8::from(self.command) | (u8::from(self.last) << 1));
        body.extend_from_slice(&self.bytes);
        body
    }
}

/// The values a P-DATA-TF body carries.
///
/// # Errors
/// Where a value runs past the end of the body.
pub fn pdvs(body: &[u8]) -> Result<Vec<Pdv>> {
    let mut values = Vec::new();
    let mut rest = body;
    while !rest.is_empty() {
        let (length, after) = length_of(rest, 4)?;
        let (value, next) = after.split_at(length.min(after.len()));
        if length < 2 || value.len() < length {
            return Err(protocol_error("a data value that runs past its PDU"));
        }
        values.push(Pdv {
            context_id: value[0],
            command: value[1] & 1 != 0,
            last: value[1] & 2 != 0,
            bytes: value[2..].to_vec(),
        });
        rest = next;
    }
    Ok(values)
}

/// `content` as an item of `kind`: the kind, a reserved byte, a
/// big-endian length.
fn item(kind: u8, content: &[u8]) -> Vec<u8> {
    let length = u16::try_from(content.len()).unwrap_or(u16::MAX);
    let mut item = vec![kind, 0];
    item.extend_from_slice(&length.to_be_bytes());
    item.extend_from_slice(content);
    item
}

/// The items in `bytes`, each its kind and its content.
fn items(bytes: &[u8]) -> Result<Vec<(u8, &[u8])>> {
    let mut found = Vec::new();
    let mut rest = bytes;
    while !rest.is_empty() {
        let kind = rest[0];
        let (length, after) = length_of(&rest[2.min(rest.len())..], 2)?;
        if after.len() < length {
            return Err(protocol_error("an item that runs past its PDU"));
        }
        found.push((kind, &after[..length]));
        rest = &after[length..];
    }
    Ok(found)
}

/// A big-endian length of `width` bytes, and what follows it.
fn length_of(bytes: &[u8], width: usize) -> Result<(usize, &[u8])> {
    if bytes.len() < width {
        return Err(protocol_error("a length cut short"));
    }
    let length = bytes[..width]
        .iter()
        .fold(0usize, |length, &byte| (length << 8) | usize::from(byte));
    Ok((length, &bytes[width..]))
}

/// A title in its sixteen space-padded bytes.
fn padded(title: &str) -> [u8; 16] {
    let mut field = [b' '; 16];
    for (slot, byte) in field.iter_mut().zip(title.bytes()) {
        *slot = byte;
    }
    field
}

/// A title or a UID as text, its padding off.
fn title(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes)
        .trim_matches([' ', '\0'])
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn offered() -> Associate {
        Associate {
            called: "STORE-SCP".to_string(),
            calling: "XMIP".to_string(),
            context_id: 1,
            abstract_syntax: "1.2.840.10008.5.1.4.1.1.7".to_string(),
            transfer_syntax: IMPLICIT_VR_LE.to_string(),
            result: 0,
            max_pdu: DEFAULT_MAX_PDU,
        }
    }

    #[test]
    fn a_request_and_an_acceptance_read_back_as_they_were_written() {
        let request = offered().request();
        assert_eq!(&request[4..20], b"STORE-SCP       ");
        assert_eq!(Associate::from_bytes(&request).expect("read"), offered());
        let mut accepted = offered();
        accepted.abstract_syntax = String::new();
        accepted.max_pdu = 0;
        let read = Associate::from_bytes(&accepted.accept()).expect("read");
        assert_eq!(read, accepted);
        assert!(Associate::from_bytes(&[0; 10]).is_err());
        assert!(Associate::from_bytes(&[&[0; 68][..], &[0x20, 0, 0, 9, 1][..]].concat()).is_err());
    }

    #[test]
    fn a_pdu_crosses_a_stream_and_a_value_carries_its_marks() {
        let mut wire = Vec::new();
        write(&mut wire, ASSOCIATE_RJ, &reject(CALLED_AE_NOT_RECOGNIZED)).expect("written");
        let (kind, body) = read(&mut wire.as_slice(), 64).expect("read");
        assert_eq!((kind, name(kind)), (ASSOCIATE_RJ, "A-ASSOCIATE-RJ"));
        assert_eq!(rejection(&body), "called AE title not recognized");
        assert!(read(&mut wire.as_slice(), 2).is_err());
        let value = Pdv {
            context_id: 3,
            command: false,
            last: true,
            bytes: b"data".to_vec(),
        };
        let body = value.data_transfer();
        assert_eq!(body[..6], [0, 0, 0, 6, 3, 2]);
        assert_eq!(pdvs(&body).expect("values"), vec![value]);
        assert!(pdvs(&[0, 0, 0, 9, 1, 1]).is_err());
        assert_eq!(rejection(&abort()), "a rejection with no reason in it");
    }
}
