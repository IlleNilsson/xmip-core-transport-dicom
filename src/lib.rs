#![forbid(unsafe_code)]

//! Streams that arrive as DICOM stores. One C-STORE is one Stream: the
//! data set as the sender encoded it, with the SOP class and instance and
//! the two titles beside it.
//!
//! DICOM is how images leave a modality and reach an archive: an
//! application entity opens an association with another, naming both
//! titles and the SOP class it will store, sends the C-STORE command and
//! the data set in presentation data values no longer than the PDU the far
//! end reads, takes the response, and releases (PS3.7, PS3.8). A Receive
//! Location is the storage SCP — it answers to its title, accepts the one
//! presentation context in implicit VR little endian, takes the store and
//! acknowledges it; a Send Location is the SCU, which opens, stores one
//! data set and releases. A title this side does not answer to is
//! rejected; a peer that is not speaking DICOM is aborted.
//!
//! The data set is carried whole and unread: what a modality encoded is
//! what the archive receives, and reading the elements inside it is a
//! contract technology's work.
//!
//! The origin URI carries what the association and the command knew:
//! `dicom://peer?calling=MODALITY&called=XMIP&sop-class=1.2.840.10008.5.1.4.1.1.7`
//! `&sop-instance=2.25.…`.
//! A target is `dicom://host:104?called=ARCHIVE`, or a bare `host:port`
//! calling the configured title.

pub mod dimse;
pub mod pdu;

use std::io::{BufReader, Read};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicU16, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

pub use dimse::Command;
pub use pdu::{Associate, Pdv};
use transport::error::{Result, protocol_error};
use transport::listening::{Accepting, Listening};
use transport::loopback::{FarEnd, LOOPBACK_TIMEOUT, Loopback};
use transport::socket;
use transport::wire::MAX_BODY;
use transport::{Arrived, Directions, Transport};

/// Secondary Capture Image Storage, the SOP class a store of opaque bytes
/// goes under until [`DicomTransport::storing`] names another.
pub const SECONDARY_CAPTURE: &str = "1.2.840.10008.5.1.4.1.1.7";

#[derive(Clone)]
pub struct DicomTransport {
    bind: String,
    /// This side's title.
    title: String,
    /// The title a send calls unless the target names one.
    called: String,
    sop_class: String,
    max_pdu: u32,
    timeout: Option<Duration>,
}

impl DicomTransport {
    /// Listen at `bind`; `0.0.0.0:104` is the standard port. This side is
    /// `XMIP` and calls `ANY-SCP` until [`Self::titled`].
    #[must_use]
    pub fn new(bind: impl Into<String>) -> Self {
        Self {
            bind: bind.into(),
            title: "XMIP".to_string(),
            called: "ANY-SCP".to_string(),
            sop_class: SECONDARY_CAPTURE.to_string(),
            max_pdu: pdu::DEFAULT_MAX_PDU,
            timeout: None,
        }
    }

    /// Answer to `title`, and call `called`.
    #[must_use]
    pub fn titled(mut self, title: &str, called: &str) -> Self {
        self.title = title.to_string();
        self.called = called.to_string();
        self
    }

    /// Store under `sop_class`.
    #[must_use]
    pub fn storing(mut self, sop_class: &str) -> Self {
        self.sop_class = sop_class.to_string();
        self
    }

    /// Read PDUs no longer than `max_pdu`, and say so when accepting.
    #[must_use]
    pub const fn reading_pdus_of(mut self, max_pdu: u32) -> Self {
        self.max_pdu = max_pdu;
        self
    }

    /// Give up on a peer that stops mid-association.
    #[must_use]
    pub const fn timing_out_after(mut self, timeout: Duration) -> Self {
        self.timeout = Some(timeout);
        self
    }

    /// Bind as the SCP and report the address actually assigned.
    ///
    /// # Errors
    /// Where the address is taken, malformed, or not permitted.
    pub fn bind(&self) -> Result<(TcpListener, String)> {
        socket::bind_tcp(&self.bind)
    }

    /// Accept one association, take its one store and release.
    ///
    /// # Errors
    /// Where the connection broke, the peer is not speaking DICOM
    /// (aborted), called another title (rejected), or sent a command
    /// other than C-STORE (answered with the status that says so).
    pub fn accept_one(&self, listener: &TcpListener) -> Result<Arrived> {
        let (stream, peer) = socket::accept_tcp(listener, self.timeout)?;
        let (mut reader, mut writer) = socket::split(stream)?;
        let (kind, body) = pdu::read(&mut reader, MAX_BODY)?;
        if kind != pdu::ASSOCIATE_RQ {
            pdu::write(&mut writer, pdu::ABORT, &pdu::abort())?;
            return Err(protocol_error(format!(
                "{} where an association was expected",
                pdu::name(kind)
            )));
        }
        let offered = Associate::from_bytes(&body)?;
        if offered.called != self.title {
            let reason = pdu::reject(pdu::CALLED_AE_NOT_RECOGNIZED);
            pdu::write(&mut writer, pdu::ASSOCIATE_RJ, &reason)?;
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
        pdu::write(&mut writer, pdu::ASSOCIATE_AC, &accepted.accept())?;
        let (command, data_set) = read_message(&mut reader)?;
        let status = if command.field == dimse::C_STORE_RQ {
            dimse::SUCCESS
        } else {
            dimse::UNRECOGNIZED_OPERATION
        };
        let response = command.response(status).to_bytes();
        let max = offered.max_pdu.max(6) as usize;
        for body in dimse::fragments(&response, offered.context_id, true, max) {
            pdu::write(&mut writer, pdu::P_DATA, &body)?;
        }
        if status != dimse::SUCCESS {
            return Err(protocol_error(format!(
                "a command {:#06x} where a C-STORE was expected",
                command.field
            )));
        }
        let (kind, _) = pdu::read(&mut reader, MAX_BODY)?;
        if kind == pdu::RELEASE_RQ {
            pdu::write(&mut writer, pdu::RELEASE_RP, &[0; 4])?;
        }
        let origin = format!(
            "dicom://{peer}?calling={}&called={}&sop-class={}&sop-instance={}",
            offered.calling, offered.called, command.sop_class, command.sop_instance
        );
        Ok(Arrived::new(origin, data_set))
    }

    /// Where a send is going: the address, and the title to call.
    fn resolve(&self, target: &str) -> (String, String) {
        let rest = socket::target("dicom", target).map_or(target, |(authority, _)| authority);
        match rest.split_once('?') {
            Some((address, query)) => {
                let called = query
                    .split('&')
                    .find_map(|pair| pair.strip_prefix("called="))
                    .unwrap_or(&self.called);
                (address.to_string(), called.to_string())
            }
            None => (rest.to_string(), self.called.clone()),
        }
    }

    /// Open an association with `called` at `address`, store `bytes` as
    /// one data set, and release.
    fn store(&self, address: &str, called: &str, bytes: &[u8]) -> Result<()> {
        let stream = socket::connect_tcp(address, self.timeout)?;
        let (mut reader, mut writer) = socket::split(stream)?;
        let offered = Associate {
            called: called.to_string(),
            calling: self.title.clone(),
            context_id: 1,
            abstract_syntax: self.sop_class.clone(),
            transfer_syntax: pdu::IMPLICIT_VR_LE.to_string(),
            result: 0,
            max_pdu: self.max_pdu,
        };
        pdu::write(&mut writer, pdu::ASSOCIATE_RQ, &offered.request())?;
        let accepted = associated(&mut reader)?;
        let max = if accepted.max_pdu == 0 {
            self.max_pdu
        } else {
            accepted.max_pdu
        } as usize;
        let command = Command::store(&self.sop_class, &next_instance(), next_message_id());
        let command_bodies = dimse::fragments(&command.to_bytes(), 1, true, max);
        let data_bodies = dimse::fragments(bytes, 1, false, max);
        for body in command_bodies.iter().chain(&data_bodies) {
            pdu::write(&mut writer, pdu::P_DATA, body)?;
        }
        let (response, _) = read_message(&mut reader)?;
        if response.status != dimse::SUCCESS {
            return Err(protocol_error(format!(
                "the SCP answered the store with status {:#06x}",
                response.status
            )));
        }
        pdu::write(&mut writer, pdu::RELEASE_RQ, &[0; 4])?;
        let (kind, _) = pdu::read(&mut reader, MAX_BODY)?;
        if kind != pdu::RELEASE_RP {
            return Err(protocol_error(format!(
                "{} where the release reply was expected",
                pdu::name(kind)
            )));
        }
        Ok(())
    }
}

/// The acceptance the far end answered the request with.
fn associated(reader: &mut BufReader<TcpStream>) -> Result<Associate> {
    let (kind, body) = pdu::read(reader, MAX_BODY)?;
    match kind {
        pdu::ASSOCIATE_AC => {
            let accepted = Associate::from_bytes(&body)?;
            if accepted.result != 0 {
                return Err(protocol_error(format!(
                    "the presentation context was not accepted: result {}",
                    accepted.result
                )));
            }
            Ok(accepted)
        }
        pdu::ASSOCIATE_RJ => Err(protocol_error(format!(
            "the association was rejected: {}",
            pdu::rejection(&body)
        ))),
        other => Err(protocol_error(format!(
            "{} where the acceptance was expected",
            pdu::name(other)
        ))),
    }
}

/// One message off the association: the command, and the data set that
/// follows where the command says one does.
fn read_message(reader: &mut impl Read) -> Result<(Command, Vec<u8>)> {
    let mut command_bytes = Vec::new();
    let mut data_set = Vec::new();
    let mut command = None;
    loop {
        let (kind, body) = pdu::read(reader, MAX_BODY)?;
        if kind != pdu::P_DATA {
            return Err(protocol_error(format!(
                "{} where data was expected",
                pdu::name(kind)
            )));
        }
        for value in pdu::pdvs(&body)? {
            if value.command {
                command_bytes.extend_from_slice(&value.bytes);
                if value.last {
                    let read = Command::from_bytes(&command_bytes)?;
                    if !read.has_data_set {
                        return Ok((read, data_set));
                    }
                    command = Some(read);
                }
            } else {
                data_set.extend_from_slice(&value.bytes);
                if value.last {
                    let command =
                        command.ok_or_else(|| protocol_error("a data set before its command"))?;
                    return Ok((command, data_set));
                }
            }
        }
    }
}

/// A message id no other message from this process carries at once.
fn next_message_id() -> u16 {
    static COUNTER: AtomicU16 = AtomicU16::new(1);
    COUNTER.fetch_add(1, Ordering::Relaxed)
}

/// A SOP instance UID no other store from this process carries.
fn next_instance() -> String {
    static COUNTER: AtomicU16 = AtomicU16::new(1);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |since| since.as_nanos());
    format!("2.25.{nanos}{}", COUNTER.fetch_add(1, Ordering::Relaxed))
}

impl Transport for DicomTransport {
    fn name(&self) -> &'static str {
        "dicom"
    }

    fn directions(&self) -> Directions {
        Directions::BOTH
    }

    fn receive(&self) -> Result<Vec<Arrived>> {
        let (listener, _) = self.bind()?;
        Ok(vec![self.accept_one(&listener)?])
    }

    fn send(&self, target: &str, bytes: &[u8]) -> Result<()> {
        let (address, called) = self.resolve(target);
        self.store(&address, &called, bytes)
    }
}

impl DicomTransport {
    /// Both ends on this machine: an ephemeral local port, the loopback
    /// timeout on the association, and one title — `XMIP` — storing to
    /// itself.
    #[must_use]
    pub fn loopback() -> Self {
        Self::new("127.0.0.1:0").timing_out_after(LOOPBACK_TIMEOUT)
    }
}

impl Accepting for DicomTransport {
    fn take_one(self, listener: &TcpListener) -> Result<Arrived> {
        self.accept_one(listener)
    }
}

impl Loopback for DicomTransport {
    fn far_end(&self) -> Result<Box<dyn FarEnd>> {
        Ok(Box::new(Listening::new(self.clone(), self.bind()?)))
    }

    /// The near end calls the far end by this side's title, which is what
    /// the far end answers to.
    fn send_to(&self, address: &str, payload: &[u8]) -> Result<()> {
        self.clone()
            .send(&format!("dicom://{address}?called={}", self.title), payload)
    }
}

#[cfg(test)]
mod tests {
    use std::io::Write;
    use transport::payload::edge_payloads;

    use super::*;

    fn secs(n: u64) -> Duration {
        Duration::from_secs(n)
    }

    fn scp() -> (DicomTransport, TcpListener, String) {
        let scp = DicomTransport::new("127.0.0.1:0")
            .titled("ARCHIVE", "MODALITY")
            .timing_out_after(secs(2));
        let (listener, address) = scp.bind().expect("binding");
        (scp, listener, address)
    }

    fn scu() -> DicomTransport {
        DicomTransport::new("127.0.0.1:0")
            .titled("MODALITY", "ARCHIVE")
            .timing_out_after(secs(2))
    }

    #[test]
    fn a_data_set_is_stored_in_fragments_and_acknowledged() {
        let pair = DicomTransport::loopback().titled("ARCHIVE", "MODALITY");
        let long: Vec<u8> = (0..(1usize << 20))
            .map(|at| u8::try_from(at % 251).unwrap_or(0))
            .collect();
        let first = pair.round(b"DICM").expect("first");
        assert_eq!(first.bytes, b"DICM");
        assert!(first.origin_uri.starts_with("dicom://127.0.0.1:"));
        assert!(
            first
                .origin_uri
                .contains("?calling=ARCHIVE&called=ARCHIVE&sop-class=")
        );
        assert!(first.origin_uri.contains("&sop-instance=2.25."));
        let second = pair.round(&long).expect("second");
        assert_eq!(second.bytes, long);
        let third = pair.reading_pdus_of(64).round(&[]).expect("third");
        assert!(third.bytes.is_empty());
    }

    #[test]
    fn the_loopback_returns_the_edge_payloads_whole() {
        let pair = DicomTransport::loopback();
        for (name, bytes) in edge_payloads() {
            let arrived = pair
                .round(&bytes)
                .unwrap_or_else(|error| panic!("{name}: {error}"));
            assert_eq!(arrived.bytes, bytes, "{name}");
            assert!(arrived.origin_uri.contains("?calling=XMIP&called=XMIP&"));
        }
        assert!(pair.ceiling().is_none());
        assert!(pair.refuses(b"\r\n\0").is_none());
    }

    #[test]
    fn an_association_for_another_title_is_rejected() {
        let (scp, listener, address) = scp();
        let sender =
            std::thread::spawn(move || scu().send(&format!("{address}?called=OTHER"), b"x"));
        let error = scp.accept_one(&listener).expect_err("rejected");
        assert!(error.message.contains("OTHER"), "{error}");
        let error = sender.join().expect("thread").expect_err("rejected");
        assert!(!error.retryable);
        assert!(
            error.message.contains("called AE title not recognized"),
            "{error}"
        );
    }

    #[test]
    fn a_peer_that_is_not_speaking_dicom_is_aborted() {
        let (scp, listener, address) = scp();
        let poster = std::thread::spawn(move || {
            let mut stream = socket::connect_tcp(&address, Some(secs(2))).expect("connect");
            stream.write_all(b"GET / HTTP/1.1\r\n\r\n").expect("write");
            let mut answer = Vec::new();
            stream.read_to_end(&mut answer).expect("read");
            answer
        });
        let error = scp.accept_one(&listener).expect_err("not DICOM");
        assert!(!error.retryable, "{error}");
        let answer = poster.join().expect("thread");
        assert!(answer.is_empty() || answer[0] == pdu::ABORT);
    }

    #[test]
    fn a_command_that_is_not_a_store_is_answered_with_its_status() {
        let (scp, listener, address) = scp();
        let caller = std::thread::spawn(move || {
            let stream = socket::connect_tcp(&address, Some(secs(2))).expect("connect");
            let (mut reader, mut writer) = socket::split(stream).expect("split");
            let offered = Associate {
                called: "ARCHIVE".to_string(),
                calling: "ECHO".to_string(),
                context_id: 1,
                abstract_syntax: "1.2.840.10008.1.1".to_string(),
                transfer_syntax: pdu::IMPLICIT_VR_LE.to_string(),
                result: 0,
                max_pdu: 0,
            };
            pdu::write(&mut writer, pdu::ASSOCIATE_RQ, &offered.request()).expect("rq");
            associated(&mut reader).expect("accepted");
            let mut echo = Command::store("1.2.840.10008.1.1", "", 1);
            echo.field = 0x0030;
            echo.has_data_set = false;
            let body = dimse::fragments(&echo.to_bytes(), 1, true, 16_384);
            pdu::write(&mut writer, pdu::P_DATA, &body[0]).expect("command");
            read_message(&mut reader).expect("response").0.status
        });
        assert!(scp.accept_one(&listener).is_err());
        assert_eq!(
            caller.join().expect("thread"),
            dimse::UNRECOGNIZED_OPERATION
        );
        assert_eq!(scp.name(), "dicom");
        assert_eq!(scp.directions(), Directions::BOTH);
        assert!(scp.claims().is_none());
        assert_eq!(
            scp.resolve("dicom://h:104?called=X"),
            ("h:104".to_string(), "X".to_string())
        );
        assert_eq!(
            scp.resolve("h:104"),
            ("h:104".to_string(), "MODALITY".to_string())
        );
    }
}
