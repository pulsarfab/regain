using System.Numerics;
using System.Text.Json;
using NINA.Core.Enum;
using NINA.Image.FileFormat;
using NINA.Image.ImageData;
using NINA.Image.Interfaces;
using NINA.Profile.Interfaces;
using Regain.Hub;

namespace Regain.NINA;

/// Save exact member images through NINA's ordinary image factory and file writer.
/// The caller owns capture admission/reconciliation; this adapter never starts an exposure.
internal interface IHubCameraGroupImages
{
    FileSaveInfo SaveSettings();
    Task<string> SaveAsync(HubCameraGroups groups, Guid group, Guid operation, Guid configuredSource, FileSaveInfo settings, CancellationToken token);
}
internal sealed class HubCameraGroupImages(IImageDataFactory images, IProfileService profiles) : IHubCameraGroupImages
{
    private readonly bool signedPixels = profiles.ActiveProfile.CameraSettings.ASCOMCreate32BitData;
    public FileSaveInfo SaveSettings() => new(profiles);
    internal async Task<HubCameraExposureData> DownloadAsync(HubCameraGroups groups, Guid group, Guid operation,
        Guid configuredSource, CancellationToken token)
    {
        var status = await groups.StatusAsync(group, operation, token).ConfigureAwait(false);
        var bindings = status.GetProperty("bindings").EnumerateArray().ToArray();
        var index = Array.FindIndex(bindings, b => b.GetProperty("configuredSource").GetGuid() == configuredSource);
        if (index < 0 || status.GetProperty("result").ValueKind == JsonValueKind.Null) throw new IOException("No retained camera member result");
        var member = status.GetProperty("result").GetProperty("members")[index]; var profile = member.GetProperty("captureProfile");
        if (!member.GetProperty("requireScalarImage").GetBoolean() || profile.ValueKind == JsonValueKind.Null || member.GetProperty("image").ValueKind == JsonValueKind.Null)
            throw new IOException("This image was not admitted with frozen scalar camera metadata");
        var completed = member.GetProperty("image"); var geometry = completed.GetProperty("geometry");
        var sensor = (SensorType)profile.GetProperty("sensorType").GetInt32();
        var depth = 32 - BitOperations.LeadingZeroCount((uint)profile.GetProperty("maxAdu").GetInt32());
        var metadata = HubCameraFrames.Metadata(geometry, completed.GetProperty("exposure"), sensor,
            profile.GetProperty("bayerOffsetX").GetInt32(), profile.GetProperty("bayerOffsetY").GetInt32());
        metadata.Camera.Id = completed.GetProperty("source").GetString();
        metadata.Camera.Name = profile.GetProperty("sensorName").GetString() ?? metadata.Camera.Id;
        metadata.Image.ImageType = member.GetProperty("request").GetProperty("light").GetBoolean() ? "LIGHT" : "DARK";
        metadata.GenericHeaders.Add(new StringMetaDataHeader("RGNGROUP", group.ToString(), "Regain camera group"));
        metadata.GenericHeaders.Add(new StringMetaDataHeader("RGNOP", operation.ToString(), "Regain retained operation"));
        metadata.GenericHeaders.Add(new StringMetaDataHeader("RGNACQ", completed.GetProperty("acquisition").GetString()!, "Regain acquisition"));
        metadata.GenericHeaders.Add(new StringMetaDataHeader("RGNGEN", completed.GetProperty("generation").GetString()!, "Regain source generation"));
        using var image = await groups.DownloadAsync(group, operation, configuredSource, HubImageBudget.Shared, HubCameraImages.MaximumTransferTime, token).ConfigureAwait(false);
        if (image.Request.Source != completed.GetProperty("source").GetGuid() || image.Request.Generation != completed.GetProperty("generation").GetGuid() ||
            image.Request.Acquisition != completed.GetProperty("acquisition").GetGuid()) throw new IOException("Retained camera metadata differs from the image pin");
        var signed = depth > 16 || signedPixels;
        var pixels = HubCameraArrays.RowMajorIntegers(image, signed, cancellation: token);
        return new HubCameraExposureData(images, pixels, image.Descriptor.Width, image.Descriptor.Height, depth, sensor == SensorType.RGGB, metadata);
    }
    public async Task<string> SaveAsync(HubCameraGroups groups, Guid group, Guid operation, Guid configuredSource,
        FileSaveInfo settings, CancellationToken token)
    {
        var exposure = await DownloadAsync(groups, group, operation, configuredSource, token).ConfigureAwait(false);
        var image = await exposure.ToImageData(null!, token).ConfigureAwait(false);
        return await image.SaveToDisk(settings, token).ConfigureAwait(false);
    }
}
