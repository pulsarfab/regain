using System.IO;
using System.Text;
using System.Text.Json;
using System.Threading;
using System.Windows.Threading;

namespace Regain.Hub.ASCOM;

internal static class Program {
    internal const int MaxRequestBytes = 4096;
    internal const int MaxResponseBytes = 1024 * 1024;

    [STAThread]
    private static int Main(string[] args) {
        if (args.Length == 0 || args[0] != "--import") return ExportServer.Run(args);
        Options options;
        try { options = Options.Parse(args); }
        catch { return 2; } // Never echo arguments, exception text, or driver output.
        var input = Console.OpenStandardInput();
        var output = Console.OpenStandardOutput();
        Console.SetOut(TextWriter.Null);
        Console.SetError(TextWriter.Null);
        var dispatcher = Dispatcher.CurrentDispatcher;
        var driver = new ImportDriver(options);
        // Activation is deferred until the parent's first connectStep. The parent
        // attaches its process ownership guard before issuing any driver request.
        var reader = new Thread(() => Serve(input, output, dispatcher, driver)) { IsBackground = true };
        reader.Start();
        Dispatcher.Run();
        return 0;
    }

    private static void Serve(Stream input, Stream output, Dispatcher dispatcher, ImportDriver driver) {
        try {
            while (ReadFrame(input) is byte[] frame) {
                using var document = JsonDocument.Parse(frame, new JsonDocumentOptions { MaxDepth = 32 });
                var request = Request.Parse(document.RootElement);
                var result = dispatcher.Invoke(() => driver.Execute(request));
                // A structured, sanitized inner error stays inside an acknowledged
                // outer frame so the existing Rust accessory transport preserves it.
                var bytes = JsonSerializer.SerializeToUtf8Bytes(new { ok = true, result });
                if (bytes.Length >= MaxResponseBytes) throw new InvalidDataException();
                output.Write(bytes, 0, bytes.Length);
                output.WriteByte((byte)'\n');
                output.Flush();
            }
        } catch {
            // Malformed framing is terminal. Do not resynchronize a corrupt stream
            // or return an acknowledgement for an unidentifiable request.
        } finally {
            try { dispatcher.Invoke(driver.Close); } catch { }
            dispatcher.BeginInvokeShutdown(DispatcherPriority.Send);
        }
    }

    private static byte[]? ReadFrame(Stream input) {
        using var frame = new MemoryStream();
        while (true) {
            var next = input.ReadByte();
            if (next < 0) {
                if (frame.Length == 0) return null;
                throw new InvalidDataException();
            }
            if (next == '\n') {
                if (frame.Length == 0) throw new InvalidDataException();
                return frame.ToArray();
            }
            if (frame.Length >= MaxRequestBytes - 1) throw new InvalidDataException();
            frame.WriteByte((byte)next);
        }
    }
}

internal sealed class Options {
    public string ProgId { get; private set; } = "";
    public string DeviceType { get; private set; } = "";
    public bool Managed { get; private set; }
    public Guid[] DeniedClasses { get; private set; } = [];
    public static Options Parse(string[] args) {
        if (args.Length is not (9 or 11) || args[0] != "--import") throw new ArgumentException();
        var values = new Dictionary<string, string>(StringComparer.Ordinal);
        for (var i = 1; i < args.Length; i += 2) values.Add(args[i], args[i + 1]);
        if (values.Count != (args.Length - 1) / 2 || !values.TryGetValue("--prog-id", out var progId)
            || !values.TryGetValue("--device-type", out var type)
            || !values.TryGetValue("--connection-policy", out var policy)
            || !values.TryGetValue("--bitness", out var bitness)) throw new ArgumentException();
        if (string.IsNullOrWhiteSpace(progId) || progId.Length > 200 || progId != progId.Trim()
            || progId.Any(char.IsControl) || !new[] { "switch", "safetymonitor", "observingconditions", "focuser", "rotator" }.Contains(type)
            || !new[] { "managed", "externallyManaged" }.Contains(policy)
            || bitness != (Environment.Is64BitProcess ? "x64" : "x86")) throw new ArgumentException();
        Guid[] denied = [];
        if (args.Length == 11) {
            if (!values.TryGetValue("--deny-clsids", out var text)) throw new ArgumentException();
            var ids = text.Split(',');
            if (ids.Length is 0 or > 256 || ids.Any(id => !Guid.TryParseExact(id, "D", out var parsed) || parsed == Guid.Empty)) throw new ArgumentException();
            denied = ids.Select(Guid.Parse).ToArray();
            if (denied.Distinct().Count() != denied.Length) throw new ArgumentException();
        }
        return new Options { ProgId = progId, DeviceType = type, Managed = policy == "managed", DeniedClasses = denied };
    }
}

internal sealed class Request {
    public long Id { get; private set; }
    public string Operation { get; private set; } = "";
    public string Member { get; private set; } = "";
    public JsonElement Parameters { get; private set; }
    public static Request Parse(JsonElement root) {
        EnsureUnique(root);
        if (root.ValueKind != JsonValueKind.Object || root.EnumerateObject().Any(p => !new[] {
                "protocol", "id", "operation", "member", "parameters" }.Contains(p.Name))
            || root.GetProperty("protocol").GetInt32() != 1 || root.GetProperty("id").GetInt64() <= 0)
            throw new InvalidDataException();
        var operation = root.GetProperty("operation").GetString() ?? throw new InvalidDataException();
        if (!new[] { "connectStep", "disconnectStep", "read", "write", "refresh" }.Contains(operation))
            throw new InvalidDataException();
        var member = root.TryGetProperty("member", out var item) ? item.GetString() ?? "" : "";
        var parameters = root.TryGetProperty("parameters", out var value) ? value.Clone() : default;
        if (parameters.ValueKind != JsonValueKind.Undefined && parameters.ValueKind != JsonValueKind.Object)
            throw new InvalidDataException();
        if (operation is not ("read" or "write") && (member.Length != 0
            || parameters.ValueKind == JsonValueKind.Object && parameters.EnumerateObject().Any()))
            throw new InvalidDataException();
        return new Request { Id = root.GetProperty("id").GetInt64(), Operation = operation, Member = member, Parameters = parameters };
    }
    private static void EnsureUnique(JsonElement value) {
        if (value.ValueKind == JsonValueKind.Object) {
            var names = new HashSet<string>(StringComparer.Ordinal);
            foreach (var property in value.EnumerateObject()) {
                if (!names.Add(property.Name)) throw new InvalidDataException();
                EnsureUnique(property.Value);
            }
        } else if (value.ValueKind == JsonValueKind.Array) {
            foreach (var item in value.EnumerateArray()) EnsureUnique(item);
        }
    }
}
