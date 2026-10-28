use super::*;
use std::{cell::RefCell, rc::Rc};
#[derive(Default)]
struct Usage {
    live: u64,
    peak: u64,
    limit: u64,
}
/// PCM reservations follow actual ownership, including old and replacement
/// arrays during a grow/copy. Dropping an input/FIFO returns its reservation.
#[derive(Clone)]
pub struct PcmMemory(Rc<RefCell<Usage>>);
impl PcmMemory {
    pub fn new(limit: u64) -> Result<Self> {
        if limit == 0 {
            return Err(budget("Native PCM needs a positive memory envelope"));
        }
        Ok(Self(Rc::new(RefCell::new(Usage {
            limit,
            ..Usage::default()
        }))))
    }
    pub fn live_bytes(&self) -> u64 {
        self.0.borrow().live
    }
    pub fn peak_bytes(&self) -> u64 {
        self.0.borrow().peak
    }
    /// Charge bounded external codec/frame storage before entering libav. The
    /// reservation remains live while the returned frame is copied into owned
    /// PCM; both owners count during that copy.
    pub fn reserve(&self, bytes: u64) -> Result<PcmReservation> {
        {
            let mut usage = self.0.borrow_mut();
            let next = usage
                .live
                .checked_add(bytes)
                .ok_or_else(|| budget("Native PCM residency overflow"))?;
            if next > usage.limit {
                return Err(budget("Native PCM exceeds its reserved memory"));
            }
            usage.live = next;
            usage.peak = usage.peak.max(next);
        }
        Ok(PcmReservation {
            memory: self.clone(),
            bytes,
        })
    }
    pub fn array(&self, samples: usize) -> Result<PcmArray> {
        let bytes = u64::try_from(samples)
            .ok()
            .and_then(|n| n.checked_mul(4))
            .ok_or_else(|| budget("Native PCM allocation overflow"))?;
        let mut result = PcmArray {
            _reservation: self.reserve(bytes)?,
            data: Vec::new(),
        };
        result
            .data
            .try_reserve_exact(samples)
            .map_err(|_| budget("Cannot allocate admitted native PCM"))?;
        result.data.resize(samples, 0.0);
        Ok(result)
    }
}
pub struct PcmArray {
    _reservation: PcmReservation,
    data: Vec<f32>,
}
pub struct PcmReservation {
    memory: PcmMemory,
    bytes: u64,
}
impl PcmArray {
    pub(crate) fn memory(&self) -> PcmMemory {
        self._reservation.memory.clone()
    }
    pub fn as_slice(&self) -> &[f32] {
        &self.data
    }
    pub fn as_mut_slice(&mut self) -> &mut [f32] {
        &mut self.data
    }
    pub fn len(&self) -> usize {
        self.data.len()
    }
    pub fn is_empty(&self) -> bool {
        self.data.is_empty()
    }
}
impl Drop for PcmReservation {
    fn drop(&mut self) {
        self.memory.0.borrow_mut().live -= self.bytes;
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn all_owners_and_replacement_storage_remain_charged_until_drop() {
        let memory = PcmMemory::new(80).unwrap();
        let first = memory.array(12).unwrap();
        assert!(memory.array(9).is_err());
        assert_eq!(memory.live_bytes(), 48);
        let replacement = memory.array(8).unwrap();
        assert_eq!(memory.peak_bytes(), 80);
        drop(first);
        assert_eq!(memory.live_bytes(), 32);
        drop(replacement);
        assert_eq!(memory.live_bytes(), 0);
    }
}
