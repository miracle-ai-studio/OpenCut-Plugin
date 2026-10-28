//! Bounded native frame payloads. Queued tickets and active readers share one
//! residency ledger; claiming a ticket must not make its bytes unaccounted.
use super::*;
use opencut_api_contracts::visual_preview::VisualPreviewPictureRead;
use sha2::{Digest, Sha256};
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
    pub fn publish(
        mut self,
        session: AudioPreviewStream,
        bytes: Vec<u8>,
    ) -> Result<PixelTicket, EditorError> {
        if bytes.len() != self.length {
            return Err(error("INVALID_VISUAL_PLAN"));
        }
        let ticket = PixelTicket {
            id: new_identity().replace('-', ""),
            length: bytes.len() as u32,
            hash: format!("{:x}", Sha256::digest(&bytes)),
        };
        let bytes = Arc::new(bytes);
        let mut state = lock(&self.store.0)?;
        state.reserved -= self.charged;
        state.allocations -= 1;
        self.active = false;
        state.live.push((Arc::downgrade(&bytes), self.charged));
        state.queued.push_back(Pending {
            id: ticket.id.clone(),
            session,
            bytes,
            expires: Instant::now() + TICKET_LIFETIME,
        });
        Ok(ticket)
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
    fn claimed_payload_keeps_its_budget_until_last_reader_drops() {
        let store = Arc::new(FramePixels::default());
        let size = 1024;
        let capacity = (size + READER_OVERHEAD) as u32;
        let ticket = store
            .reserve(size as u32, capacity)
            .unwrap()
            .publish(session(), vec![7; size])
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
            .publish(session(), vec![0; 1024])
            .unwrap();
        let mut stale = session();
        stale.generation = "2".into();
        assert!(store.claim(&stale, &ticket.id).is_err());
        assert!(store.claim(&session(), &ticket.id).is_ok());
    }
}
