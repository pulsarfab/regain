// Test-only installed-NINA harness, inert unless its explicit directory is supplied.
using System.ComponentModel.Composition;
using System.Reflection;
using System.IO;
using NINA.Core.Enum;
using System.Runtime.InteropServices;
using System.Text.Json;
using System.Windows;
using NINA.Core.Model.Equipment;
using NINA.Equipment.Interfaces;
using NINA.Equipment.Interfaces.Mediator;
using NINA.Equipment.Interfaces.ViewModel;
using NINA.Equipment.Model;
using NINA.Image.FileFormat;
using NINA.Plugin;
using NINA.Plugin.Interfaces;

[assembly: Guid("9b79123f-423d-47ad-b7d4-1a3cd94ac9f0")]
[assembly: AssemblyMetadata("MinimumApplicationVersion", "3.2.0.9001")]

[Export(typeof(IPluginManifest))]
public sealed class AcceptancePlugin : PluginBase
{
    private readonly ICameraMediator camera;
    private readonly IFilterWheelMediator wheel;
    private readonly IRotatorMediator rotator;
    private readonly IImagingMediator imaging;
    private readonly CancellationTokenSource lifetime = new();
    private Task? work;
    [ImportingConstructor]
    public AcceptancePlugin(ICameraMediator camera, IFilterWheelMediator wheel, IRotatorMediator rotator, IImagingMediator imaging)
    { this.camera = camera; this.wheel = wheel; this.rotator = rotator; this.imaging = imaging; }

    public override Task Initialize()
    {
        string? root = Environment.GetEnvironmentVariable("REGAIN_NINA_OPTICAL_ACCEPTANCE");
        if (root is not null) work = Task.Run(async () => {
            try { await Run(root); } catch (OperationCanceledException) { }
            catch (Exception error) { File.WriteAllText(Path.Combine(root, "harness-error.txt"), error.ToString()); }
        });
        return Task.CompletedTask;
    }
    private static async Task Select(object mediator, string id)
    {
        object? handler = null;
        for (var type = mediator.GetType(); type is not null; type = type.BaseType) {
            handler = type.GetField("handler", BindingFlags.Instance | BindingFlags.NonPublic)?.GetValue(mediator);
            if (handler is not null) break;
        }
        var chooser = handler?.GetType().GetProperty("DeviceChooserVM")?.GetValue(handler) as IDeviceChooserVM
            ?? throw new Exception("NINA chooser unavailable");
        await Application.Current.Dispatcher.InvokeAsync(() => chooser.SelectedDevice = chooser.Devices.Single(device => device.Id == id));
    }
    private async Task Run(string root)
    {
        Directory.CreateDirectory(root);
        File.WriteAllText(Path.Combine(root, "ready"), DateTimeOffset.UtcNow.ToString("O"));
        while (!lifetime.IsCancellationRequested) {
            await Task.Delay(250, lifetime.Token);
            string path = Path.Combine(root, "request.json");
            if (!File.Exists(path)) continue;
            JsonDocument parsed;
            try { parsed = JsonDocument.Parse(File.ReadAllText(path)); }
            catch (IOException) { continue; }
            catch (JsonException) { continue; }
            using var request = parsed; File.Delete(path);
            string id = request.RootElement.GetProperty("id").GetString()!;
            object response;
            try {
                switch (request.RootElement.GetProperty("command").GetString()) {
                    case "connect-qhy":
                        await wheel.Rescan(); await Select(wheel, request.RootElement.TryGetProperty("usbWheel",out var usbWheel) && usbWheel.GetBoolean() ? "Regain.CFW" : "Regain.QHY.CFW");
                        await camera.Rescan(); await Select(camera, "ZwoGain");
                        bool cameraFirst = request.RootElement.TryGetProperty("cameraFirst",out var order) && order.GetBoolean();
                        if (cameraFirst && !await camera.Connect()) throw new Exception("Camera connect failed");
                        if (!await wheel.Connect()) throw new Exception("QHY wheel connect failed");
                        if (!cameraFirst && !await camera.Connect()) throw new Exception("Camera connect failed");
                        response = new { id, ok = true, camera = camera.GetDevice().Name, wheel = wheel.GetDevice().Name, wheelInfo = wheel.GetDevice().DriverInfo,
                            slots = ((IFilterWheel)wheel.GetDevice()).Names.Length, position = ((IFilterWheel)wheel.GetDevice()).Position }; break;
                    case "move-wheel":
                        int target = request.RootElement.GetProperty("position").GetInt32();
                        await wheel.ChangeFilter(((IFilterWheel)wheel.GetDevice()).Filters[target],lifetime.Token,null);
                        if (((IFilterWheel)wheel.GetDevice()).Position != target) throw new Exception("Wrong filter readback");
                        response = new { id, ok = true, position = ((IFilterWheel)wheel.GetDevice()).Position }; break;
                    case "wheel-state":
                        response = new { id, ok = true, connected = wheel.GetDevice().Connected, position = ((IFilterWheel)wheel.GetDevice()).Position,
                            diagnostics = wheel.GetDevice().Action(wheel.GetDevice().Id == "Regain.CFW" ? "Regain.Status" : "Regain.Diagnostics","") }; break;
                    case "disconnect-camera":
                        await camera.Disconnect(); response = new { id, ok = true, wheelConnected = wheel.GetDevice().Connected, wheelPosition = ((IFilterWheel)wheel.GetDevice()).Position }; break;
                    case "connect-camera":
                        if (!await camera.Connect()) throw new Exception("Camera connect failed"); response = new { id, ok = true }; break;
                    case "connect-wheel":
                        if (!await wheel.Connect()) throw new Exception("Wheel connect failed"); response = new { id, ok = true, position = ((IFilterWheel)wheel.GetDevice()).Position }; break;
                    case "capture-reject-wheel":
                        var pending = imaging.CaptureImage(new CaptureSequence { ExposureTime = 5, Binning = new BinningMode(1,1), Gain = 30, Offset = 30 },lifetime.Token,null,"Regain shared wheel guard acceptance");
                        await Task.Delay(1200,lifetime.Token);
                        string rejected;
                        try { ((IFilterWheel)wheel.GetDevice()).Position = 1; throw new Exception("Wheel move was accepted during exposure"); }
                        catch (InvalidOperationException error) { rejected = error.Message; }
                        var protectedImage = await (await pending).ToImageData();
                        var protectedPath = await protectedImage.SaveToDisk(new FileSaveInfo { FilePath = root, FilePattern = id, FileType = FileTypeEnum.FITS },lifetime.Token);
                        response = new { id, ok = true, rejected, saved = protectedPath, width = protectedImage.Properties.Width, height = protectedImage.Properties.Height }; break;
                    case "connect":
                        bool cameraOnly = Environment.GetEnvironmentVariable("REGAIN_NINA_OPTICAL_ACCEPTANCE_CAMERA_ONLY") == "1";
                        if (!cameraOnly && Environment.GetEnvironmentVariable("REGAIN_NINA_OPTICAL_ACCEPTANCE_NO_WHEEL") != "1") {
                            await wheel.Rescan(); await Select(wheel, "ZwoGain.EFW");
                            if (!await wheel.Connect()) throw new Exception("Wheel connect failed");
                        }
                        if (!cameraOnly) {
                            await rotator.Rescan(); await Select(rotator, "ZwoGain.CAA");
                            if (!await rotator.Connect()) throw new Exception("Rotator connect failed");
                        }
                        await camera.Rescan(); await Select(camera, "ZwoGain");
                        if (!await camera.Connect()) throw new Exception("Camera connect failed");
                        response = new { id, ok = true, camera = camera.GetDevice().Name, wheel = wheel.GetDevice()?.Name, rotator = rotator.GetDevice()?.Name }; break;
                    case "rotator-state":
                        var device = rotator.GetDevice() ?? throw new Exception("Rotator unavailable");
                        var state = device.GetType().GetMethod("ReadVerifiedOpticalState")?.Invoke(device, null)
                            ?? throw new Exception("Verified optical state unavailable");
                        var epoch = device.GetType().GetProperty("OpticalConnectionEpoch")?.GetValue(device);
                        response = new { id, ok = true, state, epoch, connected = device.Connected }; break;
                    case "capture":
                        var seq = new CaptureSequence { ExposureTime = 5, Binning = new BinningMode(1, 1), Gain = 100, Offset = 30 };
                        var exposure = await imaging.CaptureImage(seq, lifetime.Token, null, "Regain optical recovery acceptance");
                        var image = await exposure.ToImageData();
                        string saved = await image.SaveToDisk(new FileSaveInfo { FilePath = root, FilePattern = id, FileType = FileTypeEnum.FITS }, lifetime.Token);
                        response = new { id, ok = true, saved, width = image.Properties.Width, height = image.Properties.Height }; break;
                    case "reconnect-camera":
                        await camera.Disconnect(); if (!await camera.Connect()) throw new Exception("Camera reconnect failed");
                        response = new { id, ok = true }; break;
                    case "disconnect-wheel":
                        await wheel.Disconnect(); response = new { id, ok = true }; break;
                    case "disconnect":
                        await camera.Disconnect(); await rotator.Disconnect(); await wheel.Disconnect();
                        response = new { id, ok = true }; break;
                    default: throw new Exception("Unknown acceptance command");
                }
            } catch (Exception error) { response = new { id, ok = false, error = error.ToString() }; }
            File.WriteAllText(Path.Combine(root, id + ".json"), JsonSerializer.Serialize(response, new JsonSerializerOptions { WriteIndented = true }));
        }
    }
    public override async Task Teardown() { lifetime.Cancel(); if (work is not null) try { await work; } catch (OperationCanceledException) { } lifetime.Dispose(); }
}
