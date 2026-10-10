//! One finite ImageBytes body on a dedicated, authenticated local stream.
//! A transfer borrows an existing control connection and never touches equipment.
use super::{
    acquisition::CapturedImage,
    image::{
        CameraImage, IMAGE_CHUNK_BYTES, IMAGEBYTES_HEADER_BYTES, ImageBudget, ImageDescriptor,
        ImageOrder, ImageReadError, TransferBuffer, read_imagebytes_matching,
    },
};
use crate::{
    client::{ClientError, RemoteError, connection_error, decode},
    host::handshake,
    ipc::{Command, ProtocolError, Request, RpcError, VERSION, read_frame},
    runtime::OutputConnection,
    service::HubService,
    source::{ErrorKind, SourceError},
};
use serde::{Deserialize, Serialize};
use std::{sync::Arc, time::Duration};
use tokio::{
    io::{AsyncRead, AsyncWrite, AsyncWriteExt},
    time::timeout,
};
use uuid::Uuid;

pub const MAX_TRANSFER_TIME: Duration = Duration::from_secs(300);
const FRAME_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ImageRequest {
    pub host_instance: Uuid,
    pub configuration_revision: Uuid,
    /// The existing control stream's hello identity, not this reader's identity.
    pub client_id: Uuid,
    pub output: Uuid,
    pub source: Uuid,
    pub generation: Uuid,
    pub acquisition: Uuid,
}
/// A retained operation is host-owned; reattachment does not require the old
/// frontend's connection identity or an ordinary single-camera output lease.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct GroupImageRequest {
    pub host_instance: Uuid,
    pub configuration_revision: Uuid,
    pub group: Uuid,
    pub operation: Uuid,
    pub source: Uuid,
    pub generation: Uuid,
    pub acquisition: Uuid,
}
trait TransferRequest: Copy + PartialEq + Serialize + serde::de::DeserializeOwned {
    fn valid(&self) -> bool;
    fn host(&self) -> Uuid;
    fn revision(&self) -> Uuid;
    fn operation(&self) -> &'static str;
    fn command(self) -> Command;
}
impl TransferRequest for ImageRequest {
    fn valid(&self) -> bool {
        ImageRequest::valid(self)
    }
    fn host(&self) -> Uuid {
        self.host_instance
    }
    fn revision(&self) -> Uuid {
        self.configuration_revision
    }
    fn operation(&self) -> &'static str {
        "cameraImage"
    }
    fn command(self) -> Command {
        Command::CameraImage { request: self }
    }
}
impl TransferRequest for GroupImageRequest {
    fn valid(&self) -> bool {
        [
            self.host_instance,
            self.configuration_revision,
            self.group,
            self.operation,
            self.source,
            self.generation,
            self.acquisition,
        ]
        .iter()
        .all(|id| !id.is_nil())
    }
    fn host(&self) -> Uuid {
        self.host_instance
    }
    fn revision(&self) -> Uuid {
        self.configuration_revision
    }
    fn operation(&self) -> &'static str {
        "cameraGroupImage"
    }
    fn command(self) -> Command {
        Command::CameraGroupImage { request: self }
    }
}
impl ImageRequest {
    fn valid(&self) -> bool {
        [
            self.host_instance,
            self.configuration_revision,
            self.client_id,
            self.output,
            self.source,
            self.generation,
            self.acquisition,
        ]
        .iter()
        .all(|id| !id.is_nil())
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ImageManifest<R = ImageRequest> {
    pub request: R,
    pub descriptor: ImageDescriptor,
    pub payload_bytes: usize,
    pub chunk_bytes: usize,
    pub timeout_seconds: u32,
}

pub(crate) struct PreparedImage<R = ImageRequest> {
    pub(crate) manifest: ImageManifest<R>,
    image: Arc<CapturedImage>,
    buffer: TransferBuffer,
    _connection: Option<Arc<OutputConnection>>,
}
pub(crate) fn prepare(
    service: &HubService,
    request: ImageRequest,
) -> Result<PreparedImage, SourceError> {
    let invalid = || {
        SourceError::new(
            ErrorKind::Unavailable,
            "Camera image identity is no longer available",
        )
    };
    if !request.valid() || request.host_instance != service.host_id() {
        return Err(invalid());
    }
    let runtime = service.runtime()?;
    if request.configuration_revision != runtime.revision() {
        return Err(invalid());
    }
    let connection = runtime.image_connection(request.client_id, request.output)?;
    let image = connection.camera()?.image()?;
    if image.identity.source != request.source
        || image.identity.generation != request.generation
        || image.identity.acquisition != request.acquisition
    {
        return Err(invalid());
    }
    prepare_retained(request, image, Some(connection))
}
pub(crate) fn prepare_group(
    service: &HubService,
    request: GroupImageRequest,
) -> Result<PreparedImage<GroupImageRequest>, SourceError> {
    if !request.valid() || request.host_instance != service.host_id() {
        return Err(SourceError::new(
            ErrorKind::Unavailable,
            "Camera group image host identity does not match",
        ));
    }
    let runtime = service.runtime()?;
    let image = runtime.camera_group_image(
        request.configuration_revision,
        request.group,
        request.operation,
        request.source,
        request.generation,
        request.acquisition,
    )?;
    prepare_retained(request, image, None)
}
/// Shared immutable image export for ordinary outputs and retained group members.
pub(crate) fn prepare_retained<R>(
    request: R,
    image: Arc<CapturedImage>,
    connection: Option<Arc<OutputConnection>>,
) -> Result<PreparedImage<R>, SourceError> {
    let source = image.image.descriptor();
    let descriptor = ImageDescriptor::new(
        source.width(),
        source.height(),
        source.planes(),
        source.element_type(),
        source.transmission_type(),
        ImageOrder::Ascom,
    )?;
    // Reserve scratch before acknowledging a transfer. A full host budget
    // returns Busy without emitting a successful header or evicting old readers.
    let buffer = image.image.transfer_buffer()?;
    Ok(PreparedImage {
        manifest: ImageManifest {
            request,
            descriptor,
            payload_bytes: descriptor.byte_len() + IMAGEBYTES_HEADER_BYTES,
            chunk_bytes: IMAGE_CHUNK_BYTES,
            timeout_seconds: MAX_TRANSFER_TIME.as_secs() as u32,
        },
        image,
        buffer,
        _connection: connection,
    })
}
impl<R> PreparedImage<R> {
    pub(crate) async fn write<W: AsyncWrite + Unpin>(
        &mut self,
        writer: &mut W,
        chunk_timeout: Duration,
    ) -> Result<(), ProtocolError> {
        timeout(MAX_TRANSFER_TIME, async {
            timeout(
                chunk_timeout,
                writer.write_all(&self.manifest.descriptor.imagebytes_header(2, 2)),
            )
            .await
            .map_err(|_| ProtocolError::Timeout)?
            .map_err(|_| ProtocolError::Io)?;
            let mut offset = 0;
            while offset < self.image.image.bytes().len() {
                let bytes = self.buffer.fill(&self.image.image, offset);
                timeout(chunk_timeout, writer.write_all(bytes))
                    .await
                    .map_err(|_| ProtocolError::Timeout)?
                    .map_err(|_| ProtocolError::Io)?;
                offset += bytes.len();
                tokio::task::yield_now().await;
            }
            timeout(chunk_timeout, writer.shutdown())
                .await
                .map_err(|_| ProtocolError::Timeout)?
                .map_err(|_| ProtocolError::Io)
        })
        .await
        .unwrap_or(Err(ProtocolError::Timeout))
    }
}

pub struct DownloadedImage<R = ImageRequest> {
    pub manifest: ImageManifest<R>,
    pub image: CameraImage,
}

/// Supply a separate user-protected stream for this transfer. The request
/// refers to an already connected control client. No connection, capture or
/// source image download is retried. Dropping this future closes only the reader.
pub async fn download_from_stream<T>(
    stream: T,
    instance: Uuid,
    request: ImageRequest,
    budget: &ImageBudget,
    deadline: Duration,
) -> Result<DownloadedImage, ClientError>
where
    T: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    download_selected(stream, instance, request, budget, deadline).await
}
pub async fn download_group_from_stream<T>(
    stream: T,
    instance: Uuid,
    request: GroupImageRequest,
    budget: &ImageBudget,
    deadline: Duration,
) -> Result<DownloadedImage<GroupImageRequest>, ClientError>
where
    T: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    download_selected(stream, instance, request, budget, deadline).await
}
async fn download_selected<T, R>(
    mut stream: T,
    instance: Uuid,
    request: R,
    budget: &ImageBudget,
    deadline: Duration,
) -> Result<DownloadedImage<R>, ClientError>
where
    T: AsyncRead + AsyncWrite + Unpin + Send + 'static,
    R: TransferRequest,
{
    if instance.is_nil() || !request.valid() || deadline.is_zero() || deadline > MAX_TRANSFER_TIME {
        return Err(ClientError::InvalidRequest);
    }
    timeout(deadline, async {
        let hello = handshake(&mut stream, instance, FRAME_TIMEOUT)
            .await
            .map_err(connection_error)?;
        if hello.host_instance != request.host()
            || hello.configuration_revision != request.revision()
            || !hello.operations.iter().any(|op| op == request.operation())
            || !hello
                .capabilities
                .iter()
                .any(|cap| cap == "cameraImageStream")
        {
            return Err(ClientError::Protocol);
        }
        let bytes = serde_json::to_vec(&Request {
            version: VERSION,
            id: 2,
            command: request.command(),
        })
        .map_err(|_| ClientError::InvalidRequest)?;
        if bytes.len() > hello.max_frame_bytes {
            return Err(ClientError::InvalidRequest);
        }
        timeout(FRAME_TIMEOUT, async {
            stream
                .write_all(&(bytes.len() as u32).to_le_bytes())
                .await?;
            stream.write_all(&bytes).await?;
            stream.flush().await
        })
        .await
        .map_err(|_| ClientError::Timeout)?
        .map_err(|_| ClientError::Disconnected)?;
        let bytes = timeout(FRAME_TIMEOUT, read_frame(&mut stream, FRAME_TIMEOUT))
            .await
            .map_err(|_| ClientError::Timeout)?
            .map_err(|error| match error {
                ProtocolError::Timeout => ClientError::Timeout,
                ProtocolError::Io => ClientError::Disconnected,
                _ => ClientError::Protocol,
            })?
            .ok_or(ClientError::Disconnected)?;
        if bytes.len() > hello.max_frame_bytes {
            return Err(ClientError::Protocol);
        }
        let (id, reply) = decode(&bytes)?;
        if id != 2 {
            return Err(ClientError::Protocol);
        }
        let manifest: ImageManifest<R> =
            serde_json::from_value(reply?).map_err(|_| ClientError::Protocol)?;
        if manifest.request != request
            || manifest.descriptor.order() != ImageOrder::Ascom
            || manifest.payload_bytes != manifest.descriptor.byte_len() + IMAGEBYTES_HEADER_BYTES
            || manifest.chunk_bytes != IMAGE_CHUNK_BYTES
            || manifest.timeout_seconds != MAX_TRANSFER_TIME.as_secs() as u32
        {
            return Err(ClientError::Protocol);
        }
        let reply =
            read_imagebytes_matching(&mut stream, budget, 2, Some((manifest.descriptor, 2)))
                .await
                .map_err(|error| match error {
                    ImageReadError::Io(_) => ClientError::Disconnected,
                    ImageReadError::Contract(error) if error.kind == ErrorKind::Busy => {
                        ClientError::Busy
                    }
                    ImageReadError::Contract(error) if error.kind == ErrorKind::Unavailable => {
                        let error = RpcError::from(error);
                        ClientError::Remote(RemoteError {
                            code: error.code.into(),
                            message: error.message.into(),
                            upstream_code: error.upstream_code,
                            retry_after_seconds: error.retry_after_seconds,
                            fields: error.fields,
                        })
                    }
                    _ => ClientError::Protocol,
                })?;
        if reply.server_transaction != 2 {
            return Err(ClientError::Protocol);
        }
        Ok(DownloadedImage {
            manifest,
            image: reply.image,
        })
    })
    .await
    .unwrap_or(Err(ClientError::Timeout))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::camera::image::ElementType;
    use serde_json::{Value, json};
    use tokio::io::{AsyncReadExt, DuplexStream};

    async fn frame(stream: &mut DuplexStream, value: Value) {
        let bytes = serde_json::to_vec(&value).unwrap();
        stream
            .write_all(&(bytes.len() as u32).to_le_bytes())
            .await
            .unwrap();
        stream.write_all(&bytes).await.unwrap();
    }
    fn request() -> ImageRequest {
        ImageRequest {
            host_instance: Uuid::new_v4(),
            configuration_revision: Uuid::new_v4(),
            client_id: Uuid::new_v4(),
            output: Uuid::new_v4(),
            source: Uuid::new_v4(),
            generation: Uuid::new_v4(),
            acquisition: Uuid::new_v4(),
        }
    }
    async fn fake(
        request: ImageRequest,
        descriptor: ImageDescriptor,
        fault: &'static str,
    ) -> (DuplexStream, Uuid, tokio::task::JoinHandle<()>, Vec<u8>) {
        let instance = Uuid::new_v4();
        let (reader, mut server) = tokio::io::duplex(4096);
        let payload: Vec<_> = (0..descriptor.byte_len()).map(|i| (i * 17) as u8).collect();
        let pixels = payload.clone();
        let task = tokio::spawn(async move {
            let hello: Value = serde_json::from_slice(
                &read_frame(&mut server, FRAME_TIMEOUT)
                    .await
                    .unwrap()
                    .unwrap(),
            )
            .unwrap();
            assert_eq!(hello["command"]["op"], "hello");
            frame(
                &mut server,
                json!({"version":1,"id":1,"result":{
                "protocolVersion":1,"instanceId":instance,"hostInstance":request.host_instance,
                "configurationRevision":request.configuration_revision,"clientId":Uuid::new_v4(),
                "maxFrameBytes":crate::ipc::MAX_FRAME_BYTES,"maxInFlight":8,
                "operations":["cameraImage"],"capabilities":["cameraImageStream"]}}),
            )
            .await;
            let command: Value = serde_json::from_slice(
                &read_frame(&mut server, FRAME_TIMEOUT)
                    .await
                    .unwrap()
                    .unwrap(),
            )
            .unwrap();
            assert_eq!(command["id"], 2);
            assert_eq!(command["command"]["op"], "cameraImage");
            let mut manifest = json!(ImageManifest {
                request,
                descriptor,
                payload_bytes: descriptor.byte_len() + IMAGEBYTES_HEADER_BYTES,
                chunk_bytes: IMAGE_CHUNK_BYTES,
                timeout_seconds: MAX_TRANSFER_TIME.as_secs() as u32
            });
            match fault {
                "identity" => manifest["request"]["acquisition"] = json!(Uuid::new_v4()),
                "extra" => manifest["pixels"] = json!([1, 2]),
                "length" => manifest["payloadBytes"] = json!(1),
                "order" => manifest["descriptor"]["order"] = json!("sensorRows"),
                "chunk" => manifest["chunkBytes"] = json!(IMAGE_CHUNK_BYTES + 1),
                _ => {}
            }
            frame(&mut server, json!({"version":1,"id":2,"result":manifest})).await;
            let mut header = descriptor.imagebytes_header(2, 2);
            if fault == "header" {
                header[32..36].copy_from_slice(&(descriptor.width() + 1).to_le_bytes());
            }
            if fault == "transaction" {
                header[8..12].copy_from_slice(&3u32.to_le_bytes());
            }
            if fault == "serverTransaction" {
                header[12..16].copy_from_slice(&3u32.to_le_bytes());
            }
            if fault == "extension" {
                header[16..20].copy_from_slice(&45u32.to_le_bytes());
            }
            if server.write_all(&header).await.is_err() {
                return;
            }
            if fault == "extension" && server.write_all(&[0]).await.is_err() {
                return;
            }
            if fault == "hold" {
                assert_eq!(server.read(&mut [0; 1]).await.unwrap(), 0);
                return;
            }
            let payload = if fault == "truncated" {
                &payload[..payload.len() - 1]
            } else {
                &payload[..]
            };
            if server.write_all(payload).await.is_err() {
                return;
            }
            if fault == "trailing" {
                let _ = server.write_all(&[0xff]).await;
            }
        });
        (reader, instance, task, pixels)
    }

    #[tokio::test]
    async fn dedicated_reader_preserves_all_nine_element_types_and_rank_three_one_plane() {
        for element_type in [
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
            let descriptor =
                ImageDescriptor::new(3, 2, Some(1), element_type, element_type, ImageOrder::Ascom)
                    .unwrap();
            let request = request();
            let budget = ImageBudget::new(1024).unwrap();
            let (reader, instance, task, pixels) = fake(request, descriptor, "").await;
            let downloaded =
                download_from_stream(reader, instance, request, &budget, Duration::from_secs(2))
                    .await
                    .unwrap();
            task.await.unwrap();
            assert_eq!(downloaded.image.descriptor(), descriptor);
            assert_eq!(downloaded.image.bytes(), pixels);
            assert_eq!(budget.used_bytes(), pixels.len());
            drop(downloaded);
            assert_eq!(budget.used_bytes(), 0);
        }
    }

    #[tokio::test]
    async fn invalid_manifest_header_and_incomplete_bodies_never_publish_or_retain_pixels() {
        for fault in [
            "identity",
            "extra",
            "length",
            "order",
            "chunk",
            "header",
            "transaction",
            "serverTransaction",
            "extension",
            "truncated",
            "trailing",
        ] {
            let descriptor = ImageDescriptor::new(
                3,
                2,
                None,
                ElementType::Int32,
                ElementType::UInt16,
                ImageOrder::Ascom,
            )
            .unwrap();
            let request = request();
            let budget = ImageBudget::new(1024).unwrap();
            let (reader, instance, task, _) = fake(request, descriptor, fault).await;
            assert!(
                matches!(
                    download_from_stream(
                        reader,
                        instance,
                        request,
                        &budget,
                        Duration::from_secs(2)
                    )
                    .await,
                    Err(ClientError::Protocol | ClientError::Disconnected)
                ),
                "{fault}"
            );
            task.await.unwrap();
            assert_eq!(budget.used_bytes(), 0, "{fault}");
        }
    }

    #[test]
    fn transfer_scratch_is_reserved_before_allocation_and_preserves_order_for_every_type() {
        for element_type in [
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
            let descriptor = ImageDescriptor::new(
                3,
                2,
                Some(3),
                element_type,
                element_type,
                ImageOrder::SensorRows,
            )
            .unwrap();
            let budget = ImageBudget::new(descriptor.byte_len() * 2).unwrap();
            let mut allocation = budget.allocate(descriptor).unwrap();
            for (i, value) in allocation.bytes_mut().iter_mut().enumerate() {
                *value = (i * 19) as u8;
            }
            let image = allocation.finish();
            let mut buffer = image.transfer_buffer().unwrap();
            assert_eq!(budget.used_bytes(), descriptor.byte_len() * 2);
            assert_eq!(
                buffer.fill(&image, 0),
                image.imagebytes_chunk(0, IMAGE_CHUNK_BYTES).unwrap()
            );
            assert!(matches!(image.transfer_buffer(),Err(error) if error.kind==ErrorKind::Busy));
            drop(buffer);
            assert_eq!(budget.used_bytes(), descriptor.byte_len());
            drop(image);
            assert_eq!(budget.used_bytes(), 0);
        }
    }

    #[tokio::test]
    async fn reader_deadline_closes_the_stream_and_releases_partial_allocation() {
        let descriptor = ImageDescriptor::new(
            3,
            2,
            None,
            ElementType::UInt16,
            ElementType::UInt16,
            ImageOrder::Ascom,
        )
        .unwrap();
        let request = request();
        let budget = ImageBudget::new(1024).unwrap();
        let (reader, instance, task, _) = fake(request, descriptor, "hold").await;
        assert!(matches!(
            download_from_stream(
                reader,
                instance,
                request,
                &budget,
                Duration::from_millis(500)
            )
            .await,
            Err(ClientError::Timeout)
        ));
        task.await.unwrap();
        assert_eq!(budget.used_bytes(), 0);
    }

    #[tokio::test]
    async fn binary_contract_mismatch_is_rejected_before_even_a_small_receiver_budget() {
        for fault in ["header", "serverTransaction", "extension"] {
            let descriptor = ImageDescriptor::new(
                3,
                2,
                None,
                ElementType::UInt16,
                ElementType::UInt16,
                ImageOrder::Ascom,
            )
            .unwrap();
            let request = request();
            let budget = ImageBudget::new(1).unwrap();
            let (reader, instance, task, _) = fake(request, descriptor, fault).await;
            assert!(
                matches!(
                    download_from_stream(
                        reader,
                        instance,
                        request,
                        &budget,
                        Duration::from_secs(2)
                    )
                    .await,
                    Err(ClientError::Protocol)
                ),
                "{fault}"
            );
            task.await.unwrap();
            assert_eq!(budget.used_bytes(), 0);
        }
    }
}
