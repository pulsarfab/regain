using System.Text;
using System.Text.Json;
using System.Windows.Automation;
using System.Windows.Controls;
using Regain.Rotator;
using Xunit;

namespace Regain.NINA.Tests;

public sealed partial class HubNativeTests
{
    private const string UsbCameraName = "ZWO ASI6200MM Pro";
    private const string UsbCameraSerial = "0123456789abcdef";
    private static UsbRecoverySelection UsbSelection(bool connected = false, bool simulated = false,
        string name = UsbCameraName, string serial = UsbCameraSerial) => new(name, serial, connected, simulated);
    private static object UsbTarget(string serial = UsbCameraSerial, int product = 0x620b,
        string location = "USB\\VID_03C3&PID_620B\\CAMERA", ulong generation = 42) => new {
            vendor = 0x03c3, product, serial, location, address = (byte?)null, generation
        };
    private static string UsbToken(object target) => Convert.ToHexString(Encoding.UTF8.GetBytes(JsonSerializer.Serialize(target))).ToLowerInvariant();
    private static string UsbStatus(object? target = null, bool approved = true, ulong? expiry = null, bool elevated = true) => JsonSerializer.Serialize(new {
        protocolVersion = 1, running = true, elevated,
        expiresAtUnixSeconds = expiry ?? (ulong)DateTimeOffset.UtcNow.AddHours(2).ToUnixTimeSeconds(),
        approved, targets = target is null ? Array.Empty<object>() : new[] { target }
    });

    [Fact]
    public async Task UsbPreparationRejectsUnsafeSelectionsWithoutStartingAnyProcess()
    {
        int calls = 0;
        var helper = new UsbRecoveryHelper(@"C:\Regain package\regain-camera.exe", (_, _, _, _) => {
            calls++; throw new InvalidOperationException("No process may start");
        });
        foreach (var selection in new[] { UsbSelection(connected: true), UsbSelection(simulated: true),
            UsbSelection(serial: ""), UsbSelection(serial: "0000000000000000"), UsbSelection(serial: "0123456789abcdeg"),
            UsbSelection(name: "ZWO ASI6200MM Pro\" --reset"), UsbSelection(name: "Unsupported camera") }) {
            await Assert.ThrowsAsync<InvalidOperationException>(() => helper.PrepareAsync(() => selection));
        }
        Assert.Equal(0, calls);
    }

    [Fact]
    public async Task UsbPreparationBindsExactCameraThenUsesOnlyExplicitPrepareForElevation()
    {
        var calls = new List<(string path, string[] args, TimeSpan timeout)>();
        var target = UsbTarget(); string token = UsbToken(target);
        var helper = new UsbRecoveryHelper(@"C:\Selected package\regain-alpaca.exe", (path, args, timeout, _) => {
            calls.Add((path, args.ToArray(), timeout));
            return Task.FromResult(new UsbRecoveryCommandResult(0, calls.Count == 1 ? token + "\r\n" : UsbStatus(target)));
        });
        var status = await helper.PrepareAsync(() => UsbSelection(serial: UsbCameraSerial.ToUpperInvariant()));
        Assert.True(status.Approved); Assert.Contains("ready", status.Summary);
        Assert.Equal(2, calls.Count);
        Assert.All(calls, call => Assert.Equal(@"C:\Selected package\regain-device.exe", call.path));
        Assert.Equal(new[] { "zwo", "camera-direct", "--usb-target", UsbCameraName, UsbCameraSerial }, calls[0].args);
        Assert.Equal(new[] { "usb", "helper", "prepare", token }, calls[1].args);
        Assert.Equal(TimeSpan.FromSeconds(15), calls[0].timeout); Assert.Equal(TimeSpan.FromSeconds(70), calls[1].timeout);
        Assert.DoesNotContain(calls.SelectMany(call => call.args), arg => arg is "reset" or "cycle" or "runas");
    }

    [Fact]
    public async Task UsbPreparationNeverApprovesChangedConnectionSelectionOrMismatchedBinding()
    {
        foreach (bool connected in new[] { false, true }) {
            int calls = 0; var current = UsbSelection();
            var helper = new UsbRecoveryHelper(@"C:\Regain\regain-camera.exe", (_, _, _, _) => {
                calls++;
                current = connected ? UsbSelection(connected: true) : UsbSelection(serial: "fedcba9876543210");
                return Task.FromResult(new UsbRecoveryCommandResult(0, UsbToken(UsbTarget())));
            });
            await Assert.ThrowsAsync<InvalidOperationException>(() => helper.PrepareAsync(() => current));
            Assert.Equal(1, calls);
        }
        foreach (object target in new[] { UsbTarget(serial: "fedcba9876543210"), UsbTarget(product: 0x585e), UsbTarget(generation: 0) }) {
            int calls = 0;
            var helper = new UsbRecoveryHelper(@"C:\Regain\regain-camera.exe", (_, _, _, _) => {
                calls++; return Task.FromResult(new UsbRecoveryCommandResult(0, UsbToken(target)));
            });
            await Assert.ThrowsAsync<InvalidOperationException>(() => helper.PrepareAsync(() => UsbSelection()));
            Assert.Equal(1, calls);
        }
    }

    [Fact]
    public async Task UsbPreparationRequiresCurrentExactApprovalAndReportsUnknownCompletion()
    {
        foreach (string reply in new[] { UsbStatus(UsbTarget(), approved: false), UsbStatus(UsbTarget(location: "USB\\OTHER_CAMERA")),
            UsbStatus(UsbTarget(), expiry: 1), UsbStatus(UsbTarget(), elevated: false), "{}" }) {
            int calls = 0;
            var helper = new UsbRecoveryHelper(@"C:\Regain\regain-camera.exe", (_, _, _, _) => Task.FromResult(
                new UsbRecoveryCommandResult(0, ++calls == 1 ? UsbToken(UsbTarget()) : reply)));
            await Assert.ThrowsAsync<InvalidOperationException>(() => helper.PrepareAsync(() => UsbSelection()));
            Assert.Equal(2, calls);
        }
        int attempts = 0;
        var timedOut = new UsbRecoveryHelper(@"C:\Regain\regain-camera.exe", (_, _, _, _) => {
            if (++attempts == 1) return Task.FromResult(new UsbRecoveryCommandResult(0, UsbToken(UsbTarget())));
            throw new TimeoutException("Unknown preparation completion; Check before trying again");
        });
        await Assert.ThrowsAsync<TimeoutException>(() => timedOut.PrepareAsync(() => UsbSelection()));
        Assert.Equal(2, attempts); // No automatic elevation retry.
        int reboundCalls = 0;
        var rebound = new UsbRecoveryHelper(@"C:\Regain\regain-camera.exe", (_, _, _, _) => Task.FromResult(
            new UsbRecoveryCommandResult(0, ++reboundCalls == 1 ? UsbToken(UsbTarget(generation: 43)) : UsbStatus(UsbTarget(generation: 42)))));
        Assert.True((await rebound.PrepareAsync(() => UsbSelection())).Approved); // Stable approval; reset validates current generation.
    }

    [Fact]
    public async Task UsbCheckAndStopAreInertForConnectedAndSimulatedCamerasAndSurfaceErrors()
    {
        var calls = new List<string[]>();
        var helper = new UsbRecoveryHelper(@"C:\Regain\regain-camera.exe", (_, args, timeout, _) => {
            calls.Add(args.ToArray()); Assert.Equal(TimeSpan.FromSeconds(8), timeout);
            return Task.FromResult(new UsbRecoveryCommandResult(0, args.Last() == "stop" ? "{\"protocolVersion\":1,\"stopped\":true}" : UsbStatus(UsbTarget(), approved: false)));
        });
        Assert.True((await helper.CheckAsync(UsbSelection(connected: true))).Approved);
        Assert.False((await helper.CheckAsync(UsbSelection(simulated: true))).Approved);
        await helper.StopAsync();
        Assert.Equal(new[] { "usb", "helper", "status" }, calls[0]);
        Assert.Equal(new[] { "usb", "helper", "status" }, calls[1]);
        Assert.Equal(new[] { "usb", "helper", "stop" }, calls[2]);
        var absent = new UsbRecoveryHelper(@"C:\Regain\regain-camera.exe", (_, _, _, _) =>
            Task.FromResult(new UsbRecoveryCommandResult(1, "", "USB reset helper is not prepared")));
        var error = await Assert.ThrowsAsync<InvalidOperationException>(() => absent.CheckAsync(UsbSelection()));
        Assert.Contains("not prepared", error.Message);
        var malformed = new UsbRecoveryHelper(@"C:\Regain\regain-camera.exe", (_, _, _, _) => Task.FromResult(new UsbRecoveryCommandResult(0, "not JSON")));
        await Assert.ThrowsAsync<InvalidOperationException>(() => malformed.CheckAsync(UsbSelection()));
    }

    [Fact]
    public void UsbHelperArgumentsUseWindowsArgvEscapingWithoutShellOrRunas()
    {
        var start = UsbRecoveryHelper.StartInfo(@"C:\Regain package\regain-device.exe", new[] { "zwo", "camera-direct", "ZWO ASI6200MM Pro", "trailing\\", "a\"b", "$(secret); &" });
        Assert.False(start.UseShellExecute); Assert.True(start.CreateNoWindow); Assert.Equal("", start.Verb);
        Assert.Equal(@"C:\Regain package\regain-device.exe", start.FileName);
        Assert.Equal("\"zwo\" \"camera-direct\" \"ZWO ASI6200MM Pro\" \"trailing\\\\\" \"a\\\"b\" \"$(secret); &\"", start.Arguments);
        Assert.ThrowsAny<Exception>(() => UsbRecoveryHelper.StartInfo(@"C:\Regain\regain-device.exe", new[] { "bad\0argument" }));
    }

    [Fact]
    public async Task SharedUsbToggleStartsNothingAndPersistsOnlyAfterApprovalThenRevokesOnlyThisCamera()
    {
        await Wpf(async () => {
                int calls = 0; var selection = UsbSelection(); var order = new List<string>();
                var helper = new UsbRecoveryHelper(@"C:\Regain\regain-camera.exe", (_, args, _, _) => {
                    calls++; order.Add(args.Contains("--usb-target") ? "bind" : args[2]);
                    return Task.FromResult(new UsbRecoveryCommandResult(0, args.Contains("--usb-target") ? UsbToken(UsbTarget()) :
                        args.Contains("revoke") ? "{\"protocolVersion\":1,\"revoked\":true,\"targetsRemaining\":1,\"stopped\":false}" : UsbStatus(UsbTarget())));
                });
                using var panel = new UsbRecoveryHelperPanel(helper, () => selection, 0, failures => {
                    order.Add("save:" + failures); return Task.CompletedTask;
                });
                var actions = panel.Children.OfType<WrapPanel>().Single();
                var prepare = actions.Children.OfType<Button>().Single(button => AutomationProperties.GetAutomationId(button) == "usb-helper-prepare");
                var toggle = Assert.Single(panel.Children.OfType<CheckBox>());
                Assert.Equal("usb-reset-enabled", AutomationProperties.GetAutomationId(toggle));
                Assert.Equal(0, calls); Assert.True(toggle.IsEnabled); Assert.False(toggle.IsChecked); Assert.False(prepare.IsEnabled);
                Assert.Contains(panel.Children.OfType<TextBlock>(), block => block.Text.Contains("attached filter wheel"));
                selection = UsbSelection(connected: true); panel.RefreshAvailability(); Assert.False(toggle.IsEnabled);
                await panel.SetEnabledAsync(true); Assert.Equal(0, calls);
                selection = UsbSelection(simulated: true); panel.RefreshAvailability(); Assert.False(toggle.IsEnabled);
                selection = UsbSelection(); panel.RefreshAvailability(); Assert.True(toggle.IsEnabled);
                await panel.SetEnabledAsync(true); Assert.Equal(2, calls); Assert.True(toggle.IsChecked); Assert.Equal(2, panel.ResetAfterFailures);
                Assert.Equal(new[] { "bind", "prepare", "save:2" }, order);
                await panel.SetEnabledAsync(false); Assert.False(toggle.IsChecked); Assert.Equal(0, panel.ResetAfterFailures);
                Assert.Equal(new[] { "bind", "prepare", "save:2", "save:0", "status", "revoke" }, order);
                Assert.DoesNotContain("stop", order);
                selection = UsbSelection(serial: ""); panel.RefreshAvailability(); Assert.False(toggle.IsEnabled);
        });
    }

    [Fact]
    public async Task SharedUsbToggleDenialAndSelectionChangesNeverEnableSavedPolicy()
    {
        await Wpf(async () => {
            foreach (bool changed in new[] { false, true }) {
                int calls = 0, saves = 0; var selection = UsbSelection();
                var helper = new UsbRecoveryHelper(@"C:\Regain\regain-camera.exe", (_, args, _, _) => {
                    calls++;
                    if (args.Contains("--usb-target")) return Task.FromResult(new UsbRecoveryCommandResult(0, UsbToken(UsbTarget())));
                    if (changed) selection = UsbSelection(serial: "fedcba9876543210");
                    return Task.FromResult(changed ? new UsbRecoveryCommandResult(0, UsbStatus(UsbTarget())) :
                        new UsbRecoveryCommandResult(1, "", "Administrator approval was canceled."));
                });
                using var panel = new UsbRecoveryHelperPanel(helper, () => selection, 0, _ => { saves++; return Task.CompletedTask; });
                await panel.SetEnabledAsync(true);
                Assert.Equal(2, calls); Assert.Equal(0, saves); Assert.Equal(0, panel.ResetAfterFailures);
                Assert.False(Assert.Single(panel.Children.OfType<CheckBox>()).IsChecked);
            }
        });
    }

    [Fact]
    public async Task SharedUsbToggleKeepsSavedPolicySeparateFromHelperReadinessAndPersistsThreshold()
    {
        await Wpf(async () => {
            var saves = new List<int>(); var selection = UsbSelection();
            var helper = new UsbRecoveryHelper(@"C:\Regain\regain-camera.exe", (_, args, _, _) => Task.FromResult(new UsbRecoveryCommandResult(0,
                args.Contains("--usb-target") ? UsbToken(UsbTarget()) : args.Contains("prepare") ? UsbStatus(UsbTarget()) : UsbStatus(UsbTarget(), expiry: 1))));
            using var panel = new UsbRecoveryHelperPanel(helper, () => selection, 3, failures => { saves.Add(failures); return Task.CompletedTask; });
            var toggle = Assert.Single(panel.Children.OfType<CheckBox>());
            var prepare = panel.Children.OfType<WrapPanel>().Single().Children.OfType<Button>().Single(button => AutomationProperties.GetAutomationId(button) == "usb-helper-prepare");
            Assert.True(toggle.IsChecked); Assert.Empty(saves);
            await panel.CheckAsync(); Assert.True(toggle.IsChecked); Assert.Equal(System.Windows.Visibility.Visible, prepare.Visibility);
            Assert.Contains(panel.Children.OfType<TextBlock>(), block => block.Text.Contains("expired"));
            await panel.PrepareAsync(); Assert.Empty(saves); Assert.Equal(System.Windows.Visibility.Collapsed, prepare.Visibility);
            var threshold = Assert.Single(panel.Children.OfType<TextBox>());
            threshold.Text = "5"; await panel.SaveThresholdAsync(); Assert.Equal(new[] { 5 }, saves); Assert.Equal(5, panel.ResetAfterFailures);
            threshold.Text = "0"; await panel.SaveThresholdAsync(); Assert.Equal(new[] { 5 }, saves);
            Assert.Throws<ArgumentException>(() => panel.ResetAfterFailures);
            threshold.Text = "5"; selection = UsbSelection(connected: true); panel.RefreshAvailability(); Assert.False(toggle.IsEnabled);
            await panel.SetEnabledAsync(false); Assert.Equal(new[] { 5 }, saves); Assert.True(toggle.IsChecked);
        });
    }

    [Fact]
    public async Task SharedUsbToggleSaveFailurePreservesPolicyAndCleanupFailureStillLeavesPolicyOff()
    {
        await Wpf(async () => {
            int calls = 0; bool denySave = true;
            var helper = new UsbRecoveryHelper(@"C:\Regain\regain-camera.exe", (_, _, _, _) => {
                calls++; return Task.FromResult(new UsbRecoveryCommandResult(1, "", "Helper status unavailable"));
            });
            using var panel = new UsbRecoveryHelperPanel(helper, () => UsbSelection(), 2, _ => {
                if (denySave) throw new IOException("Profile is read-only"); return Task.CompletedTask;
            });
            await panel.SetEnabledAsync(false); Assert.Equal(2, panel.ResetAfterFailures); Assert.Equal(0, calls);
            denySave = false;
            await panel.SetEnabledAsync(false); Assert.Equal(0, panel.ResetAfterFailures); Assert.Equal(1, calls);
            Assert.False(Assert.Single(panel.Children.OfType<CheckBox>()).IsChecked);
            Assert.Contains(panel.Children.OfType<TextBlock>(), block => block.Text.Contains("Could not remove helper approval"));
        });
    }

    [Fact]
    public async Task SharedUsbToggleOwnsThresholdWithoutDuplicateGeneratedFieldAndRecoverySaveIsAtomic()
    {
        await Wpf(() => {
            var values = JsonSerializer.SerializeToElement(new { maxRetries = 7, usbResetAfterFailures = 3, usbPortCycle = true });
            var form = new CameraRecoveryForm(values, _ => new StackPanel(), excludedFields: ["usbResetAfterFailures"]);
            Assert.DoesNotContain(form.Editors, editor => AutomationProperties.GetAutomationId(editor) == "recovery-usbResetAfterFailures");
            Assert.Equal(3, form.Read().GetProperty("usbResetAfterFailures").GetInt32());
            Assert.Equal(7, form.Read().GetProperty("maxRetries").GetInt32());
            Assert.True(form.Read().GetProperty("usbPortCycle").GetBoolean());
            string directory = Path.Combine(Path.GetTempPath(), "regain-usb-toggle-" + Guid.NewGuid().ToString("N"));
            try {
                string path = Path.Combine(directory, "recovery.json");
                Settings.SaveRecovery(new Regain.Core.RecoveryOptions { UsbResetAfterFailures = 3, MaxRetries = 7 }, path);
                var saved = JsonSerializer.Deserialize<Regain.Core.RecoveryOptions>(File.ReadAllText(path))!;
                Settings.SaveRecovery(saved with { UsbResetAfterFailures = 0 }, path);
                saved = JsonSerializer.Deserialize<Regain.Core.RecoveryOptions>(File.ReadAllText(path))!;
                Assert.Equal(0, saved.UsbResetAfterFailures); Assert.Equal(7, saved.MaxRetries);
                Assert.Single(Directory.GetFiles(directory));
            } finally { if (Directory.Exists(directory)) Directory.Delete(directory, true); }
            return Task.CompletedTask;
        });
    }
}
