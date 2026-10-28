//! Fixed packet-prefix observations over the current authorized byte port.
//! Container parsers observe time; application import planning still owns FPS.
use crate::{
    MediaError, MediaResult,
    native_decode::{ByteSource, input::Bridge},
};
use ffmpeg_next as av;
use opencut_media_contracts::ClassicPacketRate;
use std::time::Instant;
mod matroska;
const MAX_IO: u64 = 512 * 1024 * 1024;
const MAX_PACKETS: usize = 100_000;
#[derive(Default)]
struct Statistics {
    count: u64,
    start: Option<f64>,
    end: Option<f64>,
}
impl Statistics {
    fn observe(&mut self, timestamp: f64, duration: f64) -> MediaResult<bool> {
        if !timestamp.is_finite() || !duration.is_finite() || duration < 0.0 {
            return Err(invalid("Invalid packet timing"));
        }
        if self.count >= 100 && self.end.is_some_and(|end| timestamp >= end) {
            return Ok(false);
        }
        self.start = Some(self.start.map_or(timestamp, |start| start.min(timestamp)));
        let end = timestamp + duration;
        self.end = Some(self.end.map_or(end, |old| old.max(end)));
        self.count += 1;
        Ok(true)
    }
    fn finish(self) -> MediaResult<ClassicPacketRate> {
        let rate = self
            .start
            .zip(self.end)
            .and_then(|(start, end)| {
                let rate = self.count as f64 / (end - start);
                (self.count > 0 && rate.is_finite() && rate > 0.0).then_some(rate)
            })
            .map(|rate| {
                format!("{rate:.15e}")
                    .parse::<f64>()
                    .expect("finite decimal rate")
            });
        let result = ClassicPacketRate {
            schema_version: 1,
            packet_count: self.count,
            average_packet_rate: rate,
        };
        result.validate().map_err(invalid)?;
        Ok(result)
    }
}
fn invalid(message: impl Into<String>) -> MediaError {
    MediaError::InvalidToolOutput {
        tool: "native-packet-rate".into(),
        detail: message.into(),
    }
}
fn check(source: &mut dyn ByteSource, deadline: Instant) -> MediaResult<()> {
    source.checkpoint()?;
    if Instant::now() >= deadline {
        return Err(MediaError::Backpressure(
            "Packet-rate deadline exceeded".into(),
        ));
    }
    Ok(())
}
pub fn probe_packet_rate(
    source: &mut dyn ByteSource,
    deadline: Instant,
) -> MediaResult<ClassicPacketRate> {
    check(source, deadline)?;
    let mut signature = [0; 4];
    let n = source.read_at(0, &mut signature)?;
    if n == 4 && signature == [0x1a, 0x45, 0xdf, 0xa3] {
        return matroska::probe(source, deadline);
    }
    let mut bridge = Bridge::new(source.len())?;
    let mut input = bridge.open(source, MAX_IO, deadline)?;
    // The retained upstream demuxers put default tracks first, preserving
    // source order within each disposition. Do not use codec/size heuristics.
    let selected = input
        .streams()
        .find(|s| {
            s.parameters().medium() == av::media::Type::Video
                && s.disposition()
                    .contains(av::format::stream::Disposition::DEFAULT)
        })
        .or_else(|| {
            input
                .streams()
                .find(|s| s.parameters().medium() == av::media::Type::Video)
        });
    let Some(stream) = selected else {
        return Statistics::default().finish();
    };
    let index = stream.index();
    let tb = stream.time_base();
    if tb.numerator() <= 0 || tb.denominator() <= 0 {
        return Err(invalid("Video track has no exact time base"));
    }
    let mut statistics = Statistics::default();
    for _ in 0..MAX_PACKETS {
        check(source, deadline)?;
        let remaining = MAX_IO
            .checked_sub(bridge.bytes_read())
            .ok_or_else(|| invalid("Packet probe read budget exceeded"))?;
        let mut packet = av::Packet::empty();
        let result = bridge.run(source, deadline, remaining, || {
            match packet.read(&mut input) {
                Ok(()) => Ok(true),
                Err(av::Error::Eof) => Ok(false),
                Err(e) => Err(invalid(e.to_string())),
            }
        })?;
        if !result {
            return statistics.finish();
        }
        if packet.stream() != index {
            continue;
        }
        let pts = packet
            .pts()
            .ok_or_else(|| invalid("Video packet has no presentation timestamp"))?;
        let seconds = |n: i64| n as f64 * f64::from(tb.numerator()) / f64::from(tb.denominator());
        if !statistics.observe(seconds(pts), seconds(packet.duration()))? {
            return statistics.finish();
        }
    }
    Err(invalid("Packet probe count budget exceeded"))
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn out_of_order_prefix_finishes_presentation_interval_before_stopping() {
        let mut s = Statistics::default();
        for n in 0..100 {
            assert!(s.observe(n as f64, 1.).unwrap());
        }
        assert!(s.observe(99.5, 1.).unwrap());
        assert!(s.observe(100.25, 1.).unwrap());
        assert!(!s.observe(101.25, 1.).unwrap());
        let result = s.finish().unwrap();
        assert_eq!(result.packet_count, 102);
        assert!((result.average_packet_rate.unwrap() - 102. / 101.25).abs() < 1e-13);
    }
    #[test]
    fn unknown_duration_is_not_a_guessed_nominal_rate() {
        assert_eq!(
            Statistics::default().finish().unwrap().average_packet_rate,
            None
        );
        let mut s = Statistics::default();
        s.observe(7., 0.).unwrap();
        assert_eq!(s.finish().unwrap().average_packet_rate, None);
    }
}
