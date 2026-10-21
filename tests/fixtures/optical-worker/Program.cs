// Test-only USB worker protocol fixture. Never opens hardware.
using System.Text.Json;
using System.Text.Json.Nodes;

string root = Environment.GetEnvironmentVariable("REGAIN_OPTICAL_FIXTURE_DIRECTORY") ?? Path.Combine(AppContext.BaseDirectory, "state");
Directory.CreateDirectory(root);
string kind = args.Contains("caa") ? "caa" : "efw";
string serial = kind == "caa" ? "0102030405060708" : "0102030405060709";
string statePath = Path.Combine(root, kind + "-state.json");
JsonObject Initial() => kind == "caa"
    ? new() { ["mechanical_degrees"] = 30.0, ["logical_degrees"] = 30.0, ["logical_offset"] = 0.0, ["target_degrees"] = 30.0, ["moving"] = false, ["limit_degrees"] = 360, ["error"] = 0 }
    : new() { ["position"] = 3, ["slots"] = 7, ["moving"] = false, ["calibrating"] = false, ["error"] = 0 };
JsonObject State() => File.Exists(statePath) ? JsonNode.Parse(File.ReadAllText(statePath))!.AsObject() : Initial();
object Identity() => new { serial, model = kind == "caa" ? "CAA fixture" : "EFW fixture" };
void Log(string command) {
    // Multiple short-lived fixture workers can log concurrently during cancellation tests.
    File.AppendAllText(Path.Combine(root, kind + "-" + Environment.ProcessId + ".jsonl"),
        JsonSerializer.Serialize(new { time = DateTimeOffset.UtcNow, command, pid = Environment.ProcessId }) + "\n");
}
if (args.Contains("list-details")) {
    Console.WriteLine(JsonSerializer.Serialize(new[] { new { identity = Identity(), status = State() } })); return;
}
while (Console.ReadLine() is { } line) {
    var request = JsonNode.Parse(line.TrimStart('\uFEFF'))!;
    string command = request["command"]!.GetValue<string>();
    Log(command);
    if (File.Exists(Path.Combine(root, kind + "-removed"))) {
        Console.WriteLine(JsonSerializer.Serialize(new { ok = false, error = "HID input report: test USB device removed" })); continue;
    }
    // Lose one status response but allow the replacement worker to reopen.
    string once = Path.Combine(root, kind + "-fail-once");
    if (command == "status" && File.Exists(once)) {
        File.Delete(once);
        Console.WriteLine(JsonSerializer.Serialize(new { ok = false, error = "HID input report: injected spontaneous disconnect" })); continue;
    }
    object? result = null;
    var state = State();
    switch (command) {
        case "identity": result = Identity(); break;
        case "status": result = state; break;
        case "settings": result = new { reverse = state["reverse"]?.GetValue<bool>() ?? false }; break;
        case "reverse":
            state["reverse"] = request["enabled"]!.GetValue<bool>();
            File.WriteAllText(statePath, state.ToJsonString()); break;
        case "sync":
            state["logical_degrees"] = request["degrees"]!.GetValue<double>();
            state["logical_offset"] = state["logical_degrees"]!.GetValue<double>() - state["mechanical_degrees"]!.GetValue<double>();
            File.WriteAllText(statePath, state.ToJsonString()); break;
        case "move":
            state["position"] = request["position"]!.GetValue<int>();
            File.WriteAllText(statePath, state.ToJsonString());
            if (File.Exists(Path.Combine(root, kind + "-lose-move-ack"))) return;
            break;
        case "stop": break;
        default: throw new Exception("Unsupported fixture command: " + command);
    }
    Console.WriteLine(JsonSerializer.Serialize(new { ok = true, result }));
}
