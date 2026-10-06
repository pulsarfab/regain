using System.Text.Json;
using Regain.Hub;
using Regain.Hub.ASCOM;

internal static partial class NativeOutputs
{
    internal static async Task CreatedTypedRun(string executable, string path, Guid instance,
        HubEditorSession editor, string deviceType, CancellationToken token)
    {
        await editor.ReloadAsync(token);
        var draft = editor.Draft!;
        int sourceIndex = draft.Candidate.GetProperty("sources").GetArrayLength();
        string sourcePath = "/sources/" + sourceIndex;
        draft.AddItem("/sources"); draft.SelectVariant(sourcePath + "/backend", "simulated");
        draft.SetValue(sourcePath + "/backend/deviceType", draft.ParseScalar(draft.Field(sourcePath + "/backend/deviceType").Schema, deviceType));
        draft.SetValue(sourcePath + "/label", JsonSerializer.SerializeToElement("Created simulated " + deviceType));
        var source = draft.Field(sourcePath + "/id").Value!.Value.GetGuid();
        int start = draft.Candidate.GetProperty("outputs").GetArrayLength();
        var ids = new List<Guid>();
        for (int i = 0; i < 2; i++) {
            string output = "/outputs/" + (start + i);
            draft.AddItem("/outputs"); draft.SelectVariant(output + "/device", "proxy");
            draft.SetValue(output + "/device/deviceType", draft.ParseScalar(draft.Field(output + "/device/deviceType").Schema, deviceType));
            draft.SetValue(output + "/device/source", JsonSerializer.SerializeToElement(source));
            draft.SetValue(output + "/label", JsonSerializer.SerializeToElement("Created simulated " + deviceType + " " + i));
            draft.SetValue(output + "/number", JsonSerializer.SerializeToElement(10 + i));
            ids.Add(draft.Field(output + "/id").Value!.Value.GetGuid());
        }
        editor.Changed(); Require(await editor.ReviewAsync(token), "Created typed setup review");
        foreach (var savedSource in draft.Candidate.GetProperty("sources").EnumerateArray()) {
            var id = savedSource.GetProperty("id").GetGuid();
            // The new source is not live until Apply. Existing clients have
            // disconnected, but their pipe-close releases still need to drain.
            if (id == source) continue;
            while ((await editor.SourceStatusAsync(id, token)).GetProperty("leaseCount").GetInt32() != 0)
                await Task.Delay(25, token);
        }
        await editor.ApplyAsync(token); await editor.ReloadAsync(token);
        Require((await editor.SourceStatusAsync(source, token)).GetProperty("leaseCount").GetInt32() == 0, "Creation is inert");
        for (int i = 0; i < 2; i++)
            Require(editor.Draft!.Field("/outputs/" + (start + i) + "/id").Value!.Value.GetGuid() == ids[i], "Created identity survives reload");
        HubSelection Binding(int i) => new() { ConfigPath = path, InstanceId = instance, OutputId = ids[i],
            DeviceType = deviceType, Label = "Created simulated " + deviceType + " " + i, Simulated = true };
        if (deviceType == "rotator") {
            using var first = new RotatorOutput(Binding(0), executable);
            using var second = new RotatorOutput(Binding(1), executable);
            first.Connected = true; second.Connected = true;
            first.Sync(42.5f); first.Move(-721.5f); await Until(() => !second.IsMoving, token);
            Require(second.Position == 41 && second.MechanicalPosition == 358.5f, "Created outputs share source coordinates");
            first.Connected = false; Require(second.Connected, "Created output lease independence");
            second.Connected = false;
        } else {
            using var first = new FilterWheelOutput(Binding(0), executable);
            using var second = new FilterWheelOutput(Binding(1), executable);
            first.Connected = true; second.Connected = true;
            Require(first.Names.Length == 7 && second.FocusOffsets.SequenceEqual(new int[7]), "Created wheel metadata");
            first.Position = 4; await Until(() => second.Position == 4, token);
            first.Connected = false; Require(second.Connected && second.Position == 4, "Created wheel lease independence");
            second.Connected = false;
        }
        while ((await editor.SourceStatusAsync(source, token)).GetProperty("leaseCount").GetInt32() != 0)
            await Task.Delay(25, token);
        Console.WriteLine($"ASCOM created {deviceType} {IntPtr.Size * 8}-bit: metadata-driven creation, inert review/apply, saved IDs and shared independent outputs passed");
    }
}
