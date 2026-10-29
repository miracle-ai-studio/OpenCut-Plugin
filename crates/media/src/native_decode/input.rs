//! A bounded AVIO bridge. The demuxer persists, but each synchronous operation
//! borrows the current authorized resource port; it never retains that grant.
use super::*;
use ffmpeg_next::{self as av, ffi};
use std::{
    ffi::{c_char, c_int, c_void},
    io::{self, Read, Seek, SeekFrom},
    panic::{AssertUnwindSafe, catch_unwind},
    sync::{Arc, Mutex},
    thread::{self, ThreadId},
};

/// Match the retained demuxers' primary-track ordering. ISO BMFF and Matroska
/// stably put default tracks first; other containers preserve stored order.
/// This is deliberately not libav's codec/resolution-ranked "best" stream.
/// Explicit indices remain physical stream indices and are never re-ranked.
pub(crate) fn select_stream(
    input: &av::format::context::Input,
    medium: av::media::Type,
    selected: Option<u32>,
) -> Option<av::Stream<'_>> {
    if let Some(index) = selected {
        return input.stream(index as usize);
    }
    let prefer_default = input
        .format()
        .name()
        .split(',')
        .any(|name| matches!(name, "mov" | "matroska" | "webm"));
    let mut streams = input
        .streams()
        .filter(|stream| stream.parameters().medium() == medium);
    let first = streams.next()?;
    if !prefer_default
        || first
            .disposition()
            .contains(av::format::stream::Disposition::DEFAULT)
    {
        return Some(first);
    }
    streams
        .find(|stream| {
            stream
                .disposition()
                .contains(av::format::stream::Disposition::DEFAULT)
        })
        .or(Some(first))
}

struct Borrowed<'a> {
    source: &'a mut dyn ByteSource,
}
struct Scope {
    borrowed: *mut c_void,
    thread: ThreadId,
}
// SAFETY: a scope is dereferenced only under Shared's mutex and on `thread`.
// Foreign-thread callbacks fail before dereferencing it. Bridge::run clears it
// on success, error and unwinding, before its stack borrow can expire.
unsafe impl Send for Scope {}
struct Shared {
    scope: Option<Scope>,
    error: Option<MediaError>,
    deadline: Instant,
    remaining: u64,
    size: u64,
    bytes_read: u64,
}
pub(crate) struct Bridge {
    shared: Arc<Mutex<Shared>>,
}
struct Reader {
    shared: Arc<Mutex<Shared>>,
    position: u64,
}
struct Reset(Arc<Mutex<Shared>>);
impl Drop for Reset {
    fn drop(&mut self) {
        self.0.lock().unwrap_or_else(|e| e.into_inner()).scope = None;
    }
}

impl Shared {
    fn source(&mut self) -> MediaResult<&mut dyn ByteSource> {
        let scope = self
            .scope
            .as_ref()
            .ok_or_else(|| invalid("Native input read outside its execution"))?;
        if scope.thread != thread::current().id() {
            return Err(invalid("Native input callback changed thread"));
        }
        // SAFETY: Bridge::run pins Borrowed on its stack for the entire
        // synchronous call. Reset clears this pointer before that call returns.
        let borrowed = unsafe { &mut *scope.borrowed.cast::<Borrowed<'_>>() };
        Ok(&mut *borrowed.source)
    }
    fn check(&mut self) -> MediaResult<()> {
        if self.error.is_some() {
            return Err(invalid("Native input operation already failed"));
        }
        let deadline = self.deadline;
        checkpoint(self.source()?, deadline)
    }
    fn remember<T>(&mut self, result: MediaResult<T>) -> io::Result<T> {
        result.map_err(|error| {
            if self.error.is_none() {
                self.error = Some(error);
            }
            io::Error::other("Authorized native input operation failed")
        })
    }
}
impl Bridge {
    pub(crate) fn new(size: u64) -> MediaResult<Self> {
        if size == 0 || size > i64::MAX as u64 {
            return Err(invalid("Invalid native source length"));
        }
        Ok(Self {
            shared: Arc::new(Mutex::new(Shared {
                scope: None,
                error: None,
                deadline: Instant::now(),
                remaining: 0,
                size,
                bytes_read: 0,
            })),
        })
    }
    pub(crate) fn bytes_read(&self) -> u64 {
        self.shared
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .bytes_read
    }
    pub(crate) fn run<T>(
        &mut self,
        source: &mut dyn ByteSource,
        deadline: Instant,
        max_io: u64,
        operation: impl FnOnce() -> MediaResult<T>,
    ) -> MediaResult<T> {
        checkpoint(source, deadline)?;
        let mut borrowed = Borrowed { source };
        {
            let mut shared = self.shared.lock().unwrap_or_else(|e| e.into_inner());
            if shared.scope.is_some() || borrowed.source.len() != shared.size {
                return Err(invalid("Native source scope or immutable size changed"));
            }
            shared.scope = Some(Scope {
                borrowed: (&mut borrowed as *mut Borrowed<'_>).cast(),
                thread: thread::current().id(),
            });
            shared.deadline = deadline;
            shared.remaining = max_io;
            shared.error = None;
        }
        let reset = Reset(self.shared.clone());
        let result = operation();
        let error = self
            .shared
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .error
            .take();
        drop(reset);
        if let Some(error) = error {
            return Err(error);
        }
        checkpoint(borrowed.source, deadline)?;
        result
    }

    pub(crate) fn open(
        &mut self,
        source: &mut dyn ByteSource,
        max_io_bytes: u64,
        deadline: Instant,
    ) -> MediaResult<av::format::context::Input> {
        let mut io = av::format::context::StreamIo::from_read_seek_with_capacity(
            Reader {
                shared: self.shared.clone(),
                position: 0,
            },
            65536,
        )
        .map_err(failed)?;
        let opaque = Arc::as_ptr(&self.shared).cast_mut().cast::<c_void>();
        self.run(source, deadline, max_io_bytes, || {
            // SAFETY: AVFormatContext is uniquely owned here. Custom IO owns
            // its Reader/Arc through Input destruction. No filename is passed,
            // and nested protocol opens are denied before probing any bytes.
            unsafe {
                let mut context = ffi::avformat_alloc_context();
                if context.is_null() { return Err(invalid("Could not allocate native input")); }
                (*context).pb = io.as_mut_ptr();
                (*context).flags |= ffi::AVFMT_FLAG_CUSTOM_IO;
                (*context).io_open = Some(deny_open);
                (*context).interrupt_callback = ffi::AVIOInterruptCB { callback: Some(interrupt), opaque };
                (*context).probesize = max_io_bytes.min(4 * 1024 * 1024) as i64;
                (*context).max_analyze_duration = 5_000_000;
                (*context).max_streams = 64;
                let mut options = av::Dictionary::new();
                options.set("protocol_whitelist", "");
                options.set("format_whitelist", "mov,matroska,avi,mpeg,mpegts,png_pipe,jpeg_pipe,webp_pipe,bmp_pipe,gif,mp3,wav,flac,ogg,aac");
                options.set("enable_drefs", "0");
                options.set("use_absolute_path", "0");
                let mut raw_options = options.disown();
                let opened = ffi::avformat_open_input(&mut context, std::ptr::null(), std::ptr::null(), &mut raw_options);
                av::Dictionary::own(raw_options);
                if opened < 0 {
                    if !context.is_null() { ffi::avformat_close_input(&mut context); }
                    return Err(failed(av::Error::from(opened)));
                }
                // Do not run find_stream_info's implicit decoder probe here:
                // allocation limits must be set before any decoded surface.
                // The selected stream is opened explicitly by our decoder.
                Ok(av::format::context::Input::wrap_with_custom_io(context, io))
            }
        })
    }
}

impl Read for Reader {
    fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
        let mut shared = self.shared.lock().unwrap_or_else(|e| e.into_inner());
        let result = catch_unwind(AssertUnwindSafe(|| {
            shared.check()?;
            if output.is_empty() || self.position == shared.size {
                return Ok(0);
            }
            if shared.remaining == 0 {
                return Err(MediaError::OutputLimit {
                    limit_bytes: shared.bytes_read,
                });
            }
            let count = output
                .len()
                .min(65536)
                .min((shared.size - self.position) as usize)
                .min(shared.remaining as usize);
            let read = shared
                .source()?
                .read_at(self.position, &mut output[..count])?;
            if read == 0 || read > count {
                return Err(invalid("Native source returned an invalid byte range"));
            }
            shared.check()?;
            self.position += read as u64;
            shared.remaining -= read as u64;
            shared.bytes_read = shared.bytes_read.saturating_add(read as u64);
            Ok(read)
        }))
        .unwrap_or_else(|_| Err(invalid("Native source callback panicked")));
        shared.remember(result)
    }
}
impl Seek for Reader {
    fn seek(&mut self, position: SeekFrom) -> io::Result<u64> {
        let mut shared = self.shared.lock().unwrap_or_else(|e| e.into_inner());
        let result = catch_unwind(AssertUnwindSafe(|| {
            shared.check()?;
            let next = match position {
                SeekFrom::Start(value) => i128::from(value),
                SeekFrom::Current(value) => i128::from(self.position) + i128::from(value),
                SeekFrom::End(value) => i128::from(shared.size) + i128::from(value),
            };
            if next < 0 || next > i128::from(shared.size) {
                return Err(invalid("Native source seek is outside immutable bytes"));
            }
            self.position = next as u64;
            Ok(self.position)
        }))
        .unwrap_or_else(|_| Err(invalid("Native source seek callback panicked")));
        shared.remember(result)
    }
}
unsafe extern "C" fn deny_open(
    _: *mut ffi::AVFormatContext,
    _: *mut *mut ffi::AVIOContext,
    _: *const c_char,
    _: c_int,
    _: *mut *mut ffi::AVDictionary,
) -> c_int {
    ffi::AVERROR(av::error::EACCES)
}
unsafe extern "C" fn interrupt(opaque: *mut c_void) -> c_int {
    catch_unwind(AssertUnwindSafe(|| {
        // SAFETY: the format context's custom Reader retains this Arc. It is
        // also retained by Bridge, through every live input operation.
        let state = unsafe { &*opaque.cast::<Mutex<Shared>>() };
        let mut shared = state.lock().unwrap_or_else(|e| e.into_inner());
        let result = shared.check();
        i32::from(shared.remember(result).is_err())
    }))
    .unwrap_or(1)
}
