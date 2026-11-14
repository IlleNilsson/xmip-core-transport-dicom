//! One SCU's association as the storage SCP takes it: the request answered
//! and the command read in one turn, the data set handed on to be read as
//! the runtime asks.
//!
//! A Receive Location's associations are kept beside its listener
//! (`transport::serving::Serving`), so each opening is one caller's turn,
//! bounded whole by the node's `[tuning] receive_turn`: an SCU that sends
//! part of its request and stops is turned away at the deadline and the
//! next association is served (ADR-0018, amendment 2026-11-14). Until then
//! a receive accepted one association and read its opening on the
//! Location's own thread, every other SCU waiting behind it for as long as
//! a read waits. An association is one store: its turn is its last, and its
//! data set is read off it outside the turn.

use std::io::{BufRead, BufReader};
use std::net::{SocketAddr, TcpStream};

use context::property::DICOM_CALLING_AE_TITLE;
use net::MAX_BODY;
use transport::deadline::Within;
use transport::error::{Result, protocol_error};
use transport::serving::{Open, Turn};
use transport::{Acknowledgement, Arrived};

use crate::answer::Answer;
use crate::{Associate, DicomTransport, data_set, dimse, pdu};

/// One SCU's connection, until its opening is read.
pub(crate) struct Association {
    /// What its opening is read from; taken with its data set.
    reader: Option<BufReader<Within>>,
    writer: TcpStream,
    peer: SocketAddr,
}

impl Association {
    /// An SCU's new connection, from `peer`.
    ///
    /// # Errors
    /// Where the connection could not be cloned for its answer.
    pub(crate) fn new(stream: Within, peer: SocketAddr) -> Result<Self> {
        let (reader, writer) = stream.split()?;
        Ok(Self {
            reader: Some(reader),
            writer,
            peer,
        })
    }
}

impl Open for Association {
    fn socket(&self) -> &TcpStream {
        &self.writer
    }

    /// A request already read beside the first octets the wait saw.
    fn waiting(&self) -> bool {
        self.reader
            .as_ref()
            .is_some_and(|reader| !reader.buffer().is_empty())
    }
}

impl DicomTransport {
    /// One association's opening, its one turn: the request answered, the
    /// command read, and the data set handed on as the arrival's body. The
    /// C-STORE-RSP waits for the verdict — success on accepted, then the
    /// release; a failure not stored again on refused;
    /// [`dimse::OUT_OF_RESOURCES`] on failed. An SCU that closed before a
    /// word has hung up.
    ///
    /// # Errors
    /// Where the connection broke, the peer is not speaking DICOM
    /// (aborted), called another title (rejected), or sent a command
    /// other than C-STORE (answered with the status that says so).
    pub(crate) fn opened(&self, association: &mut Association) -> Result<Turn<Arrived>> {
        let Some(mut reader) = association.reader.take() else {
            return Ok(Turn::Closed);
        };
        let writer = &mut association.writer;
        if reader
            .fill_buf()
            .map_err(|e| transport::error::classify("reading the request", &e))?
            .is_empty()
        {
            return Ok(Turn::Closed);
        }
        let (kind, body) = pdu::read(&mut reader, MAX_BODY)?;
        if kind != pdu::ASSOCIATE_RQ {
            pdu::write(writer, pdu::ABORT, &pdu::abort())?;
            return Err(protocol_error(format!(
                "{} where an association was expected",
                pdu::name(kind)
            )));
        }
        let offered = Associate::from_bytes(&body)?;
        if offered.called != self.title {
            let reason = pdu::reject(pdu::CALLED_AE_NOT_RECOGNIZED);
            pdu::write(writer, pdu::ASSOCIATE_RJ, &reason)?;
            return Err(protocol_error(format!(
                "an association for {}, and this entity is {}",
                offered.called, self.title
            )));
        }
        let accepted = Associate {
            abstract_syntax: String::new(),
            result: 0,
            max_pdu: self.max_pdu,
            ..offered.clone()
        };
        pdu::write(writer, pdu::ASSOCIATE_AC, &accepted.accept())?;
        let (command, data_set) = data_set::read_command(reader)?;
        let answer = Answer {
            command: command.clone(),
            context_id: offered.context_id,
            max: offered.max_pdu.max(6) as usize,
        };
        if command.field != dimse::C_STORE_RQ {
            answer.respond(writer, dimse::UNRECOGNIZED_OPERATION)?;
            return Err(protocol_error(format!(
                "a command {:#06x} where a C-STORE was expected",
                command.field
            )));
        }
        let peer = association.peer;
        let origin = format!(
            "dicom://{peer}?calling={}&called={}&sop-class={}&sop-instance={}",
            offered.calling, offered.called, command.sop_class, command.sop_instance
        );
        // Let go without a verdict, the association is shut: the SCU stores
        // again.
        let held = transport::answer::Answer::held(writer)?;
        let acknowledgement = Acknowledgement::deferred(move |verdict| {
            held.with(|stream| answer.verdict(stream, verdict))
        });
        let calling = offered.calling.trim().to_string();
        Ok(Turn::Last(
            Arrived::new(origin, data_set, acknowledgement)
                .from_peer(peer)
                .observing(DICOM_CALLING_AE_TITLE, calling),
        ))
    }
}
