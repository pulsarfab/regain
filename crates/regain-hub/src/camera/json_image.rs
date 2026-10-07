//! Budgeted Alpaca JSON images. Field order is irrelevant; no pixel Value tree
//! is constructed. A shape pass precedes typed decoding into one allocation.
use super::image::{
    CameraImage, ElementType, IMAGE_CHUNK_BYTES, ImageAllocation, ImageBudget, ImageDescriptor,
    ImageOrder, ImageReadError, MAX_IMAGE_BYTES, StagedBytes,
};
use crate::source::{ErrorKind, SourceError};
use serde::{
    Deserialize,
    de::{self, DeserializeSeed, IgnoredAny, MapAccess, SeqAccess, Visitor},
};
use std::{
    fmt,
    io::{self, Read},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};
use tokio::{
    io::{AsyncRead, AsyncReadExt},
    sync::Semaphore,
};

// Bounds count raw wire bytes, including whitespace. Large arrays still have
// the existing 512-MiB pixel ceiling and compete with staging for the budget.
const MAX_JSON_BYTES: u64 = MAX_IMAGE_BYTES as u64 * 32;
const MAX_STRING_BYTES: usize = IMAGE_CHUNK_BYTES;
const MAX_NUMBER_BYTES: usize = 128;
const MAX_FIELDS: usize = 32;
static DECODERS: Semaphore = Semaphore::const_new(4);

fn invalid() -> SourceError {
    SourceError::new(ErrorKind::InvalidValue, "Invalid bounded JSON camera image")
}
fn cancelled(flag: &AtomicBool) -> Result<(), SourceError> {
    if flag.load(Ordering::Acquire) {
        Err(SourceError::new(
            ErrorKind::Disconnected,
            "Camera image reader was cancelled",
        ))
    } else {
        Ok(())
    }
}
struct CancelOnDrop {
    flag: Arc<AtomicBool>,
    worker: Option<tokio::task::AbortHandle>,
}
impl Drop for CancelOnDrop {
    fn drop(&mut self) {
        self.flag.store(true, Ordering::Release);
        // Queued blocking work can be aborted; already running work observes
        // the flag at bounded reader/pixel boundaries instead.
        if let Some(worker) = &self.worker {
            worker.abort();
        }
    }
}

#[derive(Default)]
struct Tokens {
    string: bool,
    escape: bool,
    length: usize,
}
impl Tokens {
    fn feed(&mut self, data: &[u8]) -> Result<(), SourceError> {
        for byte in data {
            if self.string {
                self.length += 1;
                if self.length > MAX_STRING_BYTES {
                    return Err(invalid());
                }
                if self.escape {
                    self.escape = false;
                } else if *byte == b'\\' {
                    self.escape = true;
                } else if *byte == b'"' {
                    self.string = false;
                    self.length = 0;
                }
            } else if *byte == b'"' {
                self.string = true;
                self.length = 0;
            } else if byte.is_ascii_whitespace()
                || matches!(byte, b'[' | b']' | b'{' | b'}' | b',' | b':')
            {
                self.length = 0;
            } else {
                self.length += 1;
                if self.length > MAX_NUMBER_BYTES {
                    return Err(invalid());
                }
            }
        }
        Ok(())
    }
}

/// Read one finite JSON response under the source actor's download deadline.
/// Four decoders bound fixed working overhead. Raw chunks and final pixels
/// both reserve shared budget before allocation. Cancellation also stops an
/// already-dispatched blocking parser; no new request/retry is made.
pub async fn read_json_image<R: AsyncRead + Unpin>(
    reader: &mut R,
    budget: &ImageBudget,
    transaction: u32,
) -> Result<CameraImage, ImageReadError> {
    let permit = DECODERS.acquire().await.map_err(|_| invalid())?;
    let flag = Arc::new(AtomicBool::new(false));
    let mut cancel = CancelOnDrop {
        flag: flag.clone(),
        worker: None,
    };
    let mut chunks = Vec::new();
    let mut tokens = Tokens::default();
    let mut total = 0u64;
    // Allocate fixed working memory only after decoder admission. Keeping a
    // 64-KiB array inside this future would charge every queued/binary caller.
    let mut buffer = Vec::new();
    buffer.try_reserve_exact(IMAGE_CHUNK_BYTES).map_err(|_| {
        SourceError::new(
            ErrorKind::Unavailable,
            "Camera JSON working allocation failed",
        )
    })?;
    buffer.resize(IMAGE_CHUNK_BYTES, 0);
    loop {
        let mut used = 0;
        let mut eof = false;
        while used < buffer.len() {
            let count = reader.read(&mut buffer[used..]).await?;
            if count == 0 {
                eof = true;
                break;
            }
            total = total
                .checked_add(count as u64)
                .filter(|n| *n <= MAX_JSON_BYTES)
                .ok_or_else(invalid)?;
            tokens.feed(&buffer[used..used + count])?;
            used += count;
        }
        if chunks.is_empty() && eof {
            // Ordinary small errors remain reportable even when readers pin the
            // whole image budget. This pass uses only bounded working memory.
            let mut decoder = serde_json::Deserializer::from_slice(&buffer[..used]);
            let envelope = EnvelopeSeed(&flag)
                .deserialize(&mut decoder)
                .map_err(|_| invalid())?;
            decoder.end().map_err(|_| invalid())?;
            envelope.descriptor(transaction)?;
        }
        if used != 0 {
            chunks.try_reserve(1).map_err(|_| {
                SourceError::new(
                    ErrorKind::Unavailable,
                    "Camera chunk list allocation failed",
                )
            })?;
            chunks.push(budget.stage(&buffer[..used])?);
        }
        if eof {
            break;
        }
        // AsyncRead can return immediately for every slice/chunk. Bound work
        // per poll even then, so cancellation and other source actors can run.
        tokio::task::yield_now().await;
    }
    drop(buffer);
    let budget = budget.clone();
    let worker = tokio::task::spawn_blocking(move || {
        let _permit = permit;
        decode(chunks, budget, transaction, &flag)
    });
    cancel.worker = Some(worker.abort_handle());
    worker
        .await
        .map_err(|_| SourceError::new(ErrorKind::Unavailable, "Camera JSON decoder stopped"))?
}

struct ChunkReader<'a> {
    chunks: &'a [StagedBytes],
    index: usize,
    offset: usize,
    flag: &'a AtomicBool,
}
impl Read for ChunkReader<'_> {
    fn read(&mut self, target: &mut [u8]) -> io::Result<usize> {
        if self.flag.load(Ordering::Acquire) {
            return Err(io::Error::new(
                io::ErrorKind::ConnectionAborted,
                "Image decoder cancelled",
            ));
        }
        if target.is_empty() {
            return Ok(0);
        }
        while self.index < self.chunks.len() {
            let remaining = &self.chunks[self.index].bytes()[self.offset..];
            if remaining.is_empty() {
                self.index += 1;
                self.offset = 0;
                continue;
            }
            let length = remaining.len().min(target.len());
            target[..length].copy_from_slice(&remaining[..length]);
            self.offset += length;
            return Ok(length);
        }
        Ok(0)
    }
}
fn decoder<'a>(
    chunks: &'a [StagedBytes],
    flag: &'a AtomicBool,
) -> serde_json::Deserializer<serde_json::de::IoRead<io::BufReader<ChunkReader<'a>>>> {
    serde_json::Deserializer::from_reader(io::BufReader::new(ChunkReader {
        chunks,
        index: 0,
        offset: 0,
        flag,
    }))
}
fn decode(
    chunks: Vec<StagedBytes>,
    budget: ImageBudget,
    transaction: u32,
    flag: &AtomicBool,
) -> Result<CameraImage, ImageReadError> {
    cancelled(flag)?;
    let mut first = decoder(&chunks, flag);
    let envelope = EnvelopeSeed(flag)
        .deserialize(&mut first)
        .map_err(|_| invalid())?;
    first.end().map_err(|_| invalid())?;
    let descriptor = envelope.descriptor(transaction)?;
    cancelled(flag)?;
    let mut image = budget.allocate(descriptor)?;
    let mut second = decoder(&chunks, flag);
    PixelsSeed {
        image: &mut image,
        descriptor,
        flag,
    }
    .deserialize(&mut second)
    .map_err(|_| invalid())?;
    second.end().map_err(|_| invalid())?;
    cancelled(flag)?;
    Ok(image.finish())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Shape {
    rank: usize,
    dimensions: [u32; 3],
    elements: usize,
}
struct ShapeSeed<'a> {
    depth: usize,
    flag: &'a AtomicBool,
}
impl<'de> DeserializeSeed<'de> for ShapeSeed<'_> {
    type Value = Shape;
    fn deserialize<D: de::Deserializer<'de>>(self, deserializer: D) -> Result<Shape, D::Error> {
        cancelled(self.flag).map_err(de::Error::custom)?;
        deserializer.deserialize_any(self)
    }
}
impl<'de> Visitor<'de> for ShapeSeed<'_> {
    type Value = Shape;
    fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str("a rectangular numeric camera image")
    }
    fn visit_unit<E: de::Error>(self) -> Result<Shape, E> {
        if self.depth != 0 {
            return Err(E::custom("Null camera pixel"));
        }
        Ok(Shape {
            rank: 0,
            dimensions: [0; 3],
            elements: 0,
        })
    }
    fn visit_i64<E: de::Error>(self, _: i64) -> Result<Shape, E> {
        Ok(Shape {
            rank: 0,
            dimensions: [0; 3],
            elements: 1,
        })
    }
    fn visit_u64<E: de::Error>(self, _: u64) -> Result<Shape, E> {
        Ok(Shape {
            rank: 0,
            dimensions: [0; 3],
            elements: 1,
        })
    }
    fn visit_f64<E: de::Error>(self, value: f64) -> Result<Shape, E> {
        if !value.is_finite() {
            return Err(E::custom("Nonfinite camera pixel"));
        }
        Ok(Shape {
            rank: 0,
            dimensions: [0; 3],
            elements: 1,
        })
    }
    fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Shape, A::Error> {
        if self.depth >= 3 {
            return Err(de::Error::custom("Camera array rank exceeds three"));
        }
        let mut count = 0u32;
        let mut child = None;
        let mut elements = 0usize;
        while let Some(shape) = seq.next_element_seed(ShapeSeed {
            depth: self.depth + 1,
            flag: self.flag,
        })? {
            if child.is_some_and(|prior| prior != shape) {
                return Err(de::Error::custom("Ragged camera image"));
            }
            child = Some(shape);
            count = count
                .checked_add(1)
                .filter(|n| *n <= i32::MAX as u32)
                .ok_or_else(|| de::Error::custom("Camera dimension exceeds Int32"))?;
            elements = elements
                .checked_add(shape.elements)
                .filter(|n| *n <= MAX_IMAGE_BYTES)
                .ok_or_else(|| de::Error::custom("Camera image element bound exceeded"))?;
        }
        let child = child.ok_or_else(|| de::Error::custom("Empty camera image dimension"))?;
        if child.elements == 0 {
            return Err(de::Error::custom("Empty camera image"));
        }
        Ok(Shape {
            rank: child.rank + 1,
            dimensions: [count, child.dimensions[0], child.dimensions[1]],
            elements,
        })
    }
}

#[derive(Deserialize)]
#[serde(field_identifier)]
enum Field {
    Value,
    Type,
    Rank,
    ErrorNumber,
    ErrorMessage,
    ClientTransactionID,
    ServerTransactionID,
    #[serde(other)]
    Other,
}
#[derive(Default)]
struct Envelope {
    value: Option<Shape>,
    element_type: Option<u32>,
    rank: Option<u32>,
    number: Option<i32>,
    message: Option<String>,
    client: Option<u32>,
    server: Option<u32>,
}
impl Envelope {
    fn descriptor(&self, transaction: u32) -> Result<ImageDescriptor, ImageReadError> {
        if self.client.is_some_and(|id| id != transaction) || self.number.is_some_and(|n| n < 0) {
            return Err(invalid().into());
        }
        let number = self.number.unwrap_or(0);
        if number != 0 {
            return Err(ImageReadError::Upstream {
                code: number as u32,
                message: self.message.clone().unwrap_or_default(),
            });
        }
        if self.message.as_ref().is_some_and(|m| !m.is_empty()) {
            return Err(invalid().into());
        }
        let shape = self.value.ok_or_else(invalid)?;
        if !matches!(shape.rank, 2 | 3) || self.rank != Some(shape.rank as u32) {
            return Err(invalid().into());
        }
        let kind = ElementType::from_id(self.element_type.ok_or_else(invalid)?)?;
        Ok(ImageDescriptor::new(
            shape.dimensions[0],
            shape.dimensions[1],
            (shape.rank == 3).then_some(shape.dimensions[2]),
            kind,
            kind,
            ImageOrder::Ascom,
        )?)
    }
}
struct EnvelopeSeed<'a>(&'a AtomicBool);
impl<'de> DeserializeSeed<'de> for EnvelopeSeed<'_> {
    type Value = Envelope;
    fn deserialize<D: de::Deserializer<'de>>(self, d: D) -> Result<Envelope, D::Error> {
        d.deserialize_map(self)
    }
}
fn store<T, E: de::Error>(slot: &mut Option<T>, value: T) -> Result<(), E> {
    if slot.replace(value).is_some() {
        Err(E::custom("Duplicate camera response field"))
    } else {
        Ok(())
    }
}
impl<'de> Visitor<'de> for EnvelopeSeed<'_> {
    type Value = Envelope;
    fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str("an Alpaca camera response")
    }
    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Envelope, A::Error> {
        let mut result = Envelope::default();
        let mut fields = 0;
        while let Some(field) = map.next_key::<Field>()? {
            cancelled(self.0).map_err(de::Error::custom)?;
            fields += 1;
            if fields > MAX_FIELDS {
                return Err(de::Error::custom("Too many camera response fields"));
            }
            match field {
                Field::Value => store(
                    &mut result.value,
                    map.next_value_seed(ShapeSeed {
                        depth: 0,
                        flag: self.0,
                    })?,
                )?,
                Field::Type => store(&mut result.element_type, map.next_value()?)?,
                Field::Rank => store(&mut result.rank, map.next_value()?)?,
                Field::ErrorNumber => store(&mut result.number, map.next_value()?)?,
                Field::ErrorMessage => {
                    let message: String = map.next_value()?;
                    if message.len() > 4096 {
                        return Err(de::Error::custom("Camera error message exceeds bound"));
                    }
                    store(&mut result.message, message)?;
                }
                Field::ClientTransactionID => store(&mut result.client, map.next_value()?)?,
                Field::ServerTransactionID => store(&mut result.server, map.next_value()?)?,
                Field::Other => {
                    map.next_value::<IgnoredAny>()?;
                }
            }
        }
        Ok(result)
    }
}

struct PixelsSeed<'a> {
    image: &'a mut ImageAllocation,
    descriptor: ImageDescriptor,
    flag: &'a AtomicBool,
}
impl<'de> DeserializeSeed<'de> for PixelsSeed<'_> {
    type Value = ();
    fn deserialize<D: de::Deserializer<'de>>(self, d: D) -> Result<(), D::Error> {
        d.deserialize_map(self)
    }
}
impl<'de> Visitor<'de> for PixelsSeed<'_> {
    type Value = ();
    fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str("typed camera pixels")
    }
    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<(), A::Error> {
        let mut offset = 0;
        let mut found = false;
        let dimensions = [
            self.descriptor.width(),
            self.descriptor.height(),
            self.descriptor.planes().unwrap_or(0),
        ];
        let rank = if self.descriptor.planes().is_some() {
            3
        } else {
            2
        };
        while let Some(field) = map.next_key::<Field>()? {
            if matches!(field, Field::Value) {
                if found {
                    return Err(de::Error::custom("Duplicate camera pixels"));
                }
                found = true;
                map.next_value_seed(ArraySeed {
                    bytes: self.image.bytes_mut(),
                    offset: &mut offset,
                    dimensions: &dimensions[..rank],
                    kind: self.descriptor.element_type(),
                    flag: self.flag,
                })?;
            } else {
                map.next_value::<IgnoredAny>()?;
            }
        }
        if !found || offset != self.image.bytes_mut().len() {
            return Err(de::Error::custom("Incomplete camera pixels"));
        }
        Ok(())
    }
}
struct ArraySeed<'a> {
    bytes: &'a mut [u8],
    offset: &'a mut usize,
    dimensions: &'a [u32],
    kind: ElementType,
    flag: &'a AtomicBool,
}
impl<'de> DeserializeSeed<'de> for ArraySeed<'_> {
    type Value = ();
    fn deserialize<D: de::Deserializer<'de>>(self, d: D) -> Result<(), D::Error> {
        cancelled(self.flag).map_err(de::Error::custom)?;
        if !self.dimensions.is_empty() {
            return d.deserialize_seq(self);
        }
        let end = self
            .offset
            .checked_add(self.kind.bytes())
            .filter(|end| *end <= self.bytes.len())
            .ok_or_else(|| de::Error::custom("Camera pixel length overflow"))?;
        let target = &mut self.bytes[*self.offset..end];
        macro_rules! integer {
            ($ty:ty) => {
                target.copy_from_slice(&<$ty>::deserialize(d)?.to_le_bytes())
            };
        }
        match self.kind {
            ElementType::Int16 => integer!(i16),
            ElementType::Int32 => integer!(i32),
            ElementType::Int64 => integer!(i64),
            ElementType::UInt16 => integer!(u16),
            ElementType::UInt32 => integer!(u32),
            ElementType::UInt64 => integer!(u64),
            ElementType::Byte => integer!(u8),
            ElementType::Single => {
                let value = f32::deserialize(d)?;
                if !value.is_finite() {
                    return Err(de::Error::custom("Nonfinite Single camera pixel"));
                }
                target.copy_from_slice(&value.to_le_bytes());
            }
            ElementType::Double => {
                let value = f64::deserialize(d)?;
                if !value.is_finite() {
                    return Err(de::Error::custom("Nonfinite Double camera pixel"));
                }
                target.copy_from_slice(&value.to_le_bytes());
            }
        }
        *self.offset = end;
        Ok(())
    }
}
impl<'de> Visitor<'de> for ArraySeed<'_> {
    type Value = ();
    fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str("a fixed camera dimension")
    }
    fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<(), A::Error> {
        for _ in 0..self.dimensions[0] {
            if seq
                .next_element_seed(ArraySeed {
                    bytes: self.bytes,
                    offset: self.offset,
                    dimensions: &self.dimensions[1..],
                    kind: self.kind,
                    flag: self.flag,
                })?
                .is_none()
            {
                return Err(de::Error::custom("Short camera dimension"));
            }
        }
        if seq.next_element::<IgnoredAny>()?.is_some() {
            return Err(de::Error::custom("Long camera dimension"));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn cancellation_is_a_terminal_io_error_not_a_retried_interruption() {
        // std::io::Bytes retries Interrupted. A cancellation error of that kind
        // would keep serde's reader spinning forever after the caller left.
        let (send, receive) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let flag = AtomicBool::new(true);
            let mut input = decoder(&[], &flag);
            let stopped = IgnoredAny::deserialize(&mut input).is_err();
            let _ = send.send(stopped);
        });
        assert!(
            receive
                .recv_timeout(Duration::from_secs(5))
                .expect("Cancelled reader did not terminate")
        );
    }

    #[test]
    fn abandoned_queued_blocking_decode_releases_raw_memory_before_any_pixels() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .max_blocking_threads(1)
            .build()
            .unwrap();
        runtime.block_on(async {
            let (release, held) = std::sync::mpsc::channel();
            let (started, ready) = tokio::sync::oneshot::channel();
            let gate = tokio::task::spawn_blocking(move || {
                let _ = started.send(());
                held.recv_timeout(Duration::from_secs(5)).unwrap();
            });
            ready.await.unwrap();
            let body = br#"{"Value":[[1,2],[3,4]],"Type":2,"Rank":2}"#.to_vec();
            let raw_length = body.len();
            let budget = ImageBudget::new(raw_length + 16).unwrap();
            let capture_budget = budget.clone();
            let capture = tokio::spawn(async move {
                read_json_image(&mut body.as_slice(), &capture_budget, 17).await
            });
            tokio::time::timeout(Duration::from_secs(5), async {
                while budget.used_bytes() != raw_length {
                    tokio::task::yield_now().await;
                }
            })
            .await
            .unwrap();
            // The only blocking thread is held: input has been staged and work
            // is dispatched, but no image allocation can have happened yet.
            capture.abort();
            assert!(matches!(capture.await, Err(error) if error.is_cancelled()));
            assert_eq!(budget.used_bytes(), raw_length);
            release.send(()).unwrap();
            gate.await.unwrap();
            tokio::time::timeout(Duration::from_secs(5), async {
                while budget.used_bytes() != 0 {
                    tokio::task::yield_now().await;
                }
            })
            .await
            .unwrap();
        });
    }
}
