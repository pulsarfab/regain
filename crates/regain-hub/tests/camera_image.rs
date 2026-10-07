//! Private binary fixtures only; no camera, vendor driver or network is opened.
use regain_hub::{
    camera::image::{
        CameraImage, ElementType as T, IMAGE_CHUNK_BYTES, ImageBudget, ImageDescriptor, ImageOrder,
        ImageReadError, MAX_IMAGE_BYTES, read_imagebytes,
    },
    source::ErrorKind,
};
use std::sync::{Arc, Barrier};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

fn descriptor(
    width: u32,
    height: u32,
    planes: Option<u32>,
    logical: T,
    encoded: T,
    order: ImageOrder,
) -> ImageDescriptor {
    ImageDescriptor::new(width, height, planes, logical, encoded, order).unwrap()
}
fn fixture(d: ImageDescriptor, data: &[u8]) -> Vec<u8> {
    let mut bytes = d.imagebytes_header(u32::MAX, 123).to_vec();
    bytes.extend_from_slice(data);
    bytes
}
fn set_field(bytes: &mut [u8], field: usize, value: u32) {
    bytes[field * 4..field * 4 + 4].copy_from_slice(&value.to_le_bytes());
}
fn image(budget: &ImageBudget, d: ImageDescriptor, data: &[u8]) -> CameraImage {
    let mut allocation = budget.allocate(d).unwrap();
    allocation.bytes_mut().copy_from_slice(data);
    allocation.finish()
}

#[test]
fn geometry_and_wire_deserialization_cannot_bypass_validation() {
    for (width, height, planes) in [
        (0, 2, None),
        (2, 0, None),
        (2, 2, Some(0)),
        (u32::MAX, 1, None),
        (i32::MAX as u32, i32::MAX as u32, Some(i32::MAX as u32)),
    ] {
        assert!(
            ImageDescriptor::new(
                width,
                height,
                planes,
                T::Double,
                T::Double,
                ImageOrder::Ascom
            )
            .is_err()
        );
    }
    let bound = descriptor(
        MAX_IMAGE_BYTES as u32,
        1,
        None,
        T::Byte,
        T::Byte,
        ImageOrder::Ascom,
    );
    assert_eq!(bound.byte_len(), MAX_IMAGE_BYTES);
    assert!(
        ImageDescriptor::new(
            MAX_IMAGE_BYTES as u32 + 1,
            1,
            None,
            T::Byte,
            T::Byte,
            ImageOrder::Ascom
        )
        .is_err()
    );
    assert_eq!(
        ImageDescriptor::new(1, 1, None, T::Int32, T::Double, ImageOrder::Ascom)
            .unwrap_err()
            .kind,
        ErrorKind::Unsupported
    );
    let mut wire = serde_json::to_value(bound).unwrap();
    wire["width"] = serde_json::json!(0);
    assert!(serde_json::from_value::<ImageDescriptor>(wire).is_err());
    let mut wire = serde_json::to_value(bound).unwrap();
    wire["extra"] = serde_json::json!(true);
    assert!(serde_json::from_value::<ImageDescriptor>(wire).is_err());
    for planes in [None, Some(1), Some(3), Some(4)] {
        let d = descriptor(3, 2, planes, T::Int32, T::UInt16, ImageOrder::SensorRows);
        assert_eq!(
            serde_json::from_str::<ImageDescriptor>(&serde_json::to_string(&d).unwrap()).unwrap(),
            d
        );
        let header = d.imagebytes_header(u32::MAX, u32::MAX);
        assert_eq!(
            &header[28..32],
            &(if planes.is_some() { 3u32 } else { 2u32 }).to_le_bytes()
        );
    }
}

#[test]
fn clones_and_abandoned_allocations_hold_one_reservation_until_last_reader() {
    let budget = ImageBudget::new(12).unwrap();
    let d = descriptor(3, 2, None, T::Int32, T::UInt16, ImageOrder::SensorRows);
    let a = budget.allocate(d).unwrap();
    assert_eq!(budget.used_bytes(), 12);
    assert_eq!(budget.allocate(d).err().unwrap().kind, ErrorKind::Busy);
    drop(a);
    assert_eq!(budget.used_bytes(), 0);
    let source = image(&budget, d, &[1; 12]);
    let reader = source.clone();
    assert_eq!(source.bytes().as_ptr(), reader.bytes().as_ptr());
    drop(source);
    assert_eq!(budget.used_bytes(), 12);
    assert!(budget.allocate(d).is_err());
    assert_eq!(reader.imagebytes_chunk(0, 12).unwrap(), [1; 12]);
    drop(reader);
    assert_eq!(budget.used_bytes(), 0);
    assert!(budget.allocate(d).is_ok());
    assert!(ImageBudget::new(0).is_err());
}

#[test]
fn simultaneous_captures_cannot_overcommit_shared_budget() {
    let budget = ImageBudget::new(16).unwrap();
    let barrier = Arc::new(Barrier::new(9));
    let ready = Arc::new(Barrier::new(9));
    let mut threads = Vec::new();
    for _ in 0..8 {
        let budget = budget.clone();
        let barrier = barrier.clone();
        let ready = ready.clone();
        threads.push(std::thread::spawn(move || {
            barrier.wait();
            let result = budget.allocate(descriptor(
                4,
                1,
                None,
                T::Int32,
                T::Int32,
                ImageOrder::Ascom,
            ));
            ready.wait();
            result
        }));
    }
    barrier.wait();
    ready.wait();
    let results: Vec<_> = threads.into_iter().map(|t| t.join().unwrap()).collect();
    assert_eq!(results.iter().filter(|v| v.is_ok()).count(), 1);
    assert_eq!(budget.used_bytes(), 16);
    drop(results);
    assert_eq!(budget.used_bytes(), 0);
}

#[test]
fn native_unsigned_frame_is_adopted_without_copy_and_transposed_in_chunks() {
    let pixels: Vec<u8> = [1u16, 2, 65535, 4, 50000, 6]
        .into_iter()
        .flat_map(u16::to_le_bytes)
        .collect();
    let frame = regain_core::Frame {
        exposure: regain_core::Exposure {
            width: 3,
            height: 2,
            bin: 1,
            x: 0,
            y: 0,
            microseconds: 1000,
            dark: false,
        },
        pixels: pixels.into(),
        metadata: serde_json::Value::Null,
    };
    let budget = ImageBudget::new(12).unwrap();
    let original_pixels = frame.pixels.as_ptr();
    let exposure = frame.exposure.clone();
    let value = budget.adopt_native(frame).unwrap();
    assert_eq!(value.bytes().as_ptr(), original_pixels);
    assert_eq!(value.descriptor().element_type(), T::Int32);
    let mut exported = Vec::new();
    for offset in (0..12).step_by(4) {
        exported.extend(value.imagebytes_chunk(offset, 5).unwrap());
    }
    assert_eq!(
        exported,
        [1u16, 4, 2, 50000, 65535, 6]
            .into_iter()
            .flat_map(u16::to_le_bytes)
            .collect::<Vec<_>>()
    );
    assert!(value.imagebytes_chunk(12, 2).unwrap().is_empty());
    for (offset, max) in [
        (1, 2),
        (14, 2),
        (0, 1),
        (0, IMAGE_CHUNK_BYTES + 1),
        (usize::MAX, 8),
    ] {
        assert!(value.imagebytes_chunk(offset, max).is_err());
    }
    drop(value);
    let malformed = regain_core::Frame {
        exposure,
        pixels: Arc::from([1u8; 11]),
        metadata: serde_json::Value::Null,
    };
    assert!(budget.adopt_native(malformed).is_err());
    assert_eq!(budget.used_bytes(), 0);
}

#[tokio::test]
async fn all_nine_numeric_encodings_and_packed_int32_survive_exactly() {
    let kinds = [
        T::Int16,
        T::Int32,
        T::Double,
        T::Single,
        T::UInt64,
        T::Byte,
        T::Int64,
        T::UInt16,
        T::UInt32,
    ];
    let pairs = kinds
        .into_iter()
        .map(|t| (t, t))
        .chain([T::Byte, T::Int16, T::UInt16].map(|t| (T::Int32, t)));
    for (logical, encoded) in pairs {
        for planes in [None, Some(1), Some(3), Some(4)] {
            let d = descriptor(3, 2, planes, logical, encoded, ImageOrder::Ascom);
            // Includes sign bits, >16-bit values and arbitrary IEEE bit patterns;
            // the transport must never round, clamp or reinterpret them as u16.
            let bytes: Vec<u8> = (0..d.byte_len())
                .map(|i| (i as u8).wrapping_mul(73))
                .collect();
            let body = fixture(d, &bytes);
            let budget = ImageBudget::new(d.byte_len()).unwrap();
            let response = read_imagebytes(&mut body.as_slice(), &budget, u32::MAX)
                .await
                .unwrap();
            assert_eq!(response.client_transaction, u32::MAX);
            assert_eq!(response.server_transaction, 123);
            assert_eq!(response.image.descriptor(), d);
            assert_eq!(response.image.bytes(), bytes);
            assert_eq!(
                response
                    .image
                    .imagebytes_chunk(0, IMAGE_CHUNK_BYTES)
                    .unwrap(),
                bytes
            );
            assert_eq!(budget.used_bytes(), d.byte_len());
            drop(response);
            assert_eq!(budget.used_bytes(), 0);
        }
    }
}

#[tokio::test]
async fn rgb_and_lrgb_ordering_preserves_plane_as_the_rightmost_index() {
    for planes in [3, 4] {
        let d = descriptor(
            3,
            2,
            Some(planes),
            T::Int32,
            T::Int32,
            ImageOrder::SensorRows,
        );
        let mut native = Vec::new();
        for y in 0..2 {
            for x in 0..3 {
                for plane in 0..planes {
                    native.extend_from_slice(&(x * 100 + y * 10 + plane).to_le_bytes());
                }
            }
        }
        let budget = ImageBudget::new(d.byte_len() * 2).unwrap();
        let value = image(&budget, d, &native);
        let mut wire = d.imagebytes_header(u32::MAX, 123).to_vec();
        for offset in (0..d.byte_len()).step_by(8) {
            wire.extend(value.imagebytes_chunk(offset, 8).unwrap());
        }
        let result = read_imagebytes(&mut wire.as_slice(), &budget, u32::MAX)
            .await
            .unwrap();
        let mut expected = Vec::new();
        for x in 0..3 {
            for y in 0..2 {
                for plane in 0..planes {
                    expected.extend_from_slice(&(x * 100 + y * 10 + plane).to_le_bytes());
                }
            }
        }
        assert_eq!(result.image.bytes(), expected);
        drop(value);
        assert_eq!(budget.used_bytes(), d.byte_len());
        drop(result);
        assert_eq!(budget.used_bytes(), 0);
    }
}

#[test]
fn image_export_crosses_chunk_boundaries_without_full_image_transposition() {
    let d = descriptor(
        257,
        129,
        Some(3),
        T::UInt32,
        T::UInt32,
        ImageOrder::SensorRows,
    );
    let budget = ImageBudget::new(d.byte_len()).unwrap();
    let mut allocation = budget.allocate(d).unwrap();
    for (i, bytes) in allocation
        .bytes_mut()
        .as_chunks_mut::<4>()
        .0
        .iter_mut()
        .enumerate()
    {
        bytes.copy_from_slice(&(i as u32).to_le_bytes());
    }
    let value = allocation.finish();
    let first_reader = value.clone();
    let second_reader = value.clone();
    drop(value);
    let mut exported = Vec::new();
    let mut offset = 0;
    while offset < d.byte_len() {
        let chunk = first_reader
            .imagebytes_chunk(offset, IMAGE_CHUNK_BYTES)
            .unwrap();
        assert!(chunk.len() <= IMAGE_CHUNK_BYTES);
        offset += chunk.len();
        exported.extend(chunk);
    }
    assert_eq!(exported.len(), d.byte_len());
    for (i, bytes) in exported.as_chunks::<4>().0.iter().enumerate() {
        let plane = i % 3;
        let pixel = i / 3;
        let x = pixel / 129;
        let y = pixel % 129;
        assert_eq!(
            u32::from_le_bytes(*bytes),
            ((y * 257 + x) * 3 + plane) as u32
        );
    }
    drop(first_reader);
    assert_eq!(budget.used_bytes(), d.byte_len());
    assert_eq!(
        second_reader.imagebytes_chunk(0, 12).unwrap(),
        exported[..12]
    );
    drop(second_reader);
    assert_eq!(budget.used_bytes(), 0);
}

#[tokio::test]
async fn malformed_headers_fail_before_pixel_reservation() {
    let d = descriptor(3, 2, None, T::Int32, T::UInt16, ImageOrder::Ascom);
    for (field, value) in [
        (0, 2),
        (1, u32::MAX),
        (2, 12),
        (4, 43),
        (4, 65537),
        (5, 0),
        (6, 99),
        (7, 1),
        (7, 3),
        (8, 0),
        (9, u32::MAX),
        (10, 1),
        (8, MAX_IMAGE_BYTES as u32),
    ] {
        let mut wire = fixture(d, &[0; 12]);
        set_field(&mut wire, field, value);
        let budget = ImageBudget::new(12).unwrap();
        assert!(
            read_imagebytes(&mut wire.as_slice(), &budget, u32::MAX)
                .await
                .is_err(),
            "field {field} = {value}"
        );
        assert_eq!(budget.used_bytes(), 0);
    }
    let mut wire = fixture(d, &[0; 12]);
    set_field(&mut wire, 6, T::Double as u32);
    assert!(
        matches!(read_imagebytes(&mut wire.as_slice(), &ImageBudget::new(12).unwrap(), u32::MAX).await,
        Err(ImageReadError::Contract(e)) if e.kind == ErrorKind::Unsupported)
    );
}

#[tokio::test]
async fn extended_header_is_bounded_and_truncation_or_trailing_data_never_publishes() {
    let d = descriptor(3, 2, None, T::Int32, T::UInt16, ImageOrder::Ascom);
    let budget = ImageBudget::new(12).unwrap();
    let wire = fixture(d, &[0; 12]);
    for length in [0, 43, 44, 55] {
        assert!(
            read_imagebytes(&mut &wire[..length], &budget, u32::MAX)
                .await
                .is_err()
        );
        assert_eq!(budget.used_bytes(), 0);
    }
    let mut trailing = wire.clone();
    trailing.push(0);
    assert!(
        read_imagebytes(&mut trailing.as_slice(), &budget, u32::MAX)
            .await
            .is_err()
    );
    assert_eq!(budget.used_bytes(), 0);
    let mut extended = d.imagebytes_header(u32::MAX, 123).to_vec();
    set_field(&mut extended, 4, IMAGE_CHUNK_BYTES as u32);
    extended.resize(IMAGE_CHUNK_BYTES, 123);
    assert!(
        read_imagebytes(&mut extended.as_slice(), &budget, u32::MAX)
            .await
            .is_err()
    );
    extended.extend_from_slice(&[7; 12]);
    let result = read_imagebytes(&mut extended.as_slice(), &budget, u32::MAX)
        .await
        .unwrap();
    assert_eq!(result.image.bytes(), [7; 12]);
    assert!(
        matches!(read_imagebytes(&mut wire.as_slice(), &budget, u32::MAX).await,
        Err(ImageReadError::Contract(e)) if e.kind == ErrorKind::Busy)
    );
    drop(result);
    assert_eq!(budget.used_bytes(), 0);
}

#[tokio::test(start_paused = true)]
async fn cancelled_partial_download_releases_memory_without_publishing_or_replaying() {
    let d = descriptor(3, 2, None, T::Int32, T::UInt16, ImageOrder::Ascom);
    let budget = ImageBudget::new(12).unwrap();
    let (mut writer, mut reader) = tokio::io::duplex(128);
    writer
        .write_all(&d.imagebytes_header(u32::MAX, 123))
        .await
        .unwrap();
    writer.write_all(&[7; 4]).await.unwrap();
    let result = tokio::time::timeout(
        std::time::Duration::from_secs(1),
        read_imagebytes(&mut reader, &budget, u32::MAX),
    )
    .await;
    assert!(result.is_err());
    assert_eq!(budget.used_bytes(), 0);
    writer.shutdown().await.unwrap();
    // Only the original four bytes were consumed; cancellation sends no command.
    let mut remaining = Vec::new();
    reader.read_to_end(&mut remaining).await.unwrap();
    assert!(remaining.is_empty());
}

#[tokio::test]
async fn upstream_error_is_utf8_and_bounded_without_allocating_an_image() {
    let d = descriptor(3, 2, None, T::Int32, T::UInt16, ImageOrder::Ascom);
    let mut wire = d.imagebytes_header(u32::MAX, 123).to_vec();
    set_field(&mut wire, 1, 0x500);
    // Error responses can leave the image geometry/type fields unspecified.
    for field in 5..11 {
        set_field(&mut wire, field, 0);
    }
    wire.extend_from_slice("Téléchargement interrompu".as_bytes());
    let budget = ImageBudget::new(12).unwrap();
    assert!(
        matches!(read_imagebytes(&mut wire.as_slice(), &budget, u32::MAX).await,
        Err(ImageReadError::Upstream {code: 0x500, message}) if message == "Téléchargement interrompu")
    );
    assert_eq!(budget.used_bytes(), 0);
    wire.truncate(44);
    wire.push(0xff);
    assert!(matches!(
        read_imagebytes(&mut wire.as_slice(), &budget, u32::MAX).await,
        Err(ImageReadError::Contract(_))
    ));
    wire.truncate(44);
    wire.resize(44 + 4097, b'x');
    assert!(matches!(
        read_imagebytes(&mut wire.as_slice(), &budget, u32::MAX).await,
        Err(ImageReadError::Contract(_))
    ));
    assert_eq!(budget.used_bytes(), 0);
}
