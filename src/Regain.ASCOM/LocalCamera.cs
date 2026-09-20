using System.Diagnostics;
using System.Text.Json;

namespace Regain.Ascom;

/// One private Rust camera process owned by this COM object. No network transport.
internal sealed class LocalCamera : IDisposable
{
    private readonly object gate = new();
    private readonly int slot;
    private Process? worker;
    private ProcessJob? job;
    private bool faulted;
    private int transaction;
    private bool disposed;
    internal LocalCamera(int slot) { this.slot = slot; }
    private void Start()
    {
        if (faulted) throw new ASCOM.DriverException("Camera worker failed. Disconnect and reconnect the camera.");
        if (disposed) throw new ObjectDisposedException(nameof(LocalCamera));
        if (worker is not null) {
            if (worker.HasExited) throw new ASCOM.DriverException("Camera worker exited. Disconnect and reconnect the camera.");
            return;
        }
        string directory = Path.GetDirectoryName(typeof(LocalCamera).Assembly.Location)!;
        string executable = Regain.Rotator.RegainPaths.EnvironmentVariable("REGAIN_CAMERA_WORKER") ?? Path.Combine(directory, "regain-camera.exe");
        string? profiles = Regain.Rotator.RegainPaths.EnvironmentVariable("REGAIN_ASCOM_PROFILES");
        string path = Path.GetFullPath(profiles is not null ? Path.Combine(profiles, $"camera-{slot + 1}.json") : Regain.Rotator.RegainPaths.Profile(Path.Combine("ASCOM", $"camera-{slot + 1}.json")));
        worker = new Process { StartInfo = new ProcessStartInfo(executable, "--profiles \"" + path + "\"" + (Regain.Rotator.RegainPaths.EnvironmentVariable("REGAIN_ASCOM_SIMULATE") == "1" ? " --simulate" : "")) {
            UseShellExecute = false, CreateNoWindow = true, WindowStyle = ProcessWindowStyle.Hidden,
            RedirectStandardInput = true, RedirectStandardOutput = true, RedirectStandardError = true,
            WorkingDirectory = Path.GetDirectoryName(executable)!
        }};
        worker.ErrorDataReceived += (_, _) => { }; // Rust writes its diagnostic file; always drain stderr.
        try { worker.Start(); job = ProcessJob.Attach(worker); worker.BeginErrorReadLine(); }
        catch { Close(); throw; }
    }
    internal JsonElement Request(string method, string member = "", object? parameters = null) => Exchange(method, member, parameters).value;
    private (JsonElement value, byte[] pixels) Exchange(string method, string member, object? parameters)
    {
        lock (gate) {
            Start(); int id = ++transaction;
            byte[] bytes = JsonSerializer.SerializeToUtf8Bytes(new { version = 1, id, method, member, @params = parameters });
            try {
                var work = Task.Run(async () => {
                    var input = worker!.StandardInput.BaseStream; var output = worker.StandardOutput.BaseStream;
                    await input.WriteAsync(BitConverter.GetBytes(bytes.Length), 0, 4).ConfigureAwait(false);
                    await input.WriteAsync(bytes, 0, bytes.Length).ConfigureAwait(false); await input.FlushAsync().ConfigureAwait(false);
                    async Task<byte[]> Read(int count) {
                        var buffer = new byte[count]; int offset = 0;
                        while (offset < count) { int n = await output.ReadAsync(buffer, offset, count - offset).ConfigureAwait(false); if (n == 0) throw new IOException("Camera worker closed its pipe"); offset += n; }
                        return buffer;
                    }
                    int length = BitConverter.ToInt32(await Read(4).ConfigureAwait(false), 0);
                    if (length < 1 || length > 1024 * 1024) throw new IOException("Invalid camera reply length");
                    using var doc = JsonDocument.Parse(await Read(length).ConfigureAwait(false)); var reply = doc.RootElement;
                    if (reply.GetProperty("version").GetInt32() != 1 || reply.GetProperty("id").GetInt32() != id) throw new IOException("Invalid camera reply identity");
                    int binary = reply.GetProperty("binaryLength").GetInt32();
                    if (binary < 0 || binary > 512 * 1024 * 1024) throw new IOException("Invalid camera image length");
                    var pixels = await Read(binary).ConfigureAwait(false);
                    return (reply.Clone(), pixels);
                });
                if (!((IAsyncResult)work).AsyncWaitHandle.WaitOne(TimeSpan.FromSeconds(120))) { Close(); throw new TimeoutException("Camera worker timed out; reconnect before continuing"); }
                var result = work.GetAwaiter().GetResult();
                var reply = result.Item1;
                int error = reply.GetProperty("errorNumber").GetInt32();
                if (error != 0) {
                    string message = reply.GetProperty("errorMessage").GetString()!;
                    throw error switch {
                        0x400 when method == "get" => new ASCOM.PropertyNotImplementedException(member, false),
                        0x400 when method == "put" && parameters is not null && member != "action" && !member.StartsWith("command") => new ASCOM.PropertyNotImplementedException(member, true),
                        0x400 => new ASCOM.MethodNotImplementedException(member),
                        0x401 => new ASCOM.InvalidValueException(message),
                        0x402 or 0x40B => new ASCOM.InvalidOperationException(message),
                        0x407 => new ASCOM.NotConnectedException(message),
                        0x40C => new ASCOM.ActionNotImplementedException(message),
                        _ => new ASCOM.DriverException(message)
                    };
                }
                return (reply.GetProperty("value").Clone(), result.Item2);
            } catch (Exception e) when (e is IOException or TimeoutException) { faulted = true; Close(); throw new ASCOM.DriverException(e.Message, e); }
        }
    }
    internal T Get<T>(string member) => JsonSerializer.Deserialize<T>(Request("get", member).GetRawText())!;
    internal void Put(string member, object? value = null) {
        if (value is double d && (double.IsNaN(d) || double.IsInfinity(d))) throw new ASCOM.InvalidValueException(member + " must be finite");
        Request("put", member, value is null ? null : new Dictionary<string, object> { [member] = value });
    }
    internal object Image(bool variant) {
        var (value, bytes) = Exchange("get", "imagearray", null);
        int w = value.GetProperty("width").GetInt32(), h = value.GetProperty("height").GetInt32();
        if (w <= 0 || h <= 0 || (long)w * h * 2 != bytes.Length) throw new ASCOM.DriverException("Invalid image dimensions");
        if (variant) {
            var image = new object[w, h]; for (int y = 0; y < h; y++) for (int x = 0; x < w; x++) image[x, y] = (int)BitConverter.ToUInt16(bytes, (y * w + x) * 2); return image;
        } else {
            var image = new int[w, h]; for (int y = 0; y < h; y++) for (int x = 0; x < w; x++) image[x, y] = (int)BitConverter.ToUInt16(bytes, (y * w + x) * 2); return image;
        }
    }
    private void Close() {
        if (worker is null) return;
        try { worker.StandardInput.Close(); if (!worker.WaitForExit(5000)) worker.Kill(); }
        catch (InvalidOperationException) { }
        finally { job?.Dispose(); job = null; worker.Dispose(); worker = null; }
    }
    public void Dispose() { lock (gate) { if (disposed) return; disposed = true; Close(); } }
}
