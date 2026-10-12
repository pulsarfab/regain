using System.Text.Json;
using Regain.Rotator;
using Xunit;

namespace Regain.NINA.Tests;

public sealed partial class HubNativeTests
{
    private static string UsbSessionStatus(object[] targets, bool running = true) => JsonSerializer.Serialize(new {
        protocolVersion = 1, running, elevated = running, sessionScoped = true,
        expiresAtUnixSeconds = (ulong?)null, targets
    });

    [Fact]
    public async Task UsbSessionApprovalHasNoClockExpiryAndStillRequiresExactCamera()
    {
        string json = UsbSessionStatus([UsbTarget()]);
        var helper = new UsbRecoveryHelper(@"C:\Regain\regain-camera.exe", (_, _, _, _) =>
            Task.FromResult(new UsbRecoveryCommandResult(0, json)));
        var ready = await helper.CheckAsync(UsbSelection());
        Assert.True(ready.SessionScoped); Assert.True(ready.IsReady);
        Assert.Contains("supervisor restarts", ready.Summary);
        Assert.False((await helper.CheckAsync(UsbSelection(serial: "fedcba9876543210"))).IsReady);
        json = UsbStatus(UsbTarget(), expiry: 1);
        var legacy = await helper.CheckAsync(UsbSelection());
        Assert.False(legacy.SessionScoped); Assert.False(legacy.IsReady);
        Assert.Contains("expired", legacy.Summary);
        json = UsbStatus(UsbTarget());
        Assert.True((await helper.CheckAsync(UsbSelection())).IsReady);
    }

    [Fact]
    public async Task UsbSessionPreparationRequiresExplicitLifetimeAndScopedApproval()
    {
        string json = UsbSessionStatus([UsbTarget()]);
        var helper = new UsbRecoveryHelper(@"C:\Regain\regain-camera.exe", (_, args, _, _) =>
            Task.FromResult(new UsbRecoveryCommandResult(0, args.Contains("--usb-target") ? UsbToken(UsbTarget()) :
                json.Replace("\"targets\":", "\"approved\":true,\"targets\":"))));
        Assert.True((await helper.PrepareAsync(() => UsbSelection())).IsReady);
        foreach (string invalid in new[] {
            json.Replace("\"sessionScoped\":true", "\"sessionScoped\":false"),
            json.Replace("\"expiresAtUnixSeconds\":null", "\"expiresAtUnixSeconds\":253402300799"),
            json.Replace("\"sessionScoped\":true,", "") }) {
            json = invalid;
            await Assert.ThrowsAsync<InvalidOperationException>(() => helper.CheckAsync(UsbSelection()));
        }
    }

    [Fact]
    public async Task DisablingUsbRevokesSelectedCameraWithoutOpeningHardwareOrStoppingOtherInstances()
    {
        var selectedLocations = new[] { UsbTarget(), UsbTarget(location: "USB\\VID_03C3&PID_620B\\SECOND") };
        object other = UsbTarget(serial: "fedcba9876543210");
        var calls = new List<string[]>();
        var helper = new UsbRecoveryHelper(@"C:\Regain\regain-camera.exe", (_, args, timeout, _) => {
            Assert.Equal(TimeSpan.FromSeconds(8), timeout); calls.Add(args.ToArray());
            if (args.Last() == "status") return Task.FromResult(new UsbRecoveryCommandResult(0,
                UsbSessionStatus([.. selectedLocations, other])));
            Assert.Equal(new[] { "usb", "helper", "revoke" }, args.Take(3));
            var revoked = UsbRecoveryHelper.DecodeTarget(args[3]);
            Assert.Equal(UsbCameraSerial, revoked.GetProperty("serial").GetString());
            return Task.FromResult(new UsbRecoveryCommandResult(0, JsonSerializer.Serialize(new {
                protocolVersion = 1, revoked = true, targetsRemaining = 4 - calls.Count, stopped = false
            })));
        });
        await helper.DisableAsync(UsbSelection(connected: true));
        Assert.Equal(3, calls.Count);
        Assert.Equal("USB\\VID_03C3&PID_620B\\CAMERA", UsbRecoveryHelper.DecodeTarget(calls[1][3]).GetProperty("location").GetString());
        Assert.Equal("USB\\VID_03C3&PID_620B\\SECOND", UsbRecoveryHelper.DecodeTarget(calls[2][3]).GetProperty("location").GetString());
        Assert.DoesNotContain(calls.SelectMany(call => call), arg => arg is "stop" or "prepare" or "--usb-target" or "reset" or "cycle");
    }

    [Fact]
    public async Task DisablingUsbIsIdempotentButDoesNotHideAuthenticationFailure()
    {
        int calls = 0;
        var helper = new UsbRecoveryHelper(@"C:\Regain\regain-camera.exe", (_, _, _, _) => {
            calls++; return Task.FromResult(new UsbRecoveryCommandResult(0, UsbSessionStatus([], running: false)));
        });
        await helper.DisableAsync(UsbSelection()); await helper.DisableAsync(UsbSelection());
        Assert.Equal(2, calls);
        await helper.DisableAsync(UsbSelection(simulated: true));
        await helper.DisableAsync(UsbSelection(name: "Unsupported"));
        Assert.Equal(2, calls);
        var rejected = new UsbRecoveryHelper(@"C:\Regain\regain-camera.exe", (_, _, _, _) =>
            Task.FromResult(new UsbRecoveryCommandResult(1, "", "USB helper server is not elevated")));
        var error = await Assert.ThrowsAsync<InvalidOperationException>(() => rejected.DisableAsync(UsbSelection()));
        Assert.Contains("not elevated", error.Message);
    }
}
