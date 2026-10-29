//! Bounded native frame payloads. Queued tickets and active readers share one
//! residency ledger; claiming a ticket must not make its bytes unaccounted.
use super::*;
use opencut_api_contracts::visual_preview::VisualPreviewPictureRead;
use ring::digest::{SHA256, digest};
use std::{collections::VecDeque, sync::Weak, time::Duration};

const CHUNK: usize = 65536;
const READER_OVERHEAD: usize = CHUNK * 3;
const TICKET_LIFETIME: Duration = Duration::from_secs(5);
struct Pending {
    id: String,
    session: AudioPreviewStream,
    bytes: Arc<Vec<u8>>,
    expires: Instant,
}
#[derive(Default)]
struct Residency {
    queued: VecDeque<Pending>,
    live: Vec<(Weak<Vec<u8>>, usize)>,
    reserved: usize,
    allocations: usize,
}
#[derive(Default)]
pub(super) struct FramePixels(Mutex<Residency>);
pub(super) struct PixelReservation {
    store: Arc<FramePixels>,
    length: usize,
    charged: usize,
    active: bool,
}
pub(super) struct PixelTicket {
    pub id: String,
    pub length: u32,
    pub hash: String,
}
pub(super) struct PreparedPixels {
    reservation: PixelReservation,
    ticket: PixelTicket,
    bytes: Arc<Vec<u8>>,
}
fn content_hash(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    digest(&SHA256, bytes)
        .as_ref()
        .iter()
        .flat_map(|byte| [HEX[(byte >> 4) as usize], HEX[(byte & 15) as usize]])
        .map(char::from)
        .collect()
}
impl FramePixels {
    pub fn reserve(
        self: &Arc<Self>,
        length: u32,
        capacity: u32,
    ) -> Result<PixelReservation, EditorError> {
        let mut state = lock(&self.0)?;
        state.queued.retain(|p| p.expires > Instant::now());
        state.live.retain(|(bytes, _)| bytes.strong_count() != 0);
        let charged = length as usize + READER_OVERHEAD;
        let resident: usize = state.live.iter().map(|(_, charge)| charge).sum();
        if length == 0
            || state.queued.len() + state.allocations >= 2
            || resident + state.reserved + charged > capacity as usize
        {
            return Err(EditorError::new(
                "VISUAL_BUDGET_EXCEEDED",
                "Native picture payloads exceed the reserved presentation envelope",
            ));
        }
        state.reserved += charged;
        state.allocations += 1;
        Ok(PixelReservation {
            store: self.clone(),
            length: length as usize,
            charged,
            active: true,
        })
    }
    pub fn clear(&self) {
        self.0
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .queued
            .clear();
    }
    pub fn discard(&self, id: &str) {
        self.0
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .queued
            .retain(|p| p.id != id);
    }
    fn claim(&self, session: &AudioPreviewStream, id: &str) -> Result<Arc<Vec<u8>>, EditorError> {
        if id.len() != 32 || !id.bytes().all(|c| c.is_ascii_hexdigit()) {
            return Err(error("INVALID_ARGUMENT"));
        }
        let mut state = lock(&self.0)?;
        state.queued.retain(|p| p.expires > Instant::now());
        let index = state
            .queued
            .iter()
            .position(|p| p.id == id && &p.session == session)
            .ok_or_else(|| error("PREVIEW_PICTURE_UNAVAILABLE"))?;
        Ok(state
            .queued
            .remove(index)
            .expect("located picture ticket")
            .bytes)
    }
}
impl PixelReservation {
    // Hash before the caller reacquires its presentation/audio state lock.
    // The reservation stays charged through preparation and final fencing.
    pub fn prepare(self, bytes: Vec<u8>) -> Result<PreparedPixels, EditorError> {
        if bytes.len() != self.length {
            return Err(error("INVALID_VISUAL_PLAN"));
        }
        let ticket = PixelTicket {
            id: new_identity().replace('-', ""),
            length: bytes.len() as u32,
            hash: content_hash(&bytes),
        };
        Ok(PreparedPixels {
            reservation: self,
            ticket,
            bytes: Arc::new(bytes),
        })
    }
}
impl PreparedPixels {
    pub fn publish(mut self, session: AudioPreviewStream) -> Result<PixelTicket, EditorError> {
        let mut state = lock(&self.reservation.store.0)?;
        state.reserved -= self.reservation.charged;
        state.allocations -= 1;
        self.reservation.active = false;
        state
            .live
            .push((Arc::downgrade(&self.bytes), self.reservation.charged));
        state.queued.push_back(Pending {
            id: self.ticket.id.clone(),
            session,
            bytes: self.bytes,
            expires: Instant::now() + TICKET_LIFETIME,
        });
        Ok(self.ticket)
    }
}
impl Drop for PixelReservation {
    fn drop(&mut self) {
        if self.active {
            let mut state = self.store.0.lock().unwrap_or_else(|e| e.into_inner());
            state.reserved -= self.charged;
            state.allocations -= 1;
        }
    }
}

pub struct VisualPictureReader {
    entry: Arc<Entry>,
    application: opencut_sqlite_store::EditorApplication,
    session: AudioPreviewStream,
    principal: String,
    bytes: Arc<Vec<u8>>,
    offset: usize,
}
impl VisualPictureReader {
    pub fn checkpoint(&self) -> Result<(), EditorError> {
        self.entry.checkpoint(&self.application)?;
        lock(&self.entry.state)?
            .machine
            .verify_visual_frame(&self.session, &self.principal, now())
    }
    pub fn read(&mut self) -> Result<Option<Vec<u8>>, EditorError> {
        self.checkpoint()?;
        if self.offset == self.bytes.len() {
            return Ok(None);
        }
        let end = (self.offset + CHUNK).min(self.bytes.len());
        let bytes = self.bytes[self.offset..end].to_vec();
        self.checkpoint()?;
        self.offset = end;
        Ok(Some(bytes))
    }
    pub fn byte_length(&self) -> u64 {
        self.bytes.len() as u64
    }
}
pub(crate) fn open(
    host: &mut Automation,
    input: VisualPreviewPictureRead,
) -> Result<VisualPictureReader, EditorError> {
    let entry = entry(&host.database, &input.session.session_id)?;
    authorize(host, &entry, true)?;
    lock(&entry.state)?.machine.verify_visual_frame(
        &input.session,
        host.caller.principal_id(),
        now(),
    )?;
    let bytes = entry.pixels.claim(&input.session, &input.picture_id)?;
    let reader = VisualPictureReader {
        application: opencut_sqlite_store::EditorApplication::open_managed(&entry.database)?,
        entry,
        session: input.session,
        principal: host.caller.principal_id().into(),
        bytes,
        offset: 0,
    };
    reader.checkpoint()?;
    Ok(reader)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn session() -> AudioPreviewStream {
        AudioPreviewStream {
            session_id: "s".into(),
            generation: "1".into(),
            overlay_epoch: "0".into(),
            owner_id: "owner".into(),
            lease_id: "lease".into(),
        }
    }
    #[test]
    fn prepared_pixels_keep_their_charge_without_publishing_a_ticket() {
        let store = Arc::new(FramePixels::default());
        let capacity = (1024 + READER_OVERHEAD) as u32;
        let prepared = store
            .reserve(1024, capacity)
            .unwrap()
            .prepare(vec![9; 1024])
            .unwrap();
        assert_eq!(
            store
                .claim(&session(), &prepared.ticket.id)
                .err()
                .unwrap()
                .code,
            "PREVIEW_PICTURE_UNAVAILABLE"
        );
        store.clear();
        assert!(store.reserve(1024, capacity).is_err());
        // A failed final authority/generation fence drops the prepared value.
        drop(prepared);
        assert!(store.reserve(1024, capacity).is_ok());
        assert_eq!(
            store
                .reserve(1024, capacity)
                .unwrap()
                .prepare(vec![0; 1023])
                .err()
                .unwrap()
                .code,
            "INVALID_VISUAL_PLAN"
        );
        assert!(store.reserve(1024, capacity).is_ok());
    }

    #[test]
    fn picture_hash_preserves_independent_sha256_vectors() {
        assert_eq!(
            content_hash(b""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        assert_eq!(
            content_hash(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        // Independent Python hashlib reference, retained native 960x540 RGBA
        // digest study. This is the full payload, not a fitted frame sample.
        let bytes: Vec<_> = (0..2_073_600).map(|n| (n * 37 % 251) as u8).collect();
        assert_eq!(
            content_hash(&bytes),
            "01c18a29fde359d29dd17364b1f99d0955048b93f2e26368deaeb055be75ca4c"
        );
    }

    #[test]
    fn claimed_payload_keeps_its_budget_until_last_reader_drops() {
        let store = Arc::new(FramePixels::default());
        let size = 1024;
        let capacity = (size + READER_OVERHEAD) as u32;
        let ticket = store
            .reserve(size as u32, capacity)
            .unwrap()
            .prepare(vec![7; size])
            .unwrap()
            .publish(session())
            .unwrap();
        let bytes = store.claim(&session(), &ticket.id).unwrap();
        assert_eq!(bytes.as_slice(), vec![7; size]);
        assert_eq!(
            store.claim(&session(), &ticket.id).unwrap_err().code,
            "PREVIEW_PICTURE_UNAVAILABLE"
        );
        store.clear();
        assert!(
            store.reserve(size as u32, capacity).is_err(),
            "an active binary reader still owns the payload"
        );
        drop(bytes);
        assert!(store.reserve(size as u32, capacity).is_ok());
    }
    #[test]
    fn cancelled_allocation_releases_budget_and_wrong_owner_cannot_consume_ticket() {
        let store = Arc::new(FramePixels::default());
        let capacity = (1024 + READER_OVERHEAD) as u32;
        let permit = store.reserve(1024, capacity).unwrap();
        assert!(store.reserve(1024, capacity).is_err());
        drop(permit);
        let ticket = store
            .reserve(1024, capacity)
            .unwrap()
            .prepare(vec![0; 1024])
            .unwrap()
            .publish(session())
            .unwrap();
        let mut stale = session();
        stale.generation = "2".into();
        assert!(store.claim(&stale, &ticket.id).is_err());
        assert!(store.claim(&session(), &ticket.id).is_ok());
    }
}
