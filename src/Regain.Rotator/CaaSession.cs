using System.Diagnostics;
using System.IO;
using System.Text.Json;
using System.Text.Json.Serialization;

namespace Regain.Rotator;

public sealed class CaaProfile
{
    public string Serial { get; set; } = "";
    public double LogicalOffset { get; set; }
    public bool Synced { get; set; }
    public bool CoordinatesUncertain { get; set; }
}

public sealed class CaaStatus
{
    [JsonPropertyName("mechanical_degrees")] public double Mechanical { get; set; }
    [JsonPropertyName("logical_degrees")] public double Logical { get; set; }
    [JsonPropertyName("logical_offset")] public double Offset { get; set; }
    [JsonPropertyName("target_degrees")] public double Target { get; set; }
    [JsonPropertyName("moving")] public bool Moving { get; set; }
    [JsonPropertyName("limit_degrees")] public int Limit { get; set; }
    [JsonPropertyName("error")] public int Error { get; set; }
    [JsonPropertyName("reverse")] public bool Reverse { get; set; }
    [JsonPropertyName("motion_error")] public string? MotionError { get; set; }
}

public sealed class CaaChoice
{
    public string Serial { get; set; } = "";
    public string Label { get; set; } = "";
    public override string ToString() => Label;
}

/// One exclusive SDK-free Rust worker per rotator. Commands are never replayed.
public sealed class CaaSession : IDisposable, IDeviceSession
{
    // Half the CAA step: absorb representation noise without accepting a full motor step.
    public const double OpticalToleranceDegrees = .01;
    public bool IsFalcon { get; }
    public string ModelName => IsFalcon ? "Pegasus Falcon V2" : "CAA Rotator";
    public string ActionPrefix => IsFalcon ? "Regain.Falcon." : "Regain.CAA.";
    public string[] SupportedActions => IsFalcon ? new[] { "Status", "Settings", "Identity", "SetReference", "RotateUnwrapped", "ResetOrigin" }.Select(a => ActionPrefix + a).ToArray() : Actions;
    private bool ValidSerial(string serial) => IsFalcon ? serial.Length is > 0 and <= 128 && serial.All(c => char.IsLetterOrDigit(c) || c is '-' or '_' or ':') : serial.Length == 16 && serial.All(Uri.IsHexDigit);
    private readonly object gate = new();
    private readonly string executable;
    public string ProfilePath { get; }
    private Process? worker;
    private StreamWriter? input;
    private readonly OpticalStateRecovery<CaaStatus> recovery = new();
    private bool opening;
    public bool ConnectionRequested { get; private set; }
    public string OpticalConnectionEpoch { get { lock (gate) { recovery.RequireVerified(); return recovery.ConnectionEpoch; } } }
    public Action<string>? Log { get; set; }
    private bool WorkerConnected => worker is { HasExited: false };
    public bool Connected { get { lock (gate) return ConnectionRequested && !recovery.IsBlocked && (WorkerConnected || recovery.CanRecover); } }
    public CaaProfile Profile { get; private set; }
    public CaaSession(string executable, string profilePath, bool falcon = false)
    {
        this.executable = executable;
        IsFalcon = falcon;
        ProfilePath = profilePath;
        Profile = ReadProfile();
    }
    private CaaProfile ReadProfile() => File.Exists(ProfilePath) ? JsonSerializer.Deserialize<CaaProfile>(File.ReadAllText(ProfilePath)) ?? new() : new();
    public static string SettingsPath(string slot) => RegainPaths.EnvironmentVariable("REGAIN_ROTATOR_SETTINGS") ?? RegainPaths.Profile(Path.Combine("Rotators", slot + ".json"));
    private Process Start(string arguments)
    {
        var process = new Process { StartInfo = new(executable, (IsFalcon ? "pegasus falcon " : "zwo caa ") + arguments + (RegainPaths.EnvironmentVariable("REGAIN_ROTATOR_SIMULATE") == "1" ? " --simulate" : "")) { UseShellExecute = false, CreateNoWindow = true,
            WindowStyle = ProcessWindowStyle.Hidden, RedirectStandardInput = true, RedirectStandardOutput = true, RedirectStandardError = true,
            StandardOutputEncoding = new System.Text.UTF8Encoding(false),
            WorkingDirectory = Path.GetDirectoryName(executable)! } };
        process.ErrorDataReceived += (_, e) => { if (!string.IsNullOrWhiteSpace(e.Data)) Diagnostic(e.Data!); };
        process.Start(); process.BeginErrorReadLine(); return process;
    }
    private void Diagnostic(string message)
    {
        try { Log?.Invoke(message); } catch { }
        try {
            string folder = Path.GetDirectoryName(ProfilePath)!; Directory.CreateDirectory(folder);
            lock (gate) {
                string path = Path.Combine(folder, "rotator.log");
                if (File.Exists(path) && new FileInfo(path).Length > 2_000_000) File.WriteAllText(path, "");
                File.AppendAllText(path, DateTimeOffset.Now.ToString("O") + " " + message + Environment.NewLine);
            }
        } catch { }
    }
    public List<CaaChoice> Discover()
    {
        if (Connected) throw new InvalidOperationException("Disconnect before refreshing devices");
        using var process = Start("list-details");
        try {
            var read = process.StandardOutput.ReadLineAsync();
            if (!read.Wait(TimeSpan.FromSeconds(15))) throw new TimeoutException("Rotator enumeration timed out");
            using var doc = JsonDocument.Parse(read.Result ?? throw new IOException("Rotator enumeration failed; check rotator.log"));
            var result = new List<CaaChoice>();
            foreach (var item in doc.RootElement.EnumerateArray()) {
                if (!item.TryGetProperty("identity", out var id)) continue;
                string serial = id.GetProperty("serial").GetString()!;
                result.Add(new() { Serial = serial, Label = id.GetProperty("model").GetString() + " — " + serial });
            }
            return result;
        } finally { if (!process.HasExited) { if (!process.WaitForExit(1000)) process.Kill(); } }
    }
    public void Select(string serial)
    {
        lock (gate) {
            if (Connected) throw new InvalidOperationException("Disconnect before changing the rotator");
            if (!ValidSerial(serial)) throw new ArgumentException("Choose a rotator from the list");
            if (!string.Equals(Profile.Serial, serial, StringComparison.OrdinalIgnoreCase)) Profile = new() { Serial = serial };
            Save();
        }
    }
    private void Save()
    {
        Directory.CreateDirectory(Path.GetDirectoryName(ProfilePath)!);
        string temporary = ProfilePath + "." + Guid.NewGuid().ToString("N") + ".tmp";
        try {
            File.WriteAllText(temporary, JsonSerializer.Serialize(Profile, new JsonSerializerOptions { WriteIndented = true }));
            if (File.Exists(ProfilePath)) File.Replace(temporary, ProfilePath, null); else File.Move(temporary, ProfilePath);
        } finally { if (File.Exists(temporary)) File.Delete(temporary); }
    }
    public void Connect() => Open(false);
    private void Open(bool recovering)
    {
        lock (gate) {
            if (WorkerConnected && !recovery.IsBlocked) return;
            CloseWorker();
            if (!recovering) ConnectionRequested = true;
            // Setup may have been opened by a separate ASCOM instance since this one was created.
            if (!recovering) { Profile = ReadProfile(); recovery.Reset(); }
            else if (ReadProfile().Serial != Profile.Serial) throw new IOException("Rotator selection changed during recovery");
            Diagnostic("Connecting rotator; settings: " + ProfilePath + "; worker: " + executable);
            if (string.IsNullOrEmpty(Profile.Serial)) {
                var choices = Discover();
                if (choices.Count == 0) throw new InvalidOperationException("No available rotator found. Check USB and close other rotator controllers.");
                if (choices.Count != 1) throw new InvalidOperationException("More than one rotator found. Choose a rotator in ASCOM setup.");
                Select(choices[0].Serial);
                Diagnostic("Saved the only available rotator as the selected rotator");
            }
            if (!ValidSerial(Profile.Serial)) throw new InvalidOperationException("The saved rotator selection is invalid. Choose a rotator in setup.");
            worker?.Dispose(); worker = Start("serve --serial " + Profile.Serial);
            input = new StreamWriter(worker.StandardInput.BaseStream, new System.Text.UTF8Encoding(false)) { AutoFlush = true };
            opening = true;
            try {
                var identity = Request(new { command = "identity" });
                if (!string.Equals(identity.GetProperty("serial").GetString(), Profile.Serial, StringComparison.OrdinalIgnoreCase)) throw new IOException("Rotator identity changed");
                if (Profile.CoordinatesUncertain) {
                    Profile.LogicalOffset = 0; Profile.Synced = false; Profile.CoordinatesUncertain = false; Save();
                    Diagnostic("Previous reference operation did not finish cleanly; sync the sky angle again");
                }
                var status = Status();
                Request(new { command = "sync", degrees = Wrap(status.Logical + Profile.LogicalOffset) });
                Diagnostic("Connected rotator using the native Rust worker");
            } catch { CloseWorker(); throw; }
            finally { opening = false; }
        }
    }
    public JsonElement Request(object request)
    {
        lock (gate) {
            if (!WorkerConnected) throw new InvalidOperationException("Rotator is disconnected");
            string name = JsonSerializer.SerializeToElement(request).GetProperty("command").GetString()!;
            bool write = !IsFalcon && !opening && name is not ("identity" or "status" or "settings" or "stop");
            if (write) recovery.Writing();
            bool acknowledged = false;
            try {
                input!.WriteLine(JsonSerializer.Serialize(request));
                var read = worker!.StandardOutput.ReadLineAsync();
                if (!read.Wait(TimeSpan.FromSeconds(10))) { CloseWorker(); throw new TimeoutException("Rotator worker timed out; motion was not retried"); }
                using var document = JsonDocument.Parse(read.Result ?? throw new IOException("Rotator worker exited"));
                acknowledged = true;
                if (!document.RootElement.GetProperty("ok").GetBoolean()) throw new InvalidOperationException(document.RootElement.GetProperty("error").GetString());
                return document.RootElement.GetProperty("result").Clone();
            } catch (Exception e) { if (write && !acknowledged) recovery.Uncertain(e); Diagnostic(e.Message); throw; }
        }
    }
    public static double Wrap(double degrees) => (degrees % 360 + 360) % 360;
    public CaaStatus Status()
    {
        lock (gate) {
            CaaStatus Read() {
                var state = JsonSerializer.Deserialize<CaaStatus>(Request(new { command = "status" }).GetRawText())!;
                if (!IsFalcon) state.Reverse = Request(new { command = "settings" }).GetProperty("reverse").GetBoolean();
                return state;
            }
            var status = IsFalcon || opening ? Read() : recovery.Read(Read, () => Open(true), CloseWorker,
                value => !value.Moving && value.Error == 0 && value.MotionError is null,
                (before, after) => Math.Abs(before.Mechanical - after.Mechanical) <= OpticalToleranceDegrees &&
                    Math.Abs(before.Logical - after.Logical) <= OpticalToleranceDegrees && Math.Abs(before.Offset - after.Offset) <= OpticalToleranceDegrees && before.Limit == after.Limit && before.Reverse == after.Reverse,
                Diagnostic);
            if (Profile.CoordinatesUncertain && !status.Moving && status.Error == 0 && status.MotionError is null) {
                Profile.LogicalOffset = status.Offset; Profile.CoordinatesUncertain = false; Save();
            }
            return status;
        }
    }
    public string VerifiedOpticalState()
    {
        var state = Status(); CheckMotion(state);
        if (state.Moving || double.IsNaN(state.Mechanical) || double.IsInfinity(state.Mechanical) ||
            double.IsNaN(state.Logical) || double.IsInfinity(state.Logical) || double.IsNaN(state.Offset) || double.IsInfinity(state.Offset))
            throw new IOException("Rotator position is unknown or moving; exposure is blocked.");
        return string.Join("|", Profile.Serial, state.Mechanical.ToString("R", System.Globalization.CultureInfo.InvariantCulture),
            state.Logical.ToString("R", System.Globalization.CultureInfo.InvariantCulture), state.Offset.ToString("R", System.Globalization.CultureInfo.InvariantCulture), state.Limit, state.Reverse ? "1" : "0");
    }
    public static bool OpticalStatesMatch(string before, string after)
    {
        var a = before.Split('|'); var b = after.Split('|');
        if (a.Length != 6 || b.Length != 6 || a[0] != b[0] || a[4] != b[4] || a[5] != b[5]) return false;
        for (int i = 1; i <= 3; i++) {
            if (!double.TryParse(a[i], System.Globalization.NumberStyles.Float, System.Globalization.CultureInfo.InvariantCulture, out double x) ||
                !double.TryParse(b[i], System.Globalization.NumberStyles.Float, System.Globalization.CultureInfo.InvariantCulture, out double y) ||
                double.IsNaN(x) || double.IsInfinity(x) || double.IsNaN(y) || double.IsInfinity(y) || Math.Abs(x - y) > OpticalToleranceDegrees) return false;
        }
        return true;
    }
    public void CheckMotion(CaaStatus status)
    {
        if (status.Error != 0 || status.MotionError is not null) throw new IOException(status.MotionError ?? "Rotator fault " + status.Error);
    }
    public void RememberCoordinates(bool? synced = null)
    {
        lock (gate) {
            Profile.LogicalOffset = Status().Offset;
            if (synced.HasValue) Profile.Synced = synced.Value;
            Save();
        }
    }
    public void Command(string name, double degrees) { if (!IsFalcon) { var state = Status(); CheckMotion(state); } Request(new { command = name, degrees }); }
    public void Sync(double degrees) { Command("sync", degrees); RememberCoordinates(true); }
    public void Halt() { Request(new { command = "stop" }); RememberCoordinates(); }
    public static readonly string[] Actions = { "Regain.CAA.Status", "Regain.CAA.Settings", "Regain.CAA.Identity", "Regain.CAA.SetReference", "Regain.CAA.SetLimit", "Regain.CAA.SetBeep", "Regain.CAA.SetAlias", "Regain.CAA.RotateUnwrapped", "Regain.CAA.ResetOrigin" };
    public string Action(string name, string parameters)
    {
        lock (gate) {
        if (!SupportedActions.Any(a => a.Equals(name, StringComparison.OrdinalIgnoreCase)) && IsFalcon) throw new NotSupportedException("Unsupported Falcon action");
        string canonical = IsFalcon ? RegainPaths.ActionName(name).Replace("regain.falcon.", "regain.caa.") : name;
        int index = Array.FindIndex(Actions, a => a.Equals(RegainPaths.ActionName(canonical), StringComparison.OrdinalIgnoreCase));
        string command = index switch { 0 => "status", 1 => "settings", 2 => "identity", 3 => "reference", 4 => "limit", 5 => "beep", 6 => "alias", 7 => "rotate-unwrapped", 8 => "reset-origin", _ => throw new NotSupportedException("Unknown rotator action") };
        JsonElement result;
        if (index < 3) result = Request(new { command });
        else if (index == 8) {
            Profile.CoordinatesUncertain = true; Profile.Synced = false; Save();
            result = Request(new { command }); RememberCoordinates(); Diagnostic("Rotator mechanical origin reset to zero");
        }
        else {
            using var values = JsonDocument.Parse(parameters);
            object request = index switch {
                5 => new { command, enabled = values.RootElement.GetProperty("enabled").GetBoolean() },
                6 => new { command, text = values.RootElement.GetProperty("text").GetString() },
                _ => new { command, degrees = values.RootElement.GetProperty("degrees").GetDouble() }
            };
            if (index is 3 or 7) { Profile.CoordinatesUncertain = true; Profile.Synced = false; Save(); }
            result = Request(request);
            if (index == 3) RememberCoordinates();
            Diagnostic("Rotator action " + name);
        }
        return result.GetRawText();
        }
    }
    public void Disconnect()
    {
        lock (gate) {
            try { if (WorkerConnected && !recovery.IsBlocked) { Halt(); Diagnostic("Disconnected rotator"); } }
            finally { ConnectionRequested = false; recovery.Reset(); CloseWorker(); }
        }
    }
    private void CloseWorker()
    {
        var closing = worker; worker = null;
        var closingInput = input; input = null;
        if (closing is null) return;
        try { closingInput?.Dispose(); if (!closing.HasExited && !closing.WaitForExit(1500)) closing.Kill(); }
        finally { closing.Dispose(); }
    }
    public void Dispose() { try { Disconnect(); } catch { CloseWorker(); } }
}
