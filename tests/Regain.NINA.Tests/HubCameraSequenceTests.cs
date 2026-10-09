using System.Text.Json;
using System.Windows;
using Moq;
using Newtonsoft.Json;
using NINA.Core.Enum;
using NINA.Image.FileFormat;
using NINA.Image.ImageAnalysis;
using NINA.Image.ImageData;
using NINA.Image.Interfaces;
using NINA.Profile.Interfaces;
using NINA.Sequencer;
using NINA.Sequencer.SequenceItem;
using NINA.Sequencer.Serialization;
using Regain.Hub;
using Xunit;
using JsonSerializer = System.Text.Json.JsonSerializer;

namespace Regain.NINA.Tests;

public sealed partial class HubNativeTests
{
    private sealed class RecordingGroupImages(string root) : IHubCameraGroupImages
    {
        internal readonly List<(Guid Source, string Path, string Pattern)> Saved = [];
        internal Func<int, CancellationToken, Task>? BeforeSave;
        public FileSaveInfo SaveSettings() => GroupFileSettings(root);
        public async Task<string> SaveAsync(HubCameraGroups groups, Guid group, Guid operation, Guid source, FileSaveInfo settings, CancellationToken token)
        {
            Saved.Add((source, settings.FilePath, settings.FilePattern));
            if (BeforeSave is not null) await BeforeSave(Saved.Count, token);
            using var image = await groups.DownloadAsync(group, operation, source, new HubImageBudget(1024 * 1024), TimeSpan.FromSeconds(10), token);
            Directory.CreateDirectory(settings.FilePath); var file = Path.Combine(settings.FilePath, settings.FilePattern + ".fixture");
            await File.WriteAllTextAsync(file, image.Request.Acquisition.ToString(), token); return file;
        }
    }
    private static FileSaveInfo GroupFileSettings(string root)
    {
        var files = new Mock<IImageFileSettings>(); files.SetupGet(s => s.FileType).Returns(FileTypeEnum.FITS);
        files.SetupGet(s => s.FilePath).Returns(root); files.SetupGet(s => s.FITSUseLegacyWriter).Returns(true);
        var profiles = new Mock<IProfileService>(); profiles.Setup(p => p.ActiveProfile.ImageFileSettings).Returns(files.Object);
        return new FileSaveInfo(profiles.Object);
    }
    private static CaptureHubCameraGroup CameraStep(Host host, IHubCameraGroupImages writer, Action? attached = null)
    {
        var step = new CaptureHubCameraGroup((path, instance, token) => { attached?.Invoke(); return HubCameraGroups.AttachAsync(host.Executable, path, instance, token); }, () => writer);
        step.SelectGroup(host.ConfigPath, host.Client.Hello.InstanceId,
            JsonSerializer.SerializeToElement(host.Config["cameraGroups"]![0]));
        step.SaveDirectory = host.DirectoryPath;
        step.Members[0].DurationSeconds = 0.1; step.Members[1].DurationSeconds = 0.3; step.Members[1].Light = false;
        return step;
    }
    private static CaptureHubCameraGroup RestoreCameraStep(CaptureHubCameraGroup step, Func<CaptureHubCameraGroup> fresh)
    {
        // NINA resolves exported instructions through its factory, then populates
        // saved settings. Exercise that real converter, including populated lists.
        var factory = new Mock<ISequencerFactory>(); factory.Setup(f => f.GetItem<CaptureHubCameraGroup>()).Returns(fresh);
        var converter = new SequenceItemCreationConverter(factory.Object, new SequenceContainerCreationConverter(factory.Object));
        var json = JsonConvert.SerializeObject(step, new JsonSerializerSettings { TypeNameHandling = TypeNameHandling.All });
        return Assert.IsType<CaptureHubCameraGroup>(JsonConvert.DeserializeObject<ISequenceItem>(json,
            new JsonSerializerSettings { Converters = [converter] }));
    }
    [Fact]
    public async Task CameraSequenceUsesTheRealNinaWriterForEachSeparateMember()
    {
        await using var host = await Host.Open(PairedCameras);
        var (writer, _) = RealGroupImages(false); var step = CameraStep(host, writer);
        await step.Execute(new GroupProgress(_ => { }), CancellationToken.None);
        Assert.False(step.ReconciliationRequired);
        var files = Directory.GetFiles(host.DirectoryPath, "*.fits", SearchOption.AllDirectories); Assert.Equal(2, files.Length);
        Assert.Equal(Path.GetDirectoryName(files[0]), Path.GetDirectoryName(files[1]));
        using var groups = await HubCameraGroups.AttachAsync(host.Executable, host.ConfigPath, host.Client.Hello.InstanceId);
        var result = await groups.StatusAsync(step.GroupId); var operation = result.GetProperty("operation").GetGuid();
        Assert.All(files, file => Assert.Equal("regain-" + operation.ToString("N"), Path.GetFileName(Path.GetDirectoryName(file))));
        foreach (var member in result.GetProperty("result").GetProperty("members").EnumerateArray()) {
            var name = member.GetProperty("source").GetGuid().ToString("N") + "-" + member.GetProperty("acquisition").GetGuid().ToString("N");
            var file = Assert.Single(files, f => Path.GetFileNameWithoutExtension(f) == name);
            Assert.StartsWith("SIMPLE", System.Text.Encoding.ASCII.GetString(await File.ReadAllBytesAsync(file)));
            Assert.Contains("saved " + file, step.Outcome);
        }
    }
    [Fact]
    public async Task CameraSequenceNeverInventsMissingExposureMetadataAndStillSavesItsHealthySibling()
    {
        await using var host = await Host.Open(PairedCameras);
        await host.Update(0, new { camera = new { exposureMetadataAvailable = false } });
        var (writer, _) = RealGroupImages(false); var step = CameraStep(host, writer);
        await Assert.ThrowsAsync<IOException>(() => step.Execute(new GroupProgress(_ => { }), CancellationToken.None));
        Assert.True(step.ReconciliationRequired); Assert.Contains("save failed", step.Outcome);
        Assert.Single(Directory.GetFiles(host.DirectoryPath, "*.fits", SearchOption.AllDirectories));
        using var groups = await HubCameraGroups.AttachAsync(host.Executable, host.ConfigPath, host.Client.Hello.InstanceId);
        var result = await groups.StatusAsync(step.GroupId); Assert.Equal("complete", result.GetProperty("phase").GetString());
        Assert.NotEqual(JsonValueKind.Null, result.GetProperty("result").GetProperty("members")[0].GetProperty("image").GetProperty("exposure").GetProperty("startTimeError").ValueKind);
    }
    [Fact]
    public async Task CameraSequenceCapturesSeparateFramesFreezesInputsAndDeepClonesSavedSettings()
    {
        await using var host = await Host.Open(PairedCameras);
        var writer = new RecordingGroupImages(host.DirectoryPath); var step = CameraStep(host, writer);
        var statuses = new List<string>();
        await step.Execute(new GroupProgress(value => {
            statuses.Add(value);
            if (value.StartsWith("Attaching")) { step.Members[0].DurationSeconds = 9; step.SaveDirectory = "changed-after-admission"; }
        }), CancellationToken.None);
        Assert.False(step.ReconciliationRequired); Assert.Contains("Group complete", step.Outcome); Assert.Equal(2, writer.Saved.Count);
        Assert.Equal(writer.Saved[0].Path, writer.Saved[1].Path); Assert.NotEqual(writer.Saved[0].Pattern, writer.Saved[1].Pattern);
        Assert.All(writer.Saved, saved => Assert.StartsWith(host.DirectoryPath, saved.Path));
        // The first configured member is an alias; filenames use the physical
        // source identity while SaveAsync is addressed by the configured ID.
        Assert.Contains(host.Config["sources"]![0]!["id"]!.GetValue<Guid>().ToString("N"), writer.Saved[0].Pattern);
        Assert.Equal(2, Directory.GetFiles(host.DirectoryPath, "*.fixture", SearchOption.AllDirectories).Length);
        using var groups = await HubCameraGroups.AttachAsync(host.Executable, host.ConfigPath, host.Client.Hello.InstanceId);
        var result = await groups.StatusAsync(step.GroupId); var requests = result.GetProperty("requests");
        Assert.Equal(0.1, requests[0].GetProperty("exposure").GetProperty("durationSeconds").GetDouble());
        Assert.False(requests[1].GetProperty("exposure").GetProperty("light").GetBoolean());
        Assert.All(requests.EnumerateArray(), r => Assert.True(r.GetProperty("requireScalarImage").GetBoolean()));
        var clone = Assert.IsType<CaptureHubCameraGroup>(step.Clone()); clone.Members[0].DurationSeconds = 5;
        Assert.Equal(9, step.Members[0].DurationSeconds); Assert.NotSame(step.Members, clone.Members);
        var json = JsonConvert.SerializeObject(step); Assert.DoesNotContain("Outcome", json); Assert.DoesNotContain("captureProfile", json);
        var restored = RestoreCameraStep(step, () => CameraStep(host, writer));
        Assert.Equal(step.GroupId, restored.GroupId); Assert.Equal(step.Members.Count, restored.Members.Count);
        await Wpf(() => { Assert.IsType<DataTemplate>(new HubSequenceTemplates()[new DataTemplateKey(typeof(CaptureHubCameraGroup))]); return Task.CompletedTask; });
    }
    [Theory, InlineData(false), InlineData(true)]
    public async Task CameraSequenceSavesHealthyMembersAndFencesPartialCaptureOrSaveFailure(bool saveFailure)
    {
        await using var host = await Host.Open(PairedCameras);
        var writer = new RecordingGroupImages(host.DirectoryPath); var attachments = 0; var step = CameraStep(host, writer, () => attachments++);
        if (saveFailure) writer.BeforeSave = (index, _) => index == 1 ? Task.FromException(new IOException("Injected disk failure")) : Task.CompletedTask;
        else await host.Update(0, new { fault = "imageError" });
        await Assert.ThrowsAsync<IOException>(() => step.Execute(new GroupProgress(_ => { }), CancellationToken.None));
        Assert.True(step.ReconciliationRequired); Assert.Contains("saved", step.Outcome);
        Assert.Single(Directory.GetFiles(host.DirectoryPath, "*.fixture", SearchOption.AllDirectories));
        Assert.Equal(step.Members[1].Source, writer.Saved.Last().Source);
        var before = attachments;
        await Assert.ThrowsAsync<InvalidOperationException>(() => step.Execute(new GroupProgress(_ => { }), CancellationToken.None)); Assert.Equal(before, attachments);
        var clone = Assert.IsType<CaptureHubCameraGroup>(step.Clone()); Assert.True(clone.ReconciliationRequired);
        var restored = RestoreCameraStep(step, () => CameraStep(host, writer));
        Assert.True(restored.ReconciliationRequired); Assert.False(restored.Validate());
        step.SelectGroup(host.ConfigPath, host.Client.Hello.InstanceId, JsonSerializer.SerializeToElement(host.Config["cameraGroups"]![0]));
        Assert.True(step.ReconciliationRequired);
        if (!saveFailure) {
            // An upstream image failure retains ordinary acquisition ownership.
            // A sequence fence reset cannot authorize another client's control.
            var busy = await Assert.ThrowsAsync<HubException>(() => host.Update(0, new { fault = "none" }));
            Assert.Equal("busy", busy.Remote!.Code); return;
        }
        step.AllowNewOperationAfterInspection(); writer.BeforeSave = null;
        await step.Execute(new GroupProgress(_ => { }), CancellationToken.None); Assert.False(step.ReconciliationRequired);
    }
    [Theory, InlineData("abortStarted"), InlineData("leaveRunning")]
    public async Task CameraSequenceCancellationUsesSavedPolicyAndDoesNotAutomaticallyReplay(string policy)
    {
        await using var host = await Host.Open(config => { PairedCameras(config); config["cameraGroups"]![0]!["cancellationPolicy"] = policy; });
        // Keep both exposures pending until the saved cancellation policy acts.
        // A short wall-clock exposure can finish while a busy CI client observes
        // the exposing state and checks the no-replay fences.
        await host.Update(0, new { fault = "stalledExposure" });
        await host.Update(1, new { fault = "stalledExposure" });
        var writer = new RecordingGroupImages(host.DirectoryPath); var step = CameraStep(host, writer);
        foreach (var member in step.Members) member.DurationSeconds = 2;
        using var cancel = new CancellationTokenSource();
        var running = step.Execute(new GroupProgress(_ => { }), cancel.Token);
        await Eventually(() => Task.FromResult(step.Outcome.Contains("exposing")));
        Assert.Throws<InvalidOperationException>(() => step.AllowNewOperationAfterInspection());
        await Assert.ThrowsAsync<InvalidOperationException>(() => step.Execute(new GroupProgress(_ => { }), CancellationToken.None));
        cancel.Cancel(); await Assert.ThrowsAnyAsync<OperationCanceledException>(() => running);
        Assert.True(step.ReconciliationRequired); Assert.Contains("saved group policy", step.Outcome); Assert.Empty(writer.Saved);
        using var groups = await HubCameraGroups.AttachAsync(host.Executable, host.ConfigPath, host.Client.Hello.InstanceId); JsonElement result = default;
        await Eventually(async () => { result = await groups.StatusAsync(step.GroupId); return HubCameraGroups.Terminal(result); });
        Assert.Equal("cancelled", result.GetProperty("phase").GetString());
        Assert.All(result.GetProperty("result").GetProperty("members").EnumerateArray(), member =>
            Assert.Equal(policy == "abortStarted" ? "aborted" : "exposing", member.GetProperty("phase").GetString()));
    }
    [Fact]
    public async Task CameraSequenceCancellationDuringSaveKeepsExistingFilesAndCompletedOperation()
    {
        await using var host = await Host.Open(PairedCameras);
        var writer = new RecordingGroupImages(host.DirectoryPath); var step = CameraStep(host, writer); using var cancel = new CancellationTokenSource();
        writer.BeforeSave = (index, _) => { if (index == 2) cancel.Cancel(); return Task.CompletedTask; };
        await Assert.ThrowsAnyAsync<OperationCanceledException>(() => step.Execute(new GroupProgress(_ => { }), cancel.Token));
        Assert.True(step.ReconciliationRequired); Assert.Contains("Saving was interrupted", step.Outcome);
        Assert.Single(Directory.GetFiles(host.DirectoryPath, "*.fixture", SearchOption.AllDirectories));
        using var groups = await HubCameraGroups.AttachAsync(host.Executable, host.ConfigPath, host.Client.Hello.InstanceId);
        Assert.Equal("complete", (await groups.StatusAsync(step.GroupId)).GetProperty("phase").GetString());
    }
    [Fact]
    public async Task CameraSequenceRejectsChangedMembershipAndUnsupportedScalarFormatBeforeAnyCapture()
    {
        await using var host = await Host.Open(PairedCameras);
        var writer = new RecordingGroupImages(host.DirectoryPath); var step = CameraStep(host, writer);
        (step.Members[0], step.Members[1]) = (step.Members[1], step.Members[0]);
        await Assert.ThrowsAsync<InvalidOperationException>(() => step.Execute(new GroupProgress(_ => { }), CancellationToken.None));
        Assert.False(step.ReconciliationRequired); Assert.Empty(writer.Saved);
        step = CameraStep(host, writer); var output = host.Config["outputs"]![0]!["id"]!.GetValue<Guid>();
        await host.Command(new { op = "connect", output });
        await host.Command(new { op = "put", output, property = HubCameraProtocol.Setting(HubCameraProperty.ReadoutMode, 1) });
        await host.Command(new { op = "disconnect", output });
        await Assert.ThrowsAsync<IOException>(() => step.Execute(new GroupProgress(_ => { }), CancellationToken.None));
        Assert.True(step.ReconciliationRequired); Assert.Contains("preflightFailed", step.Outcome); Assert.Empty(writer.Saved);
        using var groups = await HubCameraGroups.AttachAsync(host.Executable, host.ConfigPath, host.Client.Hello.InstanceId);
        var result = await groups.StatusAsync(step.GroupId);
        Assert.All(result.GetProperty("result").GetProperty("members").EnumerateArray(), member => Assert.Null(member.GetProperty("acquisition").GetString()));
    }
    [Theory, InlineData(false), InlineData(true)]
    public async Task CameraSequenceUnknownStartNeverGuessesAnOperationOrReplays(bool cancelled)
    {
        await using var host = await Host.Open(PairedCameras);
        using var cancel = new CancellationTokenSource(); var calls = new List<string>();
        using var description = JsonDocument.Parse(File.ReadAllText(Path.Combine(AppContext.BaseDirectory, "hub-config.json")));
        using var groups = new HubCameraGroups(Guid.NewGuid(), description.RootElement.GetProperty("coordination").GetProperty("cameraGroups"),
            JsonSerializer.SerializeToElement(host.Config), (command, _) => {
                calls.Add(command.GetProperty("op").GetString()!);
                if (cancelled) { cancel.Cancel(); throw new OperationCanceledException(cancel.Token); }
                throw new HubException(HubFailure.Uncertain);
            }, () => { });
        var writer = new RecordingGroupImages(host.DirectoryPath);
        var step = new CaptureHubCameraGroup((_, _, _) => Task.FromResult(groups), () => writer);
        step.SelectGroup(host.ConfigPath, host.Client.Hello.InstanceId, JsonSerializer.SerializeToElement(host.Config["cameraGroups"]![0]));
        if (cancelled) await Assert.ThrowsAnyAsync<OperationCanceledException>(() => step.Execute(new GroupProgress(_ => { }), cancel.Token));
        else await Assert.ThrowsAsync<HubException>(() => step.Execute(new GroupProgress(_ => { }), cancel.Token));
        Assert.True(step.ReconciliationRequired); Assert.Equal(new[] { "startCameraGroup" }, calls); Assert.Empty(writer.Saved);
        await Assert.ThrowsAsync<InvalidOperationException>(() => step.Execute(new GroupProgress(_ => { }), CancellationToken.None));
        Assert.Single(calls);
        Assert.True(RestoreCameraStep(step, () => CameraStep(host, writer)).ReconciliationRequired);
    }
}
