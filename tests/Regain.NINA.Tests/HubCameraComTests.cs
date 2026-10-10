using System.Diagnostics;
using System.Text.Json;
using Moq;
using NINA.Core.Model.Equipment;
using NINA.Equipment.Model;
using NINA.Image.Interfaces;
using NINA.Profile.Interfaces;
using Regain.Hub;
using Xunit;

namespace Regain.NINA.Tests;

// Normal NINA runs explicitly skip registered fixtures. The COM test script
// supplies private registration/paths, runs these cases, then removes its keys.
public sealed class RegisteredCameraTheoryAttribute : TheoryAttribute
{
    public RegisteredCameraTheoryAttribute() {
        if (Environment.GetEnvironmentVariable("REGAIN_HUB_COM_FIXTURE_PROGID") is null)
            Skip = "Run scripts/test-hub-com.ps1 for private registered camera imports";
    }
}
public sealed class HubCameraComTests
{
    [RegisteredCameraTheory]
    [InlineData("x86", "uInt16", 0)] [InlineData("x64", "uInt16", 0)]
    [InlineData("x86", "int32", 1)] [InlineData("x64", "int32", 1)]
    [InlineData("x86", "single", 0)] [InlineData("x64", "single", 0)]
    [InlineData("x86", "uInt16", 3)] [InlineData("x64", "uInt16", 3)]
    public async Task RegisteredCameraImportsRetainExactPixelsOrRejectUnrepresentableFrames(string bitness, string type, int planes)
    {
        var fixtureRoot = Path.GetFullPath(Environment.GetEnvironmentVariable("REGAIN_HUB_COM_FIXTURE_DIRECTORY")!);
        var prefix = Environment.GetEnvironmentVariable("REGAIN_HUB_COM_FIXTURE_PROGID")!;
        Assert.StartsWith("ASCOM.Rgn.F.", prefix);
        var progId = prefix + ".Camera";
        var state = Path.Combine(fixtureRoot, progId + ".json");
        var trace = state + ".trace";
        File.Delete(trace);
        await File.WriteAllTextAsync(state, JsonSerializer.Serialize(new { version = 4, imageType = type, imagePlanes = planes, imageLowerBounds = true }));
        var workers = Path.GetFullPath(Environment.GetEnvironmentVariable("REGAIN_TEST_WORKERS")!);
        var executable = Path.Combine(workers, "regain-alpaca.exe");
        var directory = Path.Combine(fixtureRoot, "nina-" + Guid.NewGuid().ToString("N")); Directory.CreateDirectory(directory);
        var configPath = Path.Combine(directory, "camera.json");
        var instance = Guid.NewGuid(); var source = Guid.NewGuid(); var first = Guid.NewGuid(); var second = Guid.NewGuid();
        await File.WriteAllTextAsync(configPath, JsonSerializer.Serialize(new {
            schemaVersion = 1, instanceId = instance, revision = Guid.NewGuid(),
            sources = new[] { new { id = source, label = "Private registered COM camera", backend = new {
                kind = "com", progId, deviceType = "camera", bitness, connectionPolicy = "managed" },
                polling = new { pollSeconds = 60, requestTimeoutSeconds = 3, connectionTimeoutSeconds = 10 } } },
            outputs = new[] { new { id = first, number = 4, label = "Private camera A", device = new { kind = "proxy", source, deviceType = "camera" } },
                new { id = second, number = 7, label = "Private camera B", device = new { kind = "proxy", source, deviceType = "camera" } } }
        }));
        using var process = new Process { StartInfo = new ProcessStartInfo(executable) { UseShellExecute = false, CreateNoWindow = true,
            WindowStyle = ProcessWindowStyle.Hidden, RedirectStandardOutput = true, RedirectStandardError = true } };
        foreach (var argument in new[] { "--hub-host", "--hub-config", configPath, "--workers", workers, "--simulate" }) process.StartInfo.ArgumentList.Add(argument);
        Assert.True(process.Start()); var errors = process.StandardError.ReadToEndAsync(); Task<string>? stdout = null;
        using var deadline = new CancellationTokenSource(TimeSpan.FromSeconds(30));
        try {
            Assert.StartsWith("Regain hub ready:", await process.StandardOutput.ReadLineAsync(deadline.Token)); stdout = process.StandardOutput.ReadToEndAsync();
            var attachment = await HubAttachment.AttachAsync(executable, configPath, workers, deadline.Token, instance);
            Assert.Null(attachment.StartedProcessId);
            using var control = await HubClient.ConnectAsync(attachment, cancellation: deadline.Token);
            Assert.False((await control.GetCameraCaptureTimingAsync(first, 0.01, deadline.Token)).Native);
            HubSelection Binding(Guid id) => new() { ConfigPath = configPath, InstanceId = instance, OutputId = id, DeviceType = "camera", Label = "Private COM camera" };
            var settings = new Mock<ICameraSettings>(); settings.SetupProperty(value => value.Timeout, 1);
            settings.SetupGet(value => value.ASCOMCreate32BitData).Returns(type == "int32");
            var profiles = new Mock<IProfileService>(); profiles.Setup(value => value.ActiveProfile.CameraSettings).Returns(settings.Object);
            using var owner = new HubCameraDevice(Binding(first), Mock.Of<IImageDataFactory>(), profiles.Object, executable, workers);
            using var sibling = new HubCameraDevice(Binding(second), Mock.Of<IImageDataFactory>(), executable: executable, workers: workers);
            await owner.Connect(deadline.Token); await sibling.Connect(deadline.Token);
            owner.EnableSubSample = true; owner.SubSampleWidth = 96; owner.SubSampleHeight = 64;
            owner.StartExposure(new CaptureSequence { ExposureTime = 0.01, Binning = new BinningMode(1, 1), Gain = -1, Offset = -1 });
            await owner.WaitUntilExposureIsReady(deadline.Token);
            if (type == "single" || planes == 3) {
                await Assert.ThrowsAsync<NotSupportedException>(() => owner.DownloadExposure(deadline.Token));
            } else {
                var frame = Assert.IsType<HubCameraExposureData>(await owner.DownloadExposure(deadline.Token));
                Assert.Equal(96, frame.Width); Assert.Equal(64, frame.Height);
                if (type == "int32") {
                    var values = Assert.IsType<int[]>(frame.Pixels);
                    Assert.Equal(-1, values[0]); Assert.Equal(-65, values[1]); Assert.Equal(-6144, values[^1]);
                } else {
                    var values = Assert.IsType<ushort[]>(frame.Pixels);
                    Assert.Equal(40001, values[0]); Assert.Equal(40065, values[1]); Assert.Equal(46144, values[^1]);
                }
                owner.Disconnect(); Assert.True(sibling.Connected); Assert.Equal(6144, frame.Pixels.Length);
            }
            Assert.Equal(1, settings.Object.Timeout);
            // End both source leases before reading the completed trace. A live
            // sibling can still append polling calls while File.ReadLines opens.
            owner.Disconnect(); sibling.Disconnect();
            var calls = File.ReadLines(trace).Select(line => JsonDocument.Parse(line)).ToArray();
            try {
                Assert.Single(calls, call => call.RootElement.GetProperty("member").GetString() == "StartExposure");
                Assert.DoesNotContain(calls, call => call.RootElement.GetProperty("member").GetString() is "AbortExposure" or "StopExposure");
                Assert.All(calls.Where(call => call.RootElement.GetProperty("member").GetString() == "Activate"),
                    call => Assert.Equal(bitness == "x86" ? 32 : 64, call.RootElement.GetProperty("bitness").GetInt32()));
            } finally { foreach (var call in calls) call.Dispose(); }
        } finally {
            if (!process.HasExited) { process.Kill(); await process.WaitForExitAsync(); }
            if (stdout is not null) await stdout; Console.WriteLine(await errors);
            Assert.StartsWith(fixtureRoot + Path.DirectorySeparatorChar, Path.GetFullPath(directory), StringComparison.OrdinalIgnoreCase);
            Directory.Delete(directory, true);
        }
    }
}
