using System.Diagnostics;
using System.IO;
using System.Text.Json;
using System.Text.Json.Serialization;

namespace Regain.Rotator;

public sealed class AccessoryProfile
{
    public string Serial { get; set; } = "";
    public bool Unidirectional { get; set; }
    public string[] Names { get; set; } = [];
    public int[] FocusOffsets { get; set; } = [];
}
public sealed class AccessoryStatus
{
    [JsonPropertyName("position")] public int Position { get; set; }
    [JsonPropertyName("moving")] public bool Moving { get; set; }
    [JsonPropertyName("calibrating")] public bool Calibrating { get; set; }
    [JsonPropertyName("detected_slots")] public int? DetectedSlots { get; set; }
    [JsonPropertyName("slots")] public int Slots { get; set; }
    [JsonPropertyName("max_step")] public int MaxStep { get; set; }
    [JsonPropertyName("speed")] public int Speed { get; set; }
    [JsonPropertyName("backlash")] public int Backlash { get; set; }
    [JsonPropertyName("beep")] public bool Beep { get; set; }
    [JsonPropertyName("reverse")] public bool Reverse { get; set; }
    [JsonPropertyName("hand_control")] public bool HandControl { get; set; }
    [JsonPropertyName("temperature_c")] public double? Temperature { get; set; }
    [JsonPropertyName("error")] public int Error { get; set; }
    [JsonPropertyName("fault")] public string? Fault { get; set; }
}

/// Exclusive USB worker session shared by the native NINA and ASCOM frontends.
public sealed class AccessorySession : IDisposable
{
    private readonly object gate = new();
    private readonly string executable;
    private Process? worker;
    private StreamWriter? input;
    public string Kind { get; }
    public string ProfilePath { get; }
    public AccessoryProfile Profile { get; private set; }
    public bool Connected { get { lock (gate) return worker is { HasExited: false }; } }
    public AccessorySession(string executable, string kind, string profilePath)
    {
        if (kind is not ("efw" or "eaf" or "fc3")) throw new ArgumentException("Unknown accessory");
        this.executable = executable; Kind = kind; ProfilePath = profilePath; Profile = ReadProfile();
    }
    public static string SettingsPath(string kind, string frontend) => RegainPaths.EnvironmentVariable("REGAIN_ACCESSORY_SETTINGS") is string directory
        ? Path.Combine(directory, kind + "-" + frontend + ".json")
        : RegainPaths.Profile(Path.Combine("Accessories", kind + "-" + frontend + ".json"));
    private AccessoryProfile ReadProfile() => File.Exists(ProfilePath) ? JsonSerializer.Deserialize<AccessoryProfile>(File.ReadAllText(ProfilePath)) ?? new() : new();
    public void Save()
    {
        lock (gate) {
            Directory.CreateDirectory(Path.GetDirectoryName(ProfilePath)!);
            var temporary = ProfilePath + "." + Guid.NewGuid().ToString("N") + ".tmp";
            try {
                File.WriteAllText(temporary, JsonSerializer.Serialize(Profile, new JsonSerializerOptions { WriteIndented = true }));
                if (File.Exists(ProfilePath)) File.Replace(temporary, ProfilePath, null); else File.Move(temporary, ProfilePath);
            } finally { if (File.Exists(temporary)) File.Delete(temporary); }
        }
    }
    private Process Start(string arguments)
    {
        if (Regain.Rotator.RegainPaths.EnvironmentVariable("REGAIN_ACCESSORY_SIMULATE") == "1") arguments += " --simulate";
        var process = new Process { StartInfo = new(executable, (Kind == "fc3" ? "" : Kind + " ") + arguments) {
            UseShellExecute = false, CreateNoWindow = true, WindowStyle = ProcessWindowStyle.Hidden,
            RedirectStandardInput = true, RedirectStandardOutput = true, RedirectStandardError = true,
            WorkingDirectory = Path.GetDirectoryName(executable)!, StandardOutputEncoding = new System.Text.UTF8Encoding(false) } };
        process.ErrorDataReceived += (_, e) => { if (e.Data is not null) Log(e.Data); };
        process.Start(); process.BeginErrorReadLine(); return process;
    }
    private void Log(string text)
    {
        try {
            var directory = Path.GetDirectoryName(ProfilePath)!; Directory.CreateDirectory(directory);
            var path = Path.Combine(directory, Kind + ".log");
            if (File.Exists(path) && new FileInfo(path).Length > 2_000_000) File.WriteAllText(path, "");
            File.AppendAllText(path, DateTimeOffset.Now.ToString("O") + " " + text + Environment.NewLine);
        } catch { }
    }
    public List<CaaChoice> Discover()
    {
        lock (gate) {
            if (Connected) throw new InvalidOperationException("Disconnect before refreshing devices");
            using var process = Start("list-details");
            try {
                var read = process.StandardOutput.ReadLineAsync();
                if (!read.Wait(TimeSpan.FromSeconds(15))) throw new TimeoutException("Device discovery timed out");
                using var doc = JsonDocument.Parse(read.Result ?? throw new IOException("Discovery failed; check the accessory log"));
                return doc.RootElement.EnumerateArray().Select(item => {
                    var identity = item.GetProperty("identity"); var serial = identity.GetProperty("serial").GetString()!;
                    return new CaaChoice { Serial = serial, Label = identity.GetProperty("model").GetString() + " — " + serial };
                }).ToList();
            } finally { if (!process.HasExited && !process.WaitForExit(1000)) process.Kill(); }
        }
    }
    public void Select(string serial)
    {
        lock (gate) {
            if (Connected) throw new InvalidOperationException("Disconnect before changing devices");
            ValidateSerial(serial);
            if (!string.Equals(Profile.Serial, serial, StringComparison.OrdinalIgnoreCase)) Profile = new() { Serial = serial };
            Save();
        }
    }
    private void ValidateSerial(string serial)
    {
        if (Kind == "fc3" ? !System.Text.RegularExpressions.Regex.IsMatch(serial, @"\A[0-9A-Fa-f]{2}(:[0-9A-Fa-f]{2}){5}\z") : (serial.Length != 16 || serial.Any(c => !Uri.IsHexDigit(c)))) throw new ArgumentException("Select a device by its serial number");
    }
    public void Connect()
    {
        lock (gate) {
            if (Connected) return;
            Profile = ReadProfile();
            if (Profile.Serial.Length == 0) {
                var choices = Discover();
                if (choices.Count != 1) throw new InvalidOperationException("Choose an available device in setup; found " + choices.Count);
                Select(choices[0].Serial);
            }
            ValidateSerial(Profile.Serial);
            worker?.Dispose(); worker = Start("serve --serial " + Profile.Serial);
            input = new StreamWriter(worker.StandardInput.BaseStream, new System.Text.UTF8Encoding(false)) { AutoFlush = true };
            try {
                var identity = Request(new { command = "identity" });
                if (!string.Equals(identity.GetProperty("serial").GetString(), Profile.Serial, StringComparison.OrdinalIgnoreCase)) throw new IOException("USB device identity changed");
                var status = Status();
                if (Kind == "efw") {
                    if (Profile.Names.Length != status.Slots || Profile.FocusOffsets.Length != status.Slots) {
                        Profile.Names = Enumerable.Range(1, status.Slots).Select(i => "Filter " + i).ToArray();
                        Profile.FocusOffsets = new int[status.Slots]; Save();
                    }
                }
                Log("Connected " + identity.GetRawText());
            } catch { CloseWorker(); throw; }
        }
    }
    public JsonElement Request(object request)
    {
        lock (gate) {
            if (!Connected) throw new InvalidOperationException("Device is disconnected");
            JsonDocument document;
            try {
                input!.WriteLine(JsonSerializer.Serialize(request));
                var read = worker!.StandardOutput.ReadLineAsync();
                if (!read.Wait(TimeSpan.FromSeconds(10))) throw new TimeoutException("USB worker timed out; command was not retried");
                document = JsonDocument.Parse(read.Result ?? throw new IOException("USB worker exited"));
            } catch (Exception error) { Log(error.Message); CloseWorker(); throw; }
            using (document) {
                if (!document.RootElement.GetProperty("ok").GetBoolean()) throw new InvalidOperationException(document.RootElement.GetProperty("error").GetString());
                return document.RootElement.GetProperty("result").Clone();
            }
        }
    }
    public AccessoryStatus Status()
    {
        var s = JsonSerializer.Deserialize<AccessoryStatus>(Request(new { command = "status" }).GetRawText())!;
        if (s.Error != 0 || s.Fault is not null) throw new IOException(s.Fault ?? "Device error " + s.Error);
        return s;
    }
    public void Move(int position)
    {
        var s = Status();
        if (position < 0 || position > (Kind == "efw" ? s.Slots - 1 : s.MaxStep)) throw new ArgumentOutOfRangeException(nameof(position));
        Request(new { command = "move", position, unidirectional = Profile.Unidirectional });
    }
    public void Halt() => Request(new { command = "halt" });
    public void Calibrate()
    {
        if (Kind != "efw") throw new NotSupportedException("Calibration applies to EFW only");
        Request(new { command = "calibrate" });
    }
    public void Disconnect() { lock (gate) CloseWorker(); }
    private void CloseWorker()
    {
        input?.Dispose(); input = null;
        if (worker is not null) {
            try { if (!worker.HasExited && !worker.WaitForExit(3000)) worker.Kill(); }
            finally { worker.Dispose(); worker = null; }
        }
    }
    public void Dispose() => Disconnect();
}
