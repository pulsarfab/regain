//! Camera HTTP publication over the same host-owned acquisition and image path.
use super::*;
use axum::{
    body::{Body, Bytes},
    response::{IntoResponse, Response},
};
use regain_hub::camera::{
    acquisition::{ExposureRequest, GuideRequest},
    image::{CameraImage, ElementType, IMAGE_CHUNK_BYTES, ImageOrder},
    ipc_image::{ImageRequest, MAX_TRANSFER_TIME, download_from_stream},
    properties::{CameraProperty, CameraSetting},
};
use tokio::sync::OwnedSemaphorePermit;

pub(super) fn supported(capabilities: &[String]) -> bool {
    [
        "cameraAcquisition",
        "cameraImageStream",
        "cameraOperationTiming",
        "asyncOutputConnection",
        "scalarDeviceState",
    ]
    .iter()
    .all(|required| capabilities.iter().any(|value| value == required))
}

pub(super) fn operation(output: Uuid, member: &str, put: bool, p: &Params) -> Result<Command> {
    if !put {
        return Ok(Command::Get {
            output,
            property: Get::Camera {
                property: CameraProperty::from_member(member).ok_or_else(|| unsupported(member))?,
            },
        });
    }
    let integer = |name| -> Result<i32> {
        i32::try_from(p.integer(name)?)
            .map_err(|_| error(0x401, "Expected an Int32 camera parameter"))
    };
    let property = match member {
        "startexposure" => Put::StartExposure {
            request: ExposureRequest {
                duration_seconds: p.number("Duration")?,
                light: p.boolean("Light")?,
            },
        },
        "stopexposure" => Put::StopExposure {},
        "abortexposure" => Put::AbortExposure {},
        "pulseguide" => {
            let request = GuideRequest {
                direction: integer("Direction")?,
                duration_milliseconds: integer("Duration")?,
            };
            request
                .validate()
                .map_err(|_| error(0x401, "Invalid guide direction or duration"))?;
            Put::PulseGuide { request }
        }
        _ => {
            let (name, value) = match member {
                "binx" => ("BinX", json!(integer("BinX")?)),
                "biny" => ("BinY", json!(integer("BinY")?)),
                "numx" => ("NumX", json!(integer("NumX")?)),
                "numy" => ("NumY", json!(integer("NumY")?)),
                "startx" => ("StartX", json!(integer("StartX")?)),
                "starty" => ("StartY", json!(integer("StartY")?)),
                "gain" => ("Gain", json!(integer("Gain")?)),
                "offset" => ("Offset", json!(integer("Offset")?)),
                "readoutmode" => ("ReadoutMode", json!(integer("ReadoutMode")?)),
                "fastreadout" => ("FastReadout", json!(p.boolean("FastReadout")?)),
                "cooleron" => ("CoolerOn", json!(p.boolean("CoolerOn")?)),
                "setccdtemperature" => ("SetCCDTemperature", json!(p.number("SetCCDTemperature")?)),
                "subexposureduration" => (
                    "SubExposureDuration",
                    json!(p.number("SubExposureDuration")?),
                ),
                _ => return Err(unsupported(member)),
            };
            let setting = CameraSetting::from_parameters(
                member,
                &regain_hub::source::Values::from([(name.into(), value)]),
            )
            .map_err(|_| error(0x401, "Invalid camera setting"))?;
            Put::CameraSetting { setting }
        }
    };
    Ok(Command::Put { output, property })
}

impl Publisher {
    /// Only reads an already completed frame. HTTP cancellation drops the reader;
    /// acquisition, guiding and the owning control stream remain host-supervised.
    pub async fn image(
        self: &Arc<Self>,
        device: &OutputDescriptor,
        params: &Params,
        binary: bool,
        transaction: u32,
        server: u32,
    ) -> Result<Response> {
        ensure!(
            device.device_type == DeviceType::Camera,
            unsupported("imagearray")
        );
        let permit = self
            .image_readers
            .clone()
            .try_acquire_owned()
            .map_err(|_| error(0x40b, "Camera image readers are busy"))?;
        let session = self
            .existing(params.optional_id("ClientID")?)
            .ok_or_else(|| error(0x407, "Connect this Alpaca client first"))?;
        ensure!(
            !session.retired.load(Ordering::SeqCst),
            error(0x407, "Hub client is disconnected")
        );
        let client = session
            .client
            .get()
            .ok_or_else(|| error(0x407, "Hub client is not connected"))?;
        ensure!(
            supported(&client.hello().capabilities),
            unsupported("Camera image publication requires an updated host")
        );
        let status = client
            .request(Command::Get {
                output: device.id,
                property: Get::CameraAcquisition {},
            })
            .await
            .map_err(translate)?;
        ensure!(
            status.get("imageReady").and_then(Value::as_bool) == Some(true),
            error(0x402, "Camera image is not ready")
        );
        let completed = status
            .get("completed")
            .ok_or_else(|| error(0x500, "Invalid completed camera identity"))?;
        let identity = |key| -> Result<Uuid> {
            completed
                .get(key)
                .and_then(Value::as_str)
                .and_then(|value| Uuid::parse_str(value).ok())
                .filter(|id| !id.is_nil())
                .ok_or_else(|| error(0x500, "Invalid completed camera identity"))
        };
        let request = ImageRequest {
            host_instance: client.hello().host_instance,
            configuration_revision: client.hello().configuration_revision,
            client_id: client.hello().client_id,
            output: device.id,
            source: identity("source")?,
            generation: identity("generation")?,
            acquisition: identity("acquisition")?,
        };
        let stream = self
            .endpoint
            .connect(Duration::from_secs(5))
            .await
            .map_err(|_| error(0x407, "Camera image endpoint is unavailable"))?;
        let frame = download_from_stream(
            stream,
            self.instance,
            request,
            &self.image_budget,
            MAX_TRANSFER_TIME,
        )
        .await
        .map_err(translate)?;
        ensure!(
            client.is_connected() && !session.retired.load(Ordering::SeqCst),
            error(0x407, "Hub client disconnected during image transfer")
        );
        response(frame.image, permit, binary, transaction, server).await
    }
}

// Pixel and concurrency reservations live until the HTTP body is consumed or
// dropped. Only bounded encoded chunks are allocated; no JSON pixel tree exists.
async fn response(
    image: CameraImage,
    permit: OwnedSemaphorePermit,
    binary: bool,
    client: u32,
    server: u32,
) -> Result<Response> {
    let descriptor = image.descriptor();
    ensure!(
        descriptor.order() == ImageOrder::Ascom,
        error(0x500, "Invalid camera image order")
    );
    let size = descriptor.transmission_type().bytes();
    if !binary
        && matches!(
            descriptor.transmission_type(),
            ElementType::Single | ElementType::Double
        )
    {
        for chunk in image.bytes().chunks(IMAGE_CHUNK_BYTES) {
            for bytes in chunk.chunks_exact(size) {
                let finite = match descriptor.transmission_type() {
                    ElementType::Single => {
                        f32::from_le_bytes(bytes.try_into().unwrap()).is_finite()
                    }
                    _ => f64::from_le_bytes(bytes.try_into().unwrap()).is_finite(),
                };
                ensure!(
                    finite,
                    error(
                        0x402,
                        "Camera pixels cannot be represented as finite JSON numbers"
                    )
                );
            }
            tokio::task::yield_now().await;
        }
    }
    let prefix = if binary {
        descriptor.imagebytes_header(client, server).to_vec()
    } else {
        format!("{{\"ClientTransactionID\":{client},\"ServerTransactionID\":{server},\"ErrorNumber\":0,\"ErrorMessage\":\"\",\"Type\":{},\"Rank\":{},\"Value\":[", descriptor.element_type() as u32, if descriptor.planes().is_some() {3} else {2}).into_bytes()
    };
    let length = descriptor.byte_len();
    let chunks = futures_util::stream::unfold(
        (image, permit, 0usize, Some(prefix)),
        move |(image, permit, mut offset, prefix)| async move {
            if let Some(prefix) = prefix {
                return Some((
                    Ok::<_, std::convert::Infallible>(Bytes::from(prefix)),
                    (image, permit, offset, None),
                ));
            }
            if offset > length || (binary && offset == length) {
                return None;
            }
            if offset == length {
                return Some((
                    Ok(Bytes::from_static(b"]}")),
                    (image, permit, length + 1, None),
                ));
            }
            let bytes = if binary {
                let end = (offset + IMAGE_CHUNK_BYTES).min(length);
                let bytes = Bytes::copy_from_slice(&image.bytes()[offset..end]);
                offset = end;
                bytes
            } else {
                let mut bytes = Vec::with_capacity(IMAGE_CHUNK_BYTES);
                while offset < length && bytes.len() < IMAGE_CHUNK_BYTES - 128 {
                    let element = offset / size;
                    let planes = descriptor.planes().unwrap_or(1) as usize;
                    let rows = descriptor.height() as usize * planes;
                    if element.is_multiple_of(rows) {
                        if element > 0 {
                            bytes.push(b',');
                        }
                        bytes.push(b'[');
                    }
                    if descriptor.planes().is_some() && element.is_multiple_of(planes) {
                        if !element.is_multiple_of(rows) {
                            bytes.push(b',');
                        }
                        bytes.push(b'[');
                    } else if !element.is_multiple_of(rows) {
                        bytes.push(b',');
                    }
                    number(
                        &mut bytes,
                        descriptor.transmission_type(),
                        &image.bytes()[offset..offset + size],
                    );
                    offset += size;
                    if descriptor.planes().is_some() && (element + 1).is_multiple_of(planes) {
                        bytes.push(b']');
                    }
                    if (element + 1).is_multiple_of(rows) {
                        bytes.push(b']');
                    }
                }
                Bytes::from(bytes)
            };
            Some((Ok(bytes), (image, permit, offset, None)))
        },
    );
    let mut response = Body::from_stream(chunks).into_response();
    response.headers_mut().insert(
        "Content-Type",
        if binary {
            "application/imagebytes"
        } else {
            "application/json"
        }
        .parse()
        .unwrap(),
    );
    response
        .headers_mut()
        .insert("Cache-Control", "no-store".parse().unwrap());
    if binary {
        response
            .headers_mut()
            .insert("Content-Length", (44 + length).to_string().parse().unwrap());
    }
    Ok(response)
}

fn number(output: &mut Vec<u8>, kind: ElementType, bytes: &[u8]) {
    macro_rules! write {
        ($ty:ty) => {
            serde_json::to_writer(output, &<$ty>::from_le_bytes(bytes.try_into().unwrap())).unwrap()
        };
    }
    match kind {
        ElementType::Byte => serde_json::to_writer(output, &bytes[0]).unwrap(),
        ElementType::Int16 => write!(i16),
        ElementType::Int32 => write!(i32),
        ElementType::Int64 => write!(i64),
        ElementType::UInt16 => write!(u16),
        ElementType::UInt32 => write!(u32),
        ElementType::UInt64 => write!(u64),
        ElementType::Single => write!(f32),
        ElementType::Double => write!(f64),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use http_body_util::BodyExt;
    use regain_hub::camera::image::{ImageBudget, ImageDescriptor};

    fn frame(kind: ElementType, planes: Option<u32>) -> (CameraImage, ImageBudget, Vec<Value>) {
        let descriptor = ImageDescriptor::new(2, 3, planes, kind, kind, ImageOrder::Ascom).unwrap();
        let budget = ImageBudget::new(descriptor.byte_len()).unwrap();
        let mut allocation = budget.allocate(descriptor).unwrap();
        let mut expected = Vec::new();
        for (index, chunk) in allocation
            .bytes_mut()
            .chunks_exact_mut(kind.bytes())
            .enumerate()
        {
            macro_rules! sample {
                ($ty:ty, $value:expr) => {{
                    let value: $ty = $value;
                    chunk.copy_from_slice(&value.to_le_bytes());
                    expected.push(json!(value));
                }};
            }
            match kind {
                ElementType::Byte => {
                    chunk[0] = index as u8;
                    expected.push(json!(index));
                }
                ElementType::Int16 => sample!(i16, index as i16 - 10),
                ElementType::Int32 => sample!(i32, i32::MIN + index as i32),
                ElementType::Int64 => sample!(i64, i64::MIN + index as i64),
                ElementType::UInt16 => sample!(u16, u16::MAX - index as u16),
                ElementType::UInt32 => sample!(u32, u32::MAX - index as u32),
                ElementType::UInt64 => sample!(u64, u64::MAX - index as u64),
                ElementType::Single => sample!(f32, index as f32 - 2.25),
                ElementType::Double => sample!(f64, index as f64 - 10.125),
            }
        }
        (allocation.finish(), budget, expected)
    }

    #[tokio::test]
    async fn image_publication_preserves_all_nine_numeric_types_ranks_and_transaction_headers() {
        for kind in [
            ElementType::Byte,
            ElementType::Int16,
            ElementType::Int32,
            ElementType::Int64,
            ElementType::UInt16,
            ElementType::UInt32,
            ElementType::UInt64,
            ElementType::Single,
            ElementType::Double,
        ] {
            for planes in [None, Some(1), Some(3)] {
                let (image, budget, expected) = frame(kind, planes);
                let readers = Arc::new(tokio::sync::Semaphore::new(1));
                let json = response(
                    image.clone(),
                    readers.clone().try_acquire_owned().unwrap(),
                    false,
                    u32::MAX,
                    123,
                )
                .await
                .unwrap();
                let json: Value =
                    serde_json::from_slice(&json.into_body().collect().await.unwrap().to_bytes())
                        .unwrap();
                assert_eq!(json["Type"], kind as u32);
                assert_eq!(json["Rank"], if planes.is_some() { 3 } else { 2 });
                assert_eq!(json["ClientTransactionID"], u32::MAX);
                assert_eq!(json["ServerTransactionID"], 123);
                for x in 0..2 {
                    for y in 0..3 {
                        for p in 0..planes.unwrap_or(1) as usize {
                            let value = if planes.is_some() {
                                &json["Value"][x][y][p]
                            } else {
                                &json["Value"][x][y]
                            };
                            assert_eq!(
                                *value,
                                expected[(x * 3 + y) * planes.unwrap_or(1) as usize + p]
                            );
                        }
                    }
                }
                let wire = response(
                    image.clone(),
                    readers.clone().try_acquire_owned().unwrap(),
                    true,
                    456,
                    u32::MAX,
                )
                .await
                .unwrap();
                let bytes = wire.into_body().collect().await.unwrap().to_bytes();
                assert_eq!(
                    &bytes[..44],
                    &image.descriptor().imagebytes_header(456, u32::MAX)
                );
                assert_eq!(&bytes[44..], image.bytes());
                assert_eq!(readers.available_permits(), 1);
                drop(image);
                assert_eq!(budget.used_bytes(), 0);
            }
        }
    }

    #[tokio::test]
    async fn large_rgb_json_stream_crosses_chunk_boundaries_and_roundtrips_without_unsigned_loss() {
        let descriptor = ImageDescriptor::new(
            2,
            20000,
            Some(3),
            ElementType::UInt64,
            ElementType::UInt64,
            ImageOrder::Ascom,
        )
        .unwrap();
        let budget = ImageBudget::new(descriptor.byte_len()).unwrap();
        let mut allocation = budget.allocate(descriptor).unwrap();
        for (index, chunk) in allocation
            .bytes_mut()
            .as_chunks_mut::<8>()
            .0
            .iter_mut()
            .enumerate()
        {
            chunk.copy_from_slice(&(u64::MAX - index as u64).to_le_bytes());
        }
        let image = allocation.finish();
        let readers = Arc::new(tokio::sync::Semaphore::new(1));
        let mut body = response(
            image.clone(),
            readers.clone().try_acquire_owned().unwrap(),
            false,
            123,
            456,
        )
        .await
        .unwrap()
        .into_body();
        let mut wire = Vec::new();
        let mut count = 0;
        while let Some(chunk) = body.frame().await {
            let chunk = chunk.unwrap().into_data().unwrap();
            assert!(chunk.len() <= IMAGE_CHUNK_BYTES);
            wire.extend_from_slice(&chunk);
            count += 1;
        }
        assert!(count > 3);
        let decoded = regain_hub::camera::json_image::read_json_image(
            &mut wire.as_slice(),
            &ImageBudget::new(8 * 1024 * 1024).unwrap(),
            123,
        )
        .await
        .unwrap();
        assert_eq!(decoded.descriptor(), descriptor);
        assert_eq!(decoded.bytes(), image.bytes());
        drop(body);
        drop(image);
        assert_eq!(budget.used_bytes(), 0);
        assert_eq!(readers.available_permits(), 1);
    }

    #[tokio::test]
    async fn body_drop_releases_budget_and_admission_and_json_rejects_nonfinite_before_headers() {
        for kind in [ElementType::Single, ElementType::Double] {
            let (image, budget, _) = frame(kind, None);
            let mut allocation = ImageBudget::new(image.bytes().len())
                .unwrap()
                .allocate(image.descriptor())
                .unwrap();
            if kind == ElementType::Single {
                allocation.bytes_mut()[..4].copy_from_slice(&f32::NAN.to_le_bytes());
            } else {
                allocation.bytes_mut()[..8].copy_from_slice(&f64::INFINITY.to_le_bytes());
            }
            let invalid = allocation.finish();
            let readers = Arc::new(tokio::sync::Semaphore::new(1));
            assert!(
                response(
                    invalid.clone(),
                    readers.clone().try_acquire_owned().unwrap(),
                    false,
                    1,
                    2
                )
                .await
                .is_err()
            );
            assert_eq!(readers.available_permits(), 1);
            let wire = response(
                invalid,
                readers.clone().try_acquire_owned().unwrap(),
                true,
                1,
                2,
            )
            .await
            .unwrap();
            drop(wire);
            assert_eq!(readers.available_permits(), 1);
            let body = response(
                image,
                readers.clone().try_acquire_owned().unwrap(),
                true,
                1,
                2,
            )
            .await
            .unwrap();
            assert_eq!(readers.available_permits(), 0);
            assert!(budget.used_bytes() > 0);
            let mut body = body.into_body();
            assert_eq!(
                body.frame()
                    .await
                    .unwrap()
                    .unwrap()
                    .into_data()
                    .unwrap()
                    .len(),
                44
            );
            drop(body);
            assert_eq!(budget.used_bytes(), 0);
            assert_eq!(readers.available_permits(), 1);
        }
    }
}
