//! What the SCP answers a store with, and how the association ends after
//! it: the C-STORE-RSP for the verdict, then the release on success, or a
//! close that reads to the SCU's own when the data set may be left unread.

use std::net::TcpStream;

use net::MAX_BODY;
use transport::error::Result;
use transport::socket;
use transport::{Refusal, Verdict};

use crate::dimse::{self, Command};
use crate::pdu;

/// What a C-STORE-RSP is written from: the request it answers, and the
/// presentation context and PDU size the association agreed.
pub(crate) struct Answer {
    pub(crate) command: Command,
    pub(crate) context_id: u8,
    pub(crate) max: usize,
}

impl Answer {
    /// The response with `status`, in PDUs no longer than the SCU reads.
    pub(crate) fn respond(&self, writer: &mut TcpStream, status: u16) -> Result<()> {
        let response = self.command.response(status).to_bytes();
        for body in dimse::fragments(&response, self.context_id, true, self.max) {
            pdu::write(writer, pdu::P_DATA, &body)?;
        }
        Ok(())
    }

    /// The verdict answered, and the association ended. Accepted: success,
    /// then the release the SCU sends once answered, read straight off the
    /// socket because nothing it sent is left unread. Not stored: the data
    /// set may be partly unread, so the association ends through
    /// [`socket::finish`], never by a drop that resets the answer away.
    pub(crate) fn verdict(&self, writer: &mut TcpStream, verdict: Verdict) -> Result<()> {
        let status = match verdict {
            Verdict::Accepted => {
                self.respond(writer, dimse::SUCCESS)?;
                let (kind, _) = pdu::read(writer, MAX_BODY)?;
                if kind == pdu::RELEASE_RQ {
                    pdu::write(writer, pdu::RELEASE_RP, &[0; 4])?;
                }
                return Ok(());
            }
            Verdict::Refused(Refusal::Unacceptable) => dimse::CANNOT_UNDERSTAND,
            Verdict::Refused(_) => dimse::NOT_AUTHORIZED,
            Verdict::Failed => dimse::OUT_OF_RESOURCES,
        };
        self.respond(writer, status)?;
        socket::finish(writer)
    }
}
