//! Lossless ImageBytes transport, independent of the source's recovery policy.
//!
//! Ordering and numeric IDs follow the ASCOM Alpaca API reference, section 8.
//! A native frame keeps its X-fast storage; exports transpose in bounded chunks.
use crate::source::{ErrorKind, SourceError};
use serde::{Deserialize, Serialize};
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
use tokio::io::{AsyncRead, AsyncReadExt};

pub const MAX_IMAGE_BYTES: usize = 512 * 1024 * 1024;
pub const IMAGE_CHUNK_BYTES: usize = 64 * 1024;
pub const IMAGEBYTES_HEADER_BYTES: usize = 44;
/// Retained encoded native metadata; overflow fails rather than losing recovery evidence.
pub const NATIVE_METADATA_BYTES: usize = 128 * 1024;
const NATIVE_WORKER_HEADER_BYTES: usize = 64 * 1024;
const MAX_ERROR_BYTES: usize = 4096;

fn invalid(message: &'static str) -> SourceError {
    SourceError::new(ErrorKind::InvalidValue, message)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[repr(u32)]
pub enum ElementType {
    Int16 = 1,
    Int32 = 2,
    Double = 3,
    Single = 4,
    UInt64 = 5,
    Byte = 6,
    Int64 = 7,
    UInt16 = 8,
    UInt32 = 9,
}
impl ElementType {
    pub fn bytes(self) -> usize {
        match self {
            Self::Byte => 1,
            Self::Int16 | Self::UInt16 => 2,
            Self::Int32 | Self::UInt32 | Self::Single => 4,
            Self::Int64 | Self::UInt64 | Self::Double => 8,
        }
    }
    pub(super) fn from_id(value: u32) -> Result<Self, SourceError> {
        match value {
            1 => Ok(Self::Int16),
            2 => Ok(Self::Int32),
            3 => Ok(Self::Double),
            4 => Ok(Self::Single),
            5 => Ok(Self::UInt64),
            6 => Ok(Self::Byte),
            7 => Ok(Self::Int64),
            8 => Ok(Self::UInt16),
            9 => Ok(Self::UInt32),
            _ => Err(SourceError::new(
                ErrorKind::Unsupported,
                "Unknown camera image element type",
            )),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ImageOrder {
    /// ASCOM Array[X,Y,plane]: plane fastest, then Y, then X.
    Ascom,
    /// Native sensor rows: plane fastest, then X, then Y.
    SensorRows,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct DescriptorWire {
    width: u32,
    height: u32,
    /// None is rank two; Some(1) is still rank three.
    planes: Option<u32>,
    element_type: ElementType,
    transmission_type: ElementType,
    order: ImageOrder,
}

/// Validated geometry and encoding. Pixel bytes are always little endian.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "DescriptorWire", into = "DescriptorWire")]
pub struct ImageDescriptor(DescriptorWire);
impl TryFrom<DescriptorWire> for ImageDescriptor {
    type Error = SourceError;
    fn try_from(wire: DescriptorWire) -> Result<Self, Self::Error> {
        for dimension in [wire.width, wire.height, wire.planes.unwrap_or(1)] {
            if dimension == 0 || dimension > i32::MAX as u32 {
                return Err(invalid(
                    "Camera image dimensions must be positive Int32 values",
                ));
            }
        }
        let logical = wire.element_type;
        let encoded = wire.transmission_type;
        if logical != encoded
            && !(logical == ElementType::Int32
                && matches!(
                    encoded,
                    ElementType::Byte | ElementType::Int16 | ElementType::UInt16
                ))
        {
            return Err(SourceError::new(
                ErrorKind::Unsupported,
                "Camera image element conversion is not losslessly supported",
            ));
        }
        let bytes = u64::from(wire.width)
            .checked_mul(u64::from(wire.height))
            .and_then(|v| v.checked_mul(u64::from(wire.planes.unwrap_or(1))))
            .and_then(|v| v.checked_mul(encoded.bytes() as u64));
        if bytes.is_none_or(|v| v > MAX_IMAGE_BYTES as u64) {
            return Err(invalid("Camera image exceeds the bounded image size"));
        }
        Ok(Self(wire))
    }
}
impl From<ImageDescriptor> for DescriptorWire {
    fn from(value: ImageDescriptor) -> Self {
        value.0
    }
}
impl ImageDescriptor {
    pub fn new(
        width: u32,
        height: u32,
        planes: Option<u32>,
        element_type: ElementType,
        transmission_type: ElementType,
        order: ImageOrder,
    ) -> Result<Self, SourceError> {
        DescriptorWire {
            width,
            height,
            planes,
            element_type,
            transmission_type,
            order,
        }
        .try_into()
    }
    pub fn width(self) -> u32 {
        self.0.width
    }
    pub fn height(self) -> u32 {
        self.0.height
    }
    pub fn planes(self) -> Option<u32> {
        self.0.planes
    }
    pub fn element_type(self) -> ElementType {
        self.0.element_type
    }
    pub fn transmission_type(self) -> ElementType {
        self.0.transmission_type
    }
    pub fn order(self) -> ImageOrder {
        self.0.order
    }
    pub fn byte_len(self) -> usize {
        self.0.width as usize
            * self.0.height as usize
            * self.0.planes.unwrap_or(1) as usize
            * self.0.transmission_type.bytes()
    }
    pub fn imagebytes_header(self, client: u32, server: u32) -> [u8; IMAGEBYTES_HEADER_BYTES] {
        let fields = [
            1,
            0,
            client,
            server,
            IMAGEBYTES_HEADER_BYTES as u32,
            self.0.element_type as u32,
            self.0.transmission_type as u32,
            if self.0.planes.is_some() { 3 } else { 2 },
            self.0.width,
            self.0.height,
            self.0.planes.unwrap_or(0),
        ];
        let mut header = [0; IMAGEBYTES_HEADER_BYTES];
        for (bytes, field) in header.as_chunks_mut::<4>().0.iter_mut().zip(fields) {
            bytes.copy_from_slice(&field.to_le_bytes());
        }
        header
    }
    /// Byte offset in source storage for an element in ASCOM serial order.
    fn ascom_offset(self, element: usize) -> usize {
        if self.0.order == ImageOrder::Ascom {
            return element * self.0.transmission_type.bytes();
        }
        let planes = self.0.planes.unwrap_or(1) as usize;
        let plane = element % planes;
        let pixel = element / planes;
        let x = pixel / self.0.height as usize;
        let y = pixel % self.0.height as usize;
        ((y * self.0.width as usize + x) * planes + plane) * self.0.transmission_type.bytes()
    }
}

struct BudgetInner {
    maximum: usize,
    used: AtomicUsize,
}
/// One shared budget must be retained by the host, including disconnected readers.
#[derive(Clone)]
pub struct ImageBudget(Arc<BudgetInner>);
impl ImageBudget {
    pub(crate) fn shares(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
    pub fn new(maximum: usize) -> Result<Self, SourceError> {
        if maximum == 0 {
            return Err(invalid("Camera image budget must be positive"));
        }
        Ok(Self(Arc::new(BudgetInner {
            maximum,
            used: AtomicUsize::new(0),
        })))
    }
    pub fn used_bytes(&self) -> usize {
        self.0.used.load(Ordering::Acquire)
    }
    fn reserve(&self, bytes: usize) -> Result<Reservation, SourceError> {
        // fetch_update was renamed in newer Rust; an explicit CAS loop retains
        // the 1.89 MSRV without relying on deprecated APIs or suppressing lints.
        let mut used = self.0.used.load(Ordering::Acquire);
        loop {
            let next = used
                .checked_add(bytes)
                .filter(|v| *v <= self.0.maximum)
                .ok_or_else(|| {
                    SourceError::new(
                        ErrorKind::Busy,
                        "Camera image memory is retained by active captures or readers",
                    )
                })?;
            match self
                .0
                .used
                .compare_exchange_weak(used, next, Ordering::AcqRel, Ordering::Acquire)
            {
                Ok(_) => break,
                Err(current) => used = current,
            }
        }
        Ok(Reservation {
            budget: self.clone(),
            bytes,
        })
    }
    pub fn allocate(&self, descriptor: ImageDescriptor) -> Result<ImageAllocation, SourceError> {
        let reservation = self.reserve(descriptor.byte_len())?;
        let mut bytes = Vec::new();
        bytes
            .try_reserve_exact(descriptor.byte_len())
            .map_err(|_| {
                SourceError::new(ErrorKind::Unavailable, "Camera image allocation failed")
            })?;
        bytes.resize(descriptor.byte_len(), 0);
        Ok(ImageAllocation {
            descriptor,
            bytes,
            reservation,
        })
    }
    /// A bounded transport chunk retained across decoding. Raw JSON staging
    /// and final pixels compete for the same budget; no unbounded Value tree.
    pub(super) fn stage(&self, data: &[u8]) -> Result<StagedBytes, SourceError> {
        if data.len() > IMAGE_CHUNK_BYTES {
            return Err(invalid("Camera transport chunk exceeds its bound"));
        }
        let reservation = self.reserve(data.len())?;
        let mut bytes = Vec::new();
        bytes.try_reserve_exact(data.len()).map_err(|_| {
            SourceError::new(ErrorKind::Unavailable, "Camera transport allocation failed")
        })?;
        bytes.extend_from_slice(data);
        Ok(StagedBytes {
            bytes,
            _reservation: reservation,
        })
    }
    /// Admit before starting a native capture. Covers retained pixels/metadata,
    /// the worker's pixel Vec and its bounded response header. This payload budget
    /// does not claim to account for worker processes or decoded JSON tree overhead.
    pub fn reserve_native(
        &self,
        exposure: &regain_core::Exposure,
    ) -> Result<NativeFramePermit, SourceError> {
        let descriptor = ImageDescriptor::new(
            exposure.width,
            exposure.height,
            None,
            ElementType::Int32,
            ElementType::UInt16,
            ImageOrder::SensorRows,
        )?;
        let reservation = self.reserve(
            descriptor.byte_len() * 2 + NATIVE_METADATA_BYTES + NATIVE_WORKER_HEADER_BYTES,
        )?;
        Ok(NativeFramePermit {
            exposure: exposure.clone(),
            descriptor,
            reservation,
        })
    }
}

/// A single native capture admission. Dropping it releases staging and output space.
pub struct NativeFramePermit {
    exposure: regain_core::Exposure,
    descriptor: ImageDescriptor,
    reservation: Reservation,
}
impl NativeFramePermit {
    /// Adopt the core's Arc without another pixel copy, retaining all metadata.
    pub fn adopt(mut self, frame: regain_core::Frame) -> Result<CameraImage, SourceError> {
        if frame.exposure != self.exposure || frame.pixels.len() != self.descriptor.byte_len() {
            return Err(invalid(
                "Native camera frame differs from its admitted exposure",
            ));
        }
        let mut metadata = BoundedMetadata(Vec::new());
        metadata
            .0
            .try_reserve_exact(NATIVE_METADATA_BYTES)
            .map_err(|_| {
                SourceError::new(
                    ErrorKind::Unavailable,
                    "Native camera metadata allocation failed",
                )
            })?;
        serde_json::to_writer(&mut metadata, &frame.metadata)
            .map_err(|_| invalid("Native camera metadata exceeds its encoded bound"))?;
        drop(frame.metadata);
        self.reservation
            .shrink_to(self.descriptor.byte_len() + NATIVE_METADATA_BYTES);
        Ok(CameraImage(Arc::new(ImageInner {
            descriptor: self.descriptor,
            bytes: Pixels::Shared(frame.pixels),
            native: Some(NativeImageInfo {
                exposure: frame.exposure,
                metadata: metadata.0,
            }),
            _reservation: self.reservation,
        })))
    }
}
struct BoundedMetadata(Vec<u8>);
impl std::io::Write for BoundedMetadata {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        if bytes.len() > NATIVE_METADATA_BYTES - self.0.len() {
            return Err(std::io::Error::other("Native metadata too large"));
        }
        self.0.extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}
pub struct NativeImageInfo {
    exposure: regain_core::Exposure,
    metadata: Vec<u8>,
}
impl NativeImageInfo {
    pub fn exposure(&self) -> &regain_core::Exposure {
        &self.exposure
    }
    /// Immutable UTF-8 JSON, including native timing, recovery, cooling and WB fields.
    pub fn metadata_json(&self) -> &[u8] {
        &self.metadata
    }
}
pub(super) struct StagedBytes {
    bytes: Vec<u8>,
    _reservation: Reservation,
}
impl StagedBytes {
    pub(super) fn bytes(&self) -> &[u8] {
        &self.bytes
    }
}
struct Reservation {
    budget: ImageBudget,
    bytes: usize,
}
impl Reservation {
    fn shrink_to(&mut self, bytes: usize) {
        assert!(bytes <= self.bytes);
        self.budget
            .0
            .used
            .fetch_sub(self.bytes - bytes, Ordering::AcqRel);
        self.bytes = bytes;
    }
}
impl Drop for Reservation {
    fn drop(&mut self) {
        self.budget.0.used.fetch_sub(self.bytes, Ordering::AcqRel);
    }
}
pub struct ImageAllocation {
    descriptor: ImageDescriptor,
    bytes: Vec<u8>,
    reservation: Reservation,
}
impl ImageAllocation {
    pub fn bytes_mut(&mut self) -> &mut [u8] {
        &mut self.bytes
    }
    pub fn finish(self) -> CameraImage {
        CameraImage(Arc::new(ImageInner {
            descriptor: self.descriptor,
            bytes: Pixels::Owned(self.bytes),
            native: None,
            _reservation: self.reservation,
        }))
    }
}
enum Pixels {
    Owned(Vec<u8>),
    Shared(Arc<[u8]>),
}
impl Pixels {
    fn as_slice(&self) -> &[u8] {
        match self {
            Self::Owned(v) => v,
            Self::Shared(v) => v,
        }
    }
}
struct ImageInner {
    descriptor: ImageDescriptor,
    bytes: Pixels,
    native: Option<NativeImageInfo>,
    _reservation: Reservation,
}
/// Clones pin the same immutable pixels and budget reservation, never another copy.
#[derive(Clone)]
pub struct CameraImage(Arc<ImageInner>);
impl CameraImage {
    pub fn descriptor(&self) -> ImageDescriptor {
        self.0.descriptor
    }
    pub fn bytes(&self) -> &[u8] {
        self.0.bytes.as_slice()
    }
    pub fn native(&self) -> Option<&NativeImageInfo> {
        self.0.native.as_ref()
    }
    /// One reusable, accounted scratch buffer per outgoing stream. Admission
    /// precedes allocation and the image's existing reservation is not copied.
    pub(crate) fn transfer_buffer(&self) -> Result<TransferBuffer, SourceError> {
        let length = self.bytes().len().min(IMAGE_CHUNK_BYTES);
        let reservation = self.0._reservation.budget.reserve(length)?;
        let mut bytes = Vec::new();
        bytes.try_reserve_exact(length).map_err(|_| {
            SourceError::new(ErrorKind::Unavailable, "Camera transfer allocation failed")
        })?;
        bytes.resize(length, 0);
        Ok(TransferBuffer {
            bytes,
            _reservation: reservation,
        })
    }
    /// Returns a bounded, element-aligned ImageBytes payload chunk in ASCOM order.
    /// The caller retains this image handle until the transfer finishes or cancels.
    pub fn imagebytes_chunk(&self, offset: usize, maximum: usize) -> Result<Vec<u8>, SourceError> {
        let descriptor = self.descriptor();
        let size = descriptor.transmission_type().bytes();
        if offset > self.bytes().len()
            || !offset.is_multiple_of(size)
            || maximum < size
            || maximum > IMAGE_CHUNK_BYTES
        {
            return Err(invalid("Invalid camera image chunk range or alignment"));
        }
        let length = (self.bytes().len() - offset).min(maximum / size * size);
        if descriptor.order() == ImageOrder::Ascom {
            return Ok(self.bytes()[offset..offset + length].to_vec());
        }
        let mut bytes = Vec::with_capacity(length);
        for element in offset / size..(offset + length) / size {
            let start = descriptor.ascom_offset(element);
            bytes.extend_from_slice(&self.bytes()[start..start + size]);
        }
        Ok(bytes)
    }
}

pub(crate) struct TransferBuffer {
    bytes: Vec<u8>,
    _reservation: Reservation,
}
impl TransferBuffer {
    pub(crate) fn fill<'a>(&'a mut self, image: &CameraImage, offset: usize) -> &'a [u8] {
        let descriptor = image.descriptor();
        let size = descriptor.transmission_type().bytes();
        debug_assert!(offset <= image.bytes().len() && offset.is_multiple_of(size));
        let length = (image.bytes().len() - offset).min(self.bytes.len());
        if descriptor.order() == ImageOrder::Ascom {
            self.bytes[..length].copy_from_slice(&image.bytes()[offset..offset + length]);
        } else {
            for (index, element) in (offset / size..(offset + length) / size).enumerate() {
                let source = descriptor.ascom_offset(element);
                self.bytes[index * size..(index + 1) * size]
                    .copy_from_slice(&image.bytes()[source..source + size]);
            }
        }
        &self.bytes[..length]
    }
}

#[derive(Debug)]
pub enum ImageReadError {
    Io(std::io::Error),
    Contract(SourceError),
    Upstream { code: u32, message: String },
}
impl std::fmt::Display for ImageReadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(e) => write!(f, "Camera image stream failed: {e}"),
            Self::Contract(e) => e.fmt(f),
            Self::Upstream { code, message } => write!(f, "Camera image error {code}: {message}"),
        }
    }
}
impl std::error::Error for ImageReadError {}
impl From<std::io::Error> for ImageReadError {
    fn from(value: std::io::Error) -> Self {
        Self::Io(value)
    }
}
impl From<SourceError> for ImageReadError {
    fn from(value: SourceError) -> Self {
        Self::Contract(value)
    }
}
pub struct ImageBytesResponse {
    pub client_transaction: u32,
    pub server_transaction: u32,
    pub image: CameraImage,
}

/// Read one finite HTTP response body. Callers supply a deadline and cancellation;
/// trailing bytes, partial bodies and invalid metadata never publish an image.
/// This function does not reconnect, restart an exposure or retry a download.
pub async fn read_imagebytes<R: AsyncRead + Unpin>(
    reader: &mut R,
    budget: &ImageBudget,
    expected_client_transaction: u32,
) -> Result<ImageBytesResponse, ImageReadError> {
    read_imagebytes_matching(reader, budget, expected_client_transaction, None).await
}

/// IPC has already admitted an exact descriptor. Reject a differing binary
/// header before reserving or allocating its pixels.
pub(crate) async fn read_imagebytes_matching<R: AsyncRead + Unpin>(
    reader: &mut R,
    budget: &ImageBudget,
    expected_client_transaction: u32,
    expected: Option<(ImageDescriptor, u32)>,
) -> Result<ImageBytesResponse, ImageReadError> {
    let mut header = [0; IMAGEBYTES_HEADER_BYTES];
    reader.read_exact(&mut header).await?;
    let fields: Vec<u32> = header
        .as_chunks::<4>()
        .0
        .iter()
        .map(|v| u32::from_le_bytes(*v))
        .collect();
    if fields[0] != 1
        || fields[1] > i32::MAX as u32
        || fields[2] != expected_client_transaction
        || fields[4] < IMAGEBYTES_HEADER_BYTES as u32
        || fields[4] > IMAGE_CHUNK_BYTES as u32
    {
        return Err(invalid("Invalid ImageBytes header or client transaction").into());
    }
    let descriptor = if fields[1] == 0 {
        let planes = match (fields[7], fields[10]) {
            (2, 0) => None,
            (3, planes) if planes != 0 => Some(planes),
            _ => return Err(invalid("Invalid ImageBytes rank or plane dimension").into()),
        };
        Some(ImageDescriptor::new(
            fields[8],
            fields[9],
            planes,
            ElementType::from_id(fields[5])?,
            ElementType::from_id(fields[6])?,
            ImageOrder::Ascom,
        )?)
    } else {
        None
    };
    if let Some((expected_descriptor, expected_server_transaction)) = expected
        && (descriptor != Some(expected_descriptor)
            || fields[3] != expected_server_transaction
            || fields[4] != IMAGEBYTES_HEADER_BYTES as u32)
    {
        return Err(invalid("ImageBytes header differs from its admitted body contract").into());
    }
    // Discard a bounded metadata extension without reserving an image-sized buffer.
    let mut remaining = fields[4] as usize - IMAGEBYTES_HEADER_BYTES;
    let mut discard = [0; 1024];
    while remaining != 0 {
        let length = remaining.min(discard.len());
        reader.read_exact(&mut discard[..length]).await?;
        remaining -= length;
    }
    let Some(descriptor) = descriptor else {
        let mut bytes = Vec::new();
        reader
            .take(MAX_ERROR_BYTES as u64 + 1)
            .read_to_end(&mut bytes)
            .await?;
        if bytes.len() > MAX_ERROR_BYTES {
            return Err(invalid("Camera image error message is too large").into());
        }
        let message =
            String::from_utf8(bytes).map_err(|_| invalid("Camera image error is not UTF-8"))?;
        return Err(ImageReadError::Upstream {
            code: fields[1],
            message,
        });
    };
    let mut allocation = budget.allocate(descriptor)?;
    reader.read_exact(allocation.bytes_mut()).await?;
    if reader.read(&mut [0; 1]).await? != 0 {
        return Err(invalid("Camera image body contains trailing data").into());
    }
    Ok(ImageBytesResponse {
        client_transaction: fields[2],
        server_transaction: fields[3],
        image: allocation.finish(),
    })
}
