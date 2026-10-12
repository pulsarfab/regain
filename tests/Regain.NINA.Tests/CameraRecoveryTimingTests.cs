using Moq;
using NINA.Core.Model.Equipment;
using NINA.Equipment.Model;
using NINA.Image.Interfaces;
using NINA.Profile.Interfaces;
using Regain.Core;
using Xunit;

namespace Regain.NINA.Tests;

public sealed class CameraRecoveryTimingTests
{
    [Theory]
    [InlineData(false, false)]
    [InlineData(true, false)]
    [InlineData(true, true)]
    public async Task NinaLongExposureDeadlineIncludesAllPreArmAttemptsAndPeripheralRecovery(bool usbReset, bool peripherals)
    {
        string root = Path.GetFullPath(Path.Combine(AppContext.BaseDirectory, "../../../../../"));
        var settings = new Mock<ICameraSettings>();
        settings.SetupProperty(value => value.Timeout, 1);
        var profiles = new Mock<IProfileService>();
        profiles.Setup(value => value.ActiveProfile.CameraSettings).Returns(settings.Object);
        var options = new RecoveryOptions {
            // Opening starts both supervisor and worker processes. Keep the
            // normal command deadline for a cold CI host; this test asserts
            // the capture budget, not how quickly those processes start.
            MaxRetries = 0, PreExposureReconnectAttempts = 10,
            ReconnectDelaySeconds = .01, UsbResetAfterFailures = usbReset ? 1 : 0
        };
        var camera = new ResilientCamera(new("ZWO ASI6200MM Pro", 9576, 6388, false, 0, 3.76, 16, true, false, [1]),
            Mock.Of<IExposureDataFactory>(), () => new HostClient(Path.Combine(root, "target/debug/regain-alpaca.exe"),
                "unused", simulate: true, direct: true, supervised: true), options, profiles.Object,
            peripheralRecoveryFactory: peripherals
                ? () => new UsbPeripheralRecovery([], new(), _ => { }) : null);
        const double seconds = 600;
        try {
            Assert.True(await camera.Connect(default));
            camera.StartExposure(new CaptureSequence { ExposureTime = seconds, Binning = new BinningMode(1, 1) });
            // Even without counting any restoration control writes, all ten
            // permitted pre-arm attempts exceed the old NINA-only allowance.
            // Long-exposure replacement remains disabled throughout this test.
            double minimum = seconds + options.ExposureGraceSeconds + options.CoolingTimeoutSeconds +
                (options.ReadyFrameDownloadRetries + 1) * (options.DownloadTimeoutSeconds + options.ReconnectDelaySeconds) +
                (options.PreExposureReconnectAttempts - 1) *
                    (options.ReconnectDelaySeconds + 2 * options.CoolingTimeoutSeconds + 16 * options.CommandTimeoutSeconds) +
                (usbReset ? 150 + (peripherals ? 120 : 0) : 0);
            Assert.True(settings.Object.Timeout >= minimum,
                $"NINA timeout {settings.Object.Timeout} cannot cover the permitted recovery minimum {minimum}");
            camera.AbortExposure();
            await Assert.ThrowsAsync<IOException>(() => camera.WaitUntilExposureIsReady(default).WaitAsync(TimeSpan.FromSeconds(10)));
            Assert.Equal(1, settings.Object.Timeout);
        } finally { camera.Disconnect(); }
    }

    [Fact]
    public void SharedCaptureBudgetKeepsUsbAndPeripheralAllowancesWithoutPermittingLongExposureReplacement()
    {
        var descriptor = new CameraDescriptor("ZWO ASI6200MM Pro", 9576, 6388, false, 0, 3.76, 16, true, false, [1]);
        using var baseline = new CameraSession(descriptor, () => throw new InvalidOperationException("No process needed"),
            new() { MaxRetries = 0 });
        using var replacements = new CameraSession(descriptor, () => throw new InvalidOperationException("No process needed"),
            new() { MaxRetries = 3 });
        using var usb = new CameraSession(descriptor, () => throw new InvalidOperationException("No process needed"),
            new() { MaxRetries = 0, UsbResetAfterFailures = 1 });
        double normal = baseline.CaptureTimeoutSeconds(600);
        Assert.Equal(normal, replacements.CaptureTimeoutSeconds(600));
        Assert.Equal(normal + 150, usb.CaptureTimeoutSeconds(600));
        usb.RecoverUsbPeripherals = (_, _) => Task.CompletedTask;
        Assert.Equal(normal + 270, usb.CaptureTimeoutSeconds(600));
        Assert.True(replacements.CaptureTimeoutSeconds(.1) > baseline.CaptureTimeoutSeconds(.1));
    }
}
