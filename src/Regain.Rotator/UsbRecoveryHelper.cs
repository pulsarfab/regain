using System.Diagnostics;
using System.Globalization;
using System.IO;
using System.Text;
using System.Text.Json;
using Regain.Hub;

namespace Regain.Rotator;

/// An ephemeral setup selection. USB bindings and helper approval are never saved in a profile.
public sealed class UsbRecoverySelection(string? name, string? serial, bool connected, bool simulated)
{
    public string Name { get; } = name ?? "";
    public string Serial { get; } = (serial ?? "").Trim().ToLowerInvariant();
    public bool Connected { get; } = connected;
    public bool Simulated { get; } = simulated;
}

public sealed class UsbRecoveryHelperStatus
{
    public bool Running { get; internal set; }
    public bool Elevated { get; internal set; }
    public bool Approved { get; internal set; }
    public bool SessionScoped { get; internal set; }
    public ulong ExpiresAtUnixSeconds { get; internal set; }
    public bool IsReady => Running && Elevated && Approved &&
        (SessionScoped || ExpiresAtUnixSeconds > (ulong)DateTimeOffset.UtcNow.ToUnixTimeSeconds());
    public string Summary => !Running ? "USB recovery helper is stopped." :
        !Elevated ? "USB recovery helper is not elevated." :
        !SessionScoped && ExpiresAtUnixSeconds <= (ulong)DateTimeOffset.UtcNow.ToUnixTimeSeconds() ? "USB recovery helper approval has expired. Enable it again before connecting." :
        !Approved ? "USB recovery helper is running; this camera is not approved. Disconnect and prepare this camera." :
        SessionScoped ? "USB recovery is ready for this Windows session, including camera and supervisor restarts." :
        "USB recovery is ready for this camera until " + DateTimeOffset.FromUnixTimeSeconds((long)ExpiresAtUnixSeconds).ToLocalTime().ToString("g", CultureInfo.CurrentCulture) + ".";
}

internal sealed class UsbRecoveryCommandResult(int exitCode, string output, string error = "")
{
    internal int ExitCode { get; } = exitCode;
    internal string Output { get; } = output;
    internal string Error { get; } = error;
}

/// Only the explicit Prepare operation can request elevation, through regain-device.
/// Check, Disable and Stop never bind/open a camera, and none of these operations resets USB.
public sealed class UsbRecoveryHelper
{
    internal delegate Task<UsbRecoveryCommandResult> Runner(string executable, IReadOnlyList<string> arguments,
        TimeSpan timeout, CancellationToken cancellation);
    private readonly string executable;
    private readonly Runner runner;
    private readonly SemaphoreSlim gate = new(1, 1);
    private static readonly IReadOnlyDictionary<string, int> Products = new Dictionary<string, int>(StringComparer.Ordinal) {
        ["ZWO ASI585MM Pro"] = 0x585e, ["ZWO ASI676MC"] = 0x676d, ["ZWO ASI662MC"] = 0x662b,
        ["ZWO ASI2600MM Duo"] = 0x2601, ["ZWO ASI2600MM Pro"] = 0x260e,
        ["ZWO ASI220MM Mini"] = 0x2209, ["ZWO ASI6200MM Pro"] = 0x620b
    };

    public UsbRecoveryHelper(string cameraWorker) : this(cameraWorker, RunAsync) { }
    internal UsbRecoveryHelper(string cameraWorker, Runner runner)
    {
        // Follow the same selected installation as the camera worker, including test overrides.
        executable = Path.Combine(Path.GetDirectoryName(Path.GetFullPath(cameraWorker))!, "regain-device.exe");
        this.runner = runner;
    }

    public static string? PreparationUnavailable(UsbRecoverySelection selection) =>
        selection.Connected ? "Disconnect the camera before preparing USB recovery." :
        selection.Simulated ? "USB recovery is unavailable for simulation." :
        !Products.ContainsKey(selection.Name) ? "Select a camera with verified USB recovery support." :
        selection.Serial.Length != 16 || selection.Serial.Any(c => !Uri.IsHexDigit(c)) || selection.Serial == "0000000000000000"
            ? "Select the camera and enter its saved 16-character hexadecimal serial number." : null;

    public async Task<UsbRecoveryHelperStatus> PrepareAsync(Func<UsbRecoverySelection> selected,
        CancellationToken cancellation = default)
    {
        await gate.WaitAsync(cancellation);
        try {
            var selection = selected();
            ValidatePreparation(selection);
            string token = (await Call(["zwo", "camera-direct", "--usb-target", selection.Name, selection.Serial],
                TimeSpan.FromSeconds(15), cancellation)).Trim();
            var target = DecodeTarget(token);
            if (!Matches(target, selection)) throw new InvalidOperationException("USB binding did not match the selected camera. No helper approval was requested.");
            // A setup edit or a connection during the read-only binding must not approve a stale selection.
            var current = selected();
            ValidatePreparation(current);
            if (current.Name != selection.Name || current.Serial != selection.Serial)
                throw new InvalidOperationException("The selected camera changed. Prepare the new selection again.");
            var reply = await Call(["usb", "helper", "prepare", token],
                TimeSpan.FromSeconds(70), cancellation);
            var status = ParseStatus(reply, selection, target);
            if (!status.IsReady)
                throw new InvalidOperationException("USB recovery helper did not confirm current approval for this camera. Check its status before connecting.");
            return status;
        } finally { gate.Release(); }
    }

    public async Task<UsbRecoveryHelperStatus> CheckAsync(UsbRecoverySelection selection, CancellationToken cancellation = default)
    {
        await gate.WaitAsync(cancellation);
        try {
            // An unscoped status is read-only and does not open a camera to produce a target token.
            return ParseStatus(await Call(["usb", "helper", "status"], TimeSpan.FromSeconds(8), cancellation), selection);
        } finally { gate.Release(); }
    }

    public async Task StopAsync(CancellationToken cancellation = default)
    {
        await gate.WaitAsync(cancellation);
        try {
            using var document = Parse(await Call(["usb", "helper", "stop"], TimeSpan.FromSeconds(8), cancellation));
            var value = document.RootElement;
            if (value.GetProperty("protocolVersion").GetInt32() != 1 || !value.GetProperty("stopped").GetBoolean())
                throw new InvalidOperationException("USB recovery helper did not confirm that it stopped.");
        } catch (Exception error) when (error is JsonException or KeyNotFoundException) {
            throw new InvalidOperationException("Invalid USB recovery helper response.", error);
        } finally { gate.Release(); }
    }

    /// Revoke only this model/serial's approved locations. Other ASCOM instances
    /// retain their approval; the broker exits when its final target is removed.
    public async Task DisableAsync(UsbRecoverySelection selection, CancellationToken cancellation = default)
    {
        if (selection.Simulated || !Products.ContainsKey(selection.Name)) return;
        await gate.WaitAsync(cancellation);
        try {
            string json = await Call(["usb", "helper", "status"], TimeSpan.FromSeconds(8), cancellation);
            var status = ParseStatus(json, selection);
            if (!status.Running || !status.Approved) return;
            using var document = Parse(json);
            var targets = document.RootElement.GetProperty("targets").EnumerateArray()
                .Where(target => Matches(target, selection)).Select(target => target.Clone()).ToArray();
            foreach (var target in targets) {
                string token = BitConverter.ToString(Encoding.UTF8.GetBytes(target.GetRawText())).Replace("-", "").ToLowerInvariant();
                using var reply = Parse(await Call(["usb", "helper", "revoke", token], TimeSpan.FromSeconds(8), cancellation));
                var value = reply.RootElement;
                int remaining = value.GetProperty("targetsRemaining").GetInt32();
                if (value.GetProperty("protocolVersion").GetInt32() != 1 || remaining < 0 || remaining > 16 ||
                    value.GetProperty("stopped").GetBoolean() != (remaining == 0))
                    throw new InvalidOperationException("USB recovery helper did not confirm camera revocation.");
                _ = value.GetProperty("revoked").GetBoolean(); // false means already revoked by another frontend.
                if (remaining == 0) break;
            }
        } catch (Exception error) when (error is JsonException or KeyNotFoundException or FormatException) {
            throw new InvalidOperationException("Invalid USB recovery helper response.", error);
        } finally { gate.Release(); }
    }

    private static void ValidatePreparation(UsbRecoverySelection selection)
    {
        string? reason = PreparationUnavailable(selection);
        if (reason is not null) throw new InvalidOperationException(reason);
    }

    private async Task<string> Call(string[] arguments, TimeSpan timeout, CancellationToken cancellation)
    {
        var result = await runner(executable, arguments, timeout, cancellation);
        cancellation.ThrowIfCancellationRequested();
        if (result.ExitCode != 0) {
            string message = new string(result.Error.Where(c => !char.IsControl(c) || c is '\r' or '\n' or '\t').Take(600).ToArray()).Trim();
            throw new InvalidOperationException(message.Length == 0 ? "USB recovery helper is unavailable. Prepare it before connecting." : message);
        }
        if (result.Output.Length > 65536) throw new InvalidOperationException("USB recovery helper response was too large.");
        return result.Output;
    }

    private static JsonDocument Parse(string json)
    {
        if (json.Length > 65536) throw new InvalidOperationException("USB recovery helper response was too large.");
        return JsonDocument.Parse(json, new JsonDocumentOptions { MaxDepth = 8 });
    }

    internal static JsonElement DecodeTarget(string token)
    {
        if (token.Length == 0 || token.Length > 4096 || token.Length % 2 != 0 || token.Any(c => !Uri.IsHexDigit(c)))
            throw new InvalidOperationException("Invalid USB camera binding. No helper approval was requested.");
        var bytes = new byte[token.Length / 2];
        for (int i = 0; i < bytes.Length; i++) bytes[i] = byte.Parse(token.Substring(i * 2, 2), NumberStyles.HexNumber, CultureInfo.InvariantCulture);
        try {
            using var document = Parse(Encoding.UTF8.GetString(bytes));
            ValidateTarget(document.RootElement);
            return document.RootElement.Clone();
        } catch (Exception error) when (error is JsonException or KeyNotFoundException or FormatException or InvalidOperationException) {
            throw new InvalidOperationException("Invalid USB camera binding. No helper approval was requested.", error);
        } finally { Array.Clear(bytes, 0, bytes.Length); }
    }

    private static void ValidateTarget(JsonElement target)
    {
        string serial = target.GetProperty("serial").GetString() ?? "";
        string location = target.GetProperty("location").GetString() ?? "";
        if (target.GetProperty("vendor").GetInt32() != 0x03c3 || !Products.Values.Contains(target.GetProperty("product").GetInt32()) ||
            serial.Length != 16 || serial.Any(c => !Uri.IsHexDigit(c)) || serial == "0000000000000000" ||
            location.Length == 0 || location.Length > 512 || location.Any(c => !(c is >= 'a' and <= 'z' or >= 'A' and <= 'Z' or >= '0' and <= '9') && !"\\&_.-".Contains(c)) ||
            target.GetProperty("address").ValueKind != JsonValueKind.Null || target.GetProperty("generation").GetUInt64() == 0)
            throw new InvalidOperationException("Invalid Windows USB camera target.");
    }

    private static bool Matches(JsonElement target, UsbRecoverySelection selection) => !selection.Simulated &&
        Products.TryGetValue(selection.Name, out int product) && target.GetProperty("product").GetInt32() == product &&
        string.Equals(target.GetProperty("serial").GetString(), selection.Serial, StringComparison.OrdinalIgnoreCase);

    private static UsbRecoveryHelperStatus ParseStatus(string json, UsbRecoverySelection selection, JsonElement? exactTarget = null)
    {
        try {
            using var document = Parse(json);
            var value = document.RootElement;
            if (value.GetProperty("protocolVersion").GetInt32() != 1) throw new InvalidOperationException("Unsupported USB recovery helper protocol.");
            var targets = value.GetProperty("targets");
            if (targets.ValueKind != JsonValueKind.Array || targets.GetArrayLength() > 16) throw new InvalidOperationException("Invalid USB recovery helper targets.");
            bool matched = false;
            foreach (var target in targets.EnumerateArray()) {
                ValidateTarget(target);
                // Approval survives a device-scoped reset at the same location.
                // Fresh arrival generation is verified by the backend on every reset, not by the allowlist.
                matched |= Matches(target, selection) && (!exactTarget.HasValue ||
                    string.Equals(target.GetProperty("location").GetString(), exactTarget.Value.GetProperty("location").GetString(), StringComparison.OrdinalIgnoreCase));
            }
            bool sessionScoped = value.TryGetProperty("sessionScoped", out var session) && session.GetBoolean();
            var expiry = value.GetProperty("expiresAtUnixSeconds");
            // Legacy helpers retain their real expiry; null is only valid with
            // an explicit session-scoped contract, never an implicit forever approval.
            if (sessionScoped != (expiry.ValueKind == JsonValueKind.Null))
                throw new InvalidOperationException("Invalid USB recovery helper lifetime.");
            ulong expires = sessionScoped ? 0 : expiry.GetUInt64();
            if (expires > 253402300799UL) throw new InvalidOperationException("Invalid USB recovery helper expiry.");
            return new UsbRecoveryHelperStatus {
                Running = value.GetProperty("running").GetBoolean(), Elevated = value.GetProperty("elevated").GetBoolean(),
                ExpiresAtUnixSeconds = expires, SessionScoped = sessionScoped,
                Approved = matched && (!exactTarget.HasValue || value.GetProperty("approved").GetBoolean())
            };
        } catch (Exception error) when (error is JsonException or KeyNotFoundException or FormatException) {
            throw new InvalidOperationException("Invalid USB recovery helper response.", error);
        }
    }

    internal static ProcessStartInfo StartInfo(string executable, IReadOnlyList<string> arguments) => new(executable,
        string.Join(" ", arguments.Select(HubAttachment.Quote))) {
        UseShellExecute = false, CreateNoWindow = true, WindowStyle = ProcessWindowStyle.Hidden,
        RedirectStandardOutput = true, RedirectStandardError = true, WorkingDirectory = Path.GetDirectoryName(executable)!
    };

    internal static async Task<UsbRecoveryCommandResult> RunAsync(string executable, IReadOnlyList<string> arguments,
        TimeSpan timeout, CancellationToken cancellation)
    {
        if (!File.Exists(executable)) throw new InvalidOperationException("regain-device.exe is missing beside the selected camera worker. Reinstall the matching Regain package.");
        cancellation.ThrowIfCancellationRequested();
        var process = new Process { StartInfo = StartInfo(executable, arguments) };
        using var deadline = CancellationTokenSource.CreateLinkedTokenSource(cancellation);
        var expired = Task.Delay(timeout, deadline.Token);
        Task<UsbRecoveryCommandResult>? completion = null;
        void StopClient() {
            // Only this unelevated CLI client is ours. Never kill a prepared shared helper.
            try { if (!process.HasExited) process.Kill(); } catch { }
        }
        try {
            if (!process.Start()) throw new InvalidOperationException("USB recovery helper client did not start.");
            async Task<UsbRecoveryCommandResult> Complete() {
                // Cancellation is enforced by the independent deadline below, never by
                // native pipe cancellation callbacks that could delay the dispatcher.
                var output = HubAttachment.ReadBounded(process.StandardOutput.BaseStream, 65536, CancellationToken.None);
                var errors = HubAttachment.ReadBounded(process.StandardError.BaseStream, 65536, CancellationToken.None);
                await Task.WhenAll(output, errors).ConfigureAwait(false);
                while (!process.HasExited) await Task.Delay(20, deadline.Token).ConfigureAwait(false);
                return new UsbRecoveryCommandResult(process.ExitCode, Encoding.UTF8.GetString(output.Result), Encoding.UTF8.GetString(errors.Result));
            }
            completion = Complete();
            // An elevated launch/consent process can inherit this client's stdout/stderr.
            // Windows redirected-pipe reads do not always cancel until that descendant
            // closes its handle, even after our unelevated client exits or is killed.
            // The deadline must therefore race the reads rather than merely cancel them.
            if (await Task.WhenAny(completion, expired).ConfigureAwait(false) != completion) {
                cancellation.ThrowIfCancellationRequested();
                throw new TimeoutException("USB helper completion is unknown. Use Check before preparing again; no reset was requested.");
            }
            return await completion.ConfigureAwait(false);
        } catch {
            StopClient(); cancellation.ThrowIfCancellationRequested();
            throw;
        } finally {
            // Do not dispose a reader/process on a cancellation callback or the caller's
            // thread: StreamReader.Dispose may wait for a native pipe read to complete.
            // Keep cleanup off the setup dispatcher and observe any late read failure.
            deadline.Cancel();
            if (completion is not null) _ = completion.ContinueWith(task => { _ = task.Exception; },
                CancellationToken.None, TaskContinuationOptions.OnlyOnFaulted | TaskContinuationOptions.ExecuteSynchronously, TaskScheduler.Default);
            _ = Task.Run(() => {
                try { process.StandardOutput.Dispose(); } catch { }
                try { process.StandardError.Dispose(); } catch { }
                process.Dispose();
            });
        }
    }
}
