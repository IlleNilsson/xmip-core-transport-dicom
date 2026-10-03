//! One DIMSE message off an association: its command whole, and its data
//! set as a reader over the presentation data values that carry it, read
//! as the runtime asks rather than gathered in memory.

use std::io::Read;

use net::MAX_BODY;
use transport::error::{Result, protocol_error};

use crate::dimse::Command;
use crate::pdu;

/// The data set of one message, read PDU by PDU off the association.
pub struct DataSet<R> {
    reader: R,
    /// What the last PDU read carried and is not read yet.
    pending: Vec<u8>,
    at: usize,
    /// Whether the value marked last has been read off the wire.
    ended: bool,
}

impl<R: Read> DataSet<R> {
    /// The data set whose first bytes are `pending`, already read with its
    /// command, and whose rest `reader` carries unless `ended`.
    const fn new(reader: R, pending: Vec<u8>, ended: bool) -> Self {
        Self {
            reader,
            pending,
            at: 0,
            ended,
        }
    }

    /// The next P-DATA-TF's data values into `pending`.
    fn next_pdu(&mut self) -> Result<()> {
        let (kind, body) = pdu::read(&mut self.reader, MAX_BODY)?;
        if kind != pdu::P_DATA {
            return Err(protocol_error(format!(
                "{} where data was expected",
                pdu::name(kind)
            )));
        }
        self.pending.clear();
        self.at = 0;
        for value in pdu::pdvs(&body)? {
            if value.command {
                return Err(protocol_error("a command inside a data set"));
            }
            self.pending.extend_from_slice(&value.bytes);
            self.ended |= value.last;
        }
        Ok(())
    }
}

impl<R: Read> Read for DataSet<R> {
    fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
        loop {
            let left = &self.pending[self.at..];
            if !left.is_empty() || buffer.is_empty() {
                let count = left.len().min(buffer.len());
                buffer[..count].copy_from_slice(&left[..count]);
                self.at += count;
                return Ok(count);
            }
            if self.ended {
                return Ok(0);
            }
            self.next_pdu().map_err(std::io::Error::other)?;
        }
    }
}

/// One message's command off `reader`, and its data set to read on: empty
/// where the command says none follows.
///
/// # Errors
/// Where the association broke, or sent what is not data, or a data set
/// before its command.
pub fn read_command<R: Read>(mut reader: R) -> Result<(Command, DataSet<R>)> {
    let mut command_bytes = Vec::new();
    loop {
        let (kind, body) = pdu::read(&mut reader, MAX_BODY)?;
        if kind != pdu::P_DATA {
            return Err(protocol_error(format!(
                "{} where data was expected",
                pdu::name(kind)
            )));
        }
        let mut values = pdu::pdvs(&body)?.into_iter();
        while let Some(value) = values.next() {
            if !value.command {
                return Err(protocol_error("a data set before its command"));
            }
            command_bytes.extend_from_slice(&value.bytes);
            if value.last {
                let command = Command::from_bytes(&command_bytes)?;
                // What follows the command in the same PDU is its data set.
                let mut pending = Vec::new();
                let mut ended = !command.has_data_set;
                for value in values {
                    if value.command {
                        return Err(protocol_error("a command inside a data set"));
                    }
                    pending.extend_from_slice(&value.bytes);
                    ended |= value.last;
                }
                return Ok((command, DataSet::new(reader, pending, ended)));
            }
        }
    }
}

/// One message off `reader`, its data set gathered whole: what an SCU
/// does with the response it waits for.
///
/// # Errors
/// As [`read_command`], and where the data set breaks off.
pub fn read_message(reader: &mut impl Read) -> Result<(Command, Vec<u8>)> {
    let (command, mut data_set) = read_command(reader)?;
    let mut bytes = Vec::new();
    data_set
        .read_to_end(&mut bytes)
        .map_err(|e| transport::error::classify("reading a data set", &e))?;
    Ok((command, bytes))
}
