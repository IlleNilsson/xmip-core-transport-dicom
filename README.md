# xmip-core-transport-dicom

DICOM transport: the DIMSE upper layer over TCP — an association, a C-STORE whose data set is one Stream, a release; a Receive Location is the storage SCP, a Send Location the SCU. A technology of [xmip-core-transport](https://github.com/IlleNilsson/xmip-core-transport).

A Receive Location keeps its listener, bound as its transport is opened (`Configured::listening`; on its first receive until 2026-11-16), and the associations opened on it (`transport::serving::Serving`): a peer that connects between two receives is queued and taken by the next, where until 2026-09-27 each receive bound a listener of its own and a peer between receives was refused. An association's opening — its request answered, its command read — is one turn, bounded whole by the node's `[tuning] receive_turn`, so an SCU that sends part of its request and stops is turned away and audited, and the next association is served; its data set is read off the association as the runtime asks, outside the turn (`association.rs`; ADR-0018, amendment 2026-11-14). Until then a receive accepted one association and read its opening on the Location's own thread, and a peer that was not speaking DICOM stopped the Location.

A send target is read by `net::Target` in [xmip-core-library-net](https://github.com/IlleNilsson/xmip-core-library-net), the one reading of a URI every technology calls, and its query is decoded there. Until 2026-09-28 this technology split the query off itself, without percent-decoding it.

## Acknowledgement

The SCU waits for the C-STORE-RSP, so it is answered after the whole receive
cycle. On Accepted the response carries status success, and the release
follows. On Refused it carries a failure the SCU does not store again:
`0x0124`, Refused: Not Authorized (PS3.7 Annex C.5), for a sender not
identified or not permitted; `0xC000`, Error: Cannot Understand (PS3.4 Table
B.2-1), for content refused. On Failed it carries `0xA700`, Refused: Out of
Resources (PS3.4 Table B.2-1), which tells the SCU the store may succeed when
sent again. An
association dropped without a verdict closes unanswered, and the SCU stores
again. The data set is read off the association as the runtime asks, PDU by
PDU, never gathered whole in memory. A send answered with an `0xA7xx` status
fails as retryable, any other failure as permanent.

## Toolchain

`rust-toolchain.toml` pins the toolchain for the whole estate. Do not change it
here.

## Verification

The included workflow is manual-only and calls the versioned shared workflow at
`IlleNilsson/.github@v1`.
