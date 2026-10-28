//! Container-precise cluster/block timing; payload bytes are never allocated.
use super::*;
use matroska_demuxer::{Frame, MatroskaFile, TrackType};
use std::{
    cell::RefCell,
    io::{self, Read, Seek, SeekFrom},
    rc::Rc,
};
struct State<'a> {
    source: &'a mut dyn ByteSource,
    position: u64,
    read: u64,
    operations: usize,
    deadline: Instant,
    error: Option<MediaError>,
}
impl State<'_> {
    fn check(&mut self) -> MediaResult<()> {
        self.operations += 1;
        if self.operations > 1_000_000 {
            return Err(invalid("Matroska metadata operation budget exceeded"));
        }
        check(self.source, self.deadline)
    }
    fn remember<T>(&mut self, result: MediaResult<T>) -> io::Result<T> {
        result.map_err(|e| {
            if self.error.is_none() {
                self.error = Some(e);
            }
            io::Error::other("Authorized Matroska byte port failed")
        })
    }
}
struct Reader<'a>(Rc<RefCell<State<'a>>>);
impl Read for Reader<'_> {
    fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
        let mut state = self.0.borrow_mut();
        let result = (|| {
            state.check()?;
            if state.position == state.source.len() || bytes.is_empty() {
                return Ok(0);
            }
            let count = bytes
                .len()
                .min(65536)
                .min((state.source.len() - state.position) as usize);
            if state.read + count as u64 > MAX_IO {
                return Err(invalid("Matroska read budget exceeded"));
            }
            let offset = state.position;
            let n = state.source.read_at(offset, &mut bytes[..count])?;
            if n == 0 || n > count {
                return Err(invalid("Matroska byte extent changed"));
            }
            state.position += n as u64;
            state.read += n as u64;
            state.check()?;
            Ok(n)
        })();
        state.remember(result)
    }
}
impl Seek for Reader<'_> {
    fn seek(&mut self, from: SeekFrom) -> io::Result<u64> {
        let mut state = self.0.borrow_mut();
        let result = (|| {
            state.check()?;
            let next = match from {
                SeekFrom::Start(n) => i128::from(n),
                SeekFrom::Current(n) => i128::from(state.position) + i128::from(n),
                SeekFrom::End(n) => i128::from(state.source.len()) + i128::from(n),
            };
            if next < 0 || next > i128::from(state.source.len()) {
                return Err(invalid("Matroska seek exceeds immutable source"));
            }
            state.position = next as u64;
            Ok(state.position)
        })();
        state.remember(result)
    }
}
struct Block {
    offset: u64,
    at: f64,
    duration: f64,
    count: u16,
}
fn cluster(
    blocks: &mut Vec<Block>,
    factor: f64,
    default: Option<f64>,
    stats: &mut Statistics,
) -> MediaResult<bool> {
    let mut order: Vec<_> = (0..blocks.len()).collect();
    order.sort_by(|&a, &b| blocks[a].at.total_cmp(&blocks[b].at));
    for pair in order.windows(2) {
        let at = blocks[pair[1]].at;
        blocks[pair[0]].duration = at - blocks[pair[0]].at;
    }
    if let Some(&last) = order.last() {
        let b = &mut blocks[last];
        if b.duration == 0.0 {
            b.duration = default.unwrap_or(0.) * f64::from(b.count);
        }
    }
    for b in blocks.iter() {
        for index in 0..b.count {
            let timestamp = b.at + b.duration * f64::from(index) / f64::from(b.count);
            if !stats.observe(
                timestamp / factor,
                (b.duration / f64::from(b.count)) / factor,
            )? {
                return Ok(false);
            }
        }
    }
    blocks.clear();
    Ok(true)
}
pub(super) fn probe(
    source: &mut dyn ByteSource,
    deadline: Instant,
) -> MediaResult<ClassicPacketRate> {
    let shared = Rc::new(RefCell::new(State {
        source,
        position: 0,
        read: 0,
        operations: 0,
        deadline,
        error: None,
    }));
    let result = (|| {
        let mut input =
            MatroskaFile::open(Reader(shared.clone())).map_err(|e| invalid(e.to_string()))?;
        let track = input
            .tracks()
            .iter()
            .find(|t| t.track_type() == TrackType::Video && t.flag_default())
            .or_else(|| {
                input
                    .tracks()
                    .iter()
                    .find(|t| t.track_type() == TrackType::Video)
            });
        let Some(track) = track else {
            return Statistics::default().finish();
        };
        let number = track.track_number().get();
        let factor = 1e9 / input.info().timestamp_scale().get() as f64;
        let default = track
            .default_duration()
            .map(|d| factor * d.get() as f64 / 1e9);
        let mut frame = Frame::default();
        let mut current = None;
        let mut blocks: Vec<Block> = Vec::new();
        let mut stats = Statistics::default();
        let mut packets = 0;
        while input
            .next_frame_metadata(&mut frame)
            .map_err(|e| invalid(e.to_string()))?
        {
            shared.borrow_mut().check()?;
            packets += 1;
            if packets > MAX_PACKETS {
                return Err(invalid("Matroska packet budget exceeded"));
            }
            if current.is_some_and(|offset| offset != frame.cluster_offset) {
                if !cluster(&mut blocks, factor, default, &mut stats)? {
                    return stats.finish();
                }
            }
            current = Some(frame.cluster_offset);
            if frame.track != number {
                continue;
            }
            if frame.frame_count == 0
                || frame.frame_index >= frame.frame_count
                || frame.signed_timestamp.abs() > ((1i128 << 53) - 1)
            {
                return Err(invalid("Matroska packet metadata is outside admission"));
            }
            if frame.frame_index == 0 {
                blocks.push(Block {
                    offset: frame.block_offset,
                    at: frame.signed_timestamp as f64,
                    duration: frame.duration.unwrap_or(0) as f64,
                    count: frame.frame_count,
                });
            } else if blocks
                .last()
                .is_none_or(|b| b.offset != frame.block_offset || b.count != frame.frame_count)
            {
                return Err(invalid("Matroska lace changed source block"));
            }
        }
        cluster(&mut blocks, factor, default, &mut stats)?;
        stats.finish()
    })();
    // Upstream may classify an IO failure as EOF; authorization, cancellation,
    // read budgets and deadlines must retain their original failure instead.
    let error = shared.borrow_mut().error.take();
    if let Some(e) = error {
        return Err(e);
    }
    shared.borrow_mut().check()?;
    result
}
