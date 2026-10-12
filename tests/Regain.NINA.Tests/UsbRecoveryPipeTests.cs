using System.Diagnostics;
using System.Text.Json;
using Regain.Rotator;
using Xunit;

namespace Regain.NINA.Tests;

public sealed class UsbRecoveryPipeTests
{
    [Theory]
    [InlineData(false, false)]
    [InlineData(true, false)]
    [InlineData(true, true)]
    public async Task InheritedOutputPipesCannotHoldSetupPastDeadlineOrCancellation(bool parentStaysAlive, bool cancel)
    {
        string executable = Path.Combine(AppContext.BaseDirectory, "fixtures", "usb-helper-pipe", "UsbHelperPipeFixture.exe");
        string directory = Path.Combine(Path.GetTempPath(), "regain-usb-pipe-" + Guid.NewGuid().ToString("N"));
        Directory.CreateDirectory(directory);
        using var cancellation = new CancellationTokenSource();
        Task<UsbRecoveryCommandResult>? operation = null;
        Process? parent = null, child = null;
        try {
            operation = UsbRecoveryHelper.RunAsync(executable,
                [parentStaysAlive ? "--spawn-and-hang" : "--spawn-and-exit", directory], TimeSpan.FromSeconds(3), cancellation.Token);
            var started = Stopwatch.StartNew();
            while (!File.Exists(Path.Combine(directory, "child-ready")) && started.Elapsed < TimeSpan.FromSeconds(2)) await Task.Delay(10);
            Assert.True(File.Exists(Path.Combine(directory, "child-ready")), "Fixture child did not start before deadline.");
            using var identities = JsonDocument.Parse(File.ReadAllText(Path.Combine(directory, "processes.json")));
            child = Process.GetProcessById(identities.RootElement.GetProperty("child").GetInt32());
            int parentPid = identities.RootElement.GetProperty("parent").GetInt32();
            try { parent = Process.GetProcessById(parentPid); } catch (ArgumentException) { }
            if (!parentStaysAlive) {
                if (parent is not null) Assert.True(parent.WaitForExit(1000), "Fixture parent should exit independently of its child.");
                await Task.Delay(50);
                // Parent is gone but descendant still holds both handles. This proves
                // real inherited pipes, rather than a timer against a sleeping parent.
                Assert.False(operation.IsCompleted);
            }
            if (cancel) cancellation.Cancel();
            if (cancel) await Assert.ThrowsAnyAsync<OperationCanceledException>(() => operation.WaitAsync(TimeSpan.FromSeconds(5)));
            else await Assert.ThrowsAsync<TimeoutException>(() => operation.WaitAsync(TimeSpan.FromSeconds(5)));
            Assert.False(child.HasExited); // The runtime must never kill a broker/process tree.
            if (parent is not null) Assert.True(parent.WaitForExit(1000), "Only the unelevated client should be killed.");
            File.WriteAllText(Path.Combine(directory, "release-child"), "");
            Assert.True(child.WaitForExit(3000));
        } finally {
            // These handles belong solely to this fixture; no PID/path enumeration.
            if (child is not null) { try { if (!child.HasExited) child.Kill(); } catch { } child.Dispose(); }
            if (parent is not null) { try { if (!parent.HasExited) parent.Kill(); } catch { } parent.Dispose(); }
            if (operation is not null) { try { await operation.WaitAsync(TimeSpan.FromSeconds(5)); } catch { } }
            if (Directory.Exists(directory)) Directory.Delete(directory, true);
        }
    }
}
