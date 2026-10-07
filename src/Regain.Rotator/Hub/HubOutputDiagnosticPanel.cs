using System.Globalization;
using System.Text.Json;
using System.Windows;
using System.Windows.Controls;

namespace Regain.Hub;

public sealed partial class HubConfigurationWindow
{
    private readonly StackPanel outputControls = new();
    private readonly TextBlock outputSummary = new() { Margin = new Thickness(8), TextWrapping = TextWrapping.Wrap, Tag = "output-summary" };
    private readonly TextBox outputResult = new() { IsReadOnly = true, AcceptsReturn = true, TextWrapping = TextWrapping.Wrap,
        VerticalScrollBarVisibility = ScrollBarVisibility.Auto, Tag = "output-result" };
    private ComboBox? diagnosticOutput;
    private Button? readOutput, nextOutput;
    private readonly Dictionary<string, TextBox> diagnosticParameters = new(StringComparer.Ordinal);
    private int? nextOutputItem;
    private void AddOutputDiagnosticsTab()
    {
        var panel = new DockPanel();
        var top = new StackPanel(); top.Children.Add(outputControls); top.Children.Add(outputSummary);
        var scroll = new ScrollViewer { Content = top, VerticalScrollBarVisibility = ScrollBarVisibility.Auto, MaxHeight = 450 };
        DockPanel.SetDock(scroll, Dock.Top); panel.Children.Add(scroll); panel.Children.Add(outputResult);
        tabs.Items.Add(new TabItem { Header = "Output health", Content = panel });
    }
    private void RenderOutputDiagnostics()
    {
        outputControls.Children.Clear(); diagnosticParameters.Clear(); nextOutputItem = null; outputSummary.Text = ""; outputResult.Clear();
        outputControls.Children.Add(new TextBlock { Text = "Read cached output health to see safety decisions, switch channels, weather readings and typed device properties. This does not connect equipment, refresh sensors or count a safety observation. Saved settings are used.", TextWrapping = TextWrapping.Wrap, Margin = new Thickness(4) });
        diagnosticOutput = new ComboBox { MinWidth = 300, MaxWidth = 650, Margin = new Thickness(4), HorizontalAlignment = HorizontalAlignment.Left, Tag = "diagnostic-output" };
        foreach (var output in session!.SavedConfiguration!.Value.GetProperty("outputs").EnumerateArray()) diagnosticOutput.Items.Add(new ComboBoxItem { Content = output.GetProperty("label").GetString(), Tag = output.GetProperty("id").GetGuid() });
        diagnosticOutput.SelectedIndex = diagnosticOutput.Items.Count == 0 ? -1 : 0;
        diagnosticOutput.SelectionChanged += (_, _) => { nextOutputItem = null; outputSummary.Text = ""; outputResult.Clear(); Controls(); }; outputControls.Children.Add(diagnosticOutput);
        var row = new StackPanel { Orientation = Orientation.Horizontal };
        foreach (var field in session.OutputDiagnosticDescription.GetProperty("parameters").EnumerateObject()) {
            var column = new StackPanel { Margin = new Thickness(4) }; column.Children.Add(new TextBlock { Text = field.Value.GetProperty("label").GetString() });
            var input = new TextBox { Text = field.Value.GetProperty("default").GetRawText(), MinWidth = 130, Tag = "diagnostic-" + field.Name, ToolTip = field.Value.GetProperty("description").GetString() };
            input.TextChanged += (_, _) => { nextOutputItem = null; Controls(); }; diagnosticParameters.Add(field.Name, input); column.Children.Add(input); row.Children.Add(column);
        }
        outputControls.Children.Add(row);
        readOutput = SourceButton("Read cached output health", async () => await Run(() => ReadOutputDiagnostics(null)));
        nextOutput = SourceButton("Read next output items", async () => await Run(() => ReadOutputDiagnostics(nextOutputItem)));
        var buttons = new StackPanel { Orientation = Orientation.Horizontal }; buttons.Children.Add(readOutput); buttons.Children.Add(nextOutput); outputControls.Children.Add(buttons);
        outputControls.Children.Add(SourceButton("Export observed output diagnostics", async () => await Run(() => {
            var dialog = new Microsoft.Win32.SaveFileDialog { Title = "Export observed hub diagnostics", FileName = "regain-hub-diagnostics.json", Filter = "JSON diagnostics|*.json", AddExtension = true };
            var snapshot = session.DiagnosticSnapshot();
            if (dialog.ShowDialog(this) == true) { System.IO.File.WriteAllText(dialog.FileName, Pretty(snapshot)); status.Text = "Exported observed host/source/output status. Editable configuration and credentials are excluded."; }
            return Task.CompletedTask;
        })));
    }
    private async Task ReadOutputDiagnostics(int? next)
    {
        if (diagnosticOutput?.SelectedItem is not ComboBoxItem selected) throw new InvalidOperationException("Select a saved output");
        var start = next ?? session!.DiagnosticParameter("start", diagnosticParameters["start"].Text); var limit = session!.DiagnosticParameter("limit", diagnosticParameters["limit"].Text);
        nextOutputItem = null; outputSummary.Text = ""; outputResult.Clear();
        JsonElement result;
        try { result = await session.OutputStatusAsync((Guid)selected.Tag, start, limit, lifetime.Token); }
        finally { if (session.State != HubEditorState.Reviewed) preview.Clear(); }
        outputResult.Text = Pretty(result); outputSummary.Text = OutputDiagnosticSummary(result);
        diagnosticParameters["start"].Text = start.ToString(CultureInfo.InvariantCulture);
        if (result.GetProperty("nextStart").ValueKind != JsonValueKind.Null) nextOutputItem = result.GetProperty("nextStart").GetInt32();
        status.Text = "Cached output observed. No equipment connection or safety confirmation was started. The export retains this observation's time and revision.";
    }
    private void OutputDiagnosticControls(bool editable)
    {
        outputControls.IsEnabled = !busy && session is not null;
        if (readOutput is not null) readOutput.IsEnabled = !busy && editable && diagnosticOutput?.SelectedItem is ComboBoxItem;
        if (nextOutput is not null) nextOutput.IsEnabled = !busy && editable && diagnosticOutput?.SelectedItem is ComboBoxItem && nextOutputItem.HasValue;
    }
    internal static string OutputDiagnosticSummary(JsonElement result)
    {
        var d = result.GetProperty("diagnostics"); var kind = d.GetProperty("kind").GetString();
        var lines = new List<string> { (result.GetProperty("simulated").GetBoolean() ? "Simulation · " : "") + "Cached " + kind + " output · revision " + result.GetProperty("configurationRevision").GetString() };
        string Number(JsonElement value) => value.GetDouble().ToString("0.0", CultureInfo.InvariantCulture);
        if (kind == "camera") {
            var acquisition = d.GetProperty("acquisition");
            if (acquisition.ValueKind != JsonValueKind.Null) {
                lines.Add("Acquisition " + acquisition.GetProperty("phase").GetString() + " · image " + (acquisition.GetProperty("imageReady").GetBoolean() ? "ready" : "not ready"));
                var guide=acquisition.GetProperty("guiding");
                if (guide.ValueKind != JsonValueKind.Null) {
                    lines.Add("Guiding " + guide.GetProperty("phase").GetString());
                    if (guide.GetProperty("error").ValueKind != JsonValueKind.Null) lines.Add(guide.GetProperty("error").GetProperty("message").GetString()!);
                }
                if (acquisition.GetProperty("error").ValueKind != JsonValueKind.Null) lines.Add(acquisition.GetProperty("error").GetProperty("message").GetString()!);
            }
            lines.Add(PollingSummary(d.GetProperty("health")));
        } else if (kind == "safety") {
            lines.Add((d.GetProperty("isSafe").GetBoolean() ? "SAFE" : "UNSAFE") + " · " + (d.GetProperty("controllerActive").GetBoolean() ? "Controller active" : "Controller inactive"));
            foreach (var member in d.GetProperty("members").EnumerateArray()) {
                var s = member.GetProperty("decision"); var policy = member.GetProperty("policy");
                lines.Add(PollingSummary(member.GetProperty("health")));
                if (member.GetProperty("health").GetProperty("writeUncertain").GetBoolean()) lines.Add("Retained uncertain write; reconcile equipment state before another command.");
                if (!member.GetProperty("enabled").GetBoolean()) { lines.Add(member.GetProperty("source").GetString() + ": Disabled; no vote."); continue; }
                var raw = s.GetProperty("rawIsSafe");
                lines.Add(member.GetProperty("source").GetString() + ": " + s.GetProperty("phase").GetString() + " · raw " + (raw.ValueKind == JsonValueKind.Null ? "unknown" : raw.GetBoolean() ? "safe" : "unsafe") + " · effective " + (s.GetProperty("permitsSafe").GetBoolean() ? "safe" : "unsafe") + " · " + s.GetProperty("reason").GetString());
                lines.Add("Failed checks " + s.GetProperty("failedCycles") + "/" + policy.GetProperty("failedCyclesToUnsafe") + "; unsafe readings " + s.GetProperty("unsafeReadings") + "/" + policy.GetProperty("unsafeReadingsToUnsafe") + "; recovery " + s.GetProperty("safeReadings") + "/" + policy.GetProperty("safeReadingsToSafe") + " safe readings, " + Number(s.GetProperty("safeHoldSeconds")) + "/" + policy.GetProperty("returnToSafeHoldSeconds") + " s hold; safe age " + (s.GetProperty("safeAgeSeconds").ValueKind == JsonValueKind.Null ? "unknown" : Number(s.GetProperty("safeAgeSeconds")) + " s") + "/" + policy.GetProperty("maximumSafeAgeSeconds") + " s.");
            }
        } else if (kind == "focuser" || kind == "rotator" || kind == "filterwheel" || kind == "covercalibrator") {
            foreach (var item in d.GetProperty("properties").EnumerateArray()) {
                var sample = item.GetProperty("sample"); var label = item.GetProperty("property").GetString();
                if (sample.GetProperty("state").GetString() == "available") {
                    var reading = sample.GetProperty("reading"); lines.Add(label + ": " + reading.GetProperty("value").GetProperty("value") + " · age " + Number(reading.GetProperty("ageSeconds")) + " s");
                } else lines.Add(label + ": unavailable · " + sample.GetProperty("error").GetProperty("message").GetString());
            }
            if (d.GetProperty("health").GetProperty("writeUncertain").GetBoolean()) lines.Add("Retained uncertain write; reconcile equipment state before another command.");
            lines.Add(PollingSummary(d.GetProperty("health")));
        } else {
            foreach (var item in d.GetProperty(kind == "switch" ? "channels" : "measurements").EnumerateArray()) {
                if (kind == "switch" && item.GetProperty("state").GetString() == "removed") { lines.Add("Channel " + item.GetProperty("number") + ": removed; number reserved."); continue; }
                var label = kind == "switch" ? "Channel " + item.GetProperty("number") + ": " + item.GetProperty("label").GetString() : item.GetProperty("metric").GetString(); var sample = item.GetProperty("sample");
                if (sample.GetProperty("state").GetString() == "available") { var reading = sample.GetProperty("reading"); lines.Add(label + ": " + reading.GetProperty("value") + " " + (kind == "switch" ? item.GetProperty("units").GetString() : reading.GetProperty("unit").GetString()) + " · age " + Number(reading.GetProperty("ageSeconds")) + " s"); }
                else lines.Add(label + ": unavailable · " + sample.GetProperty("error").GetProperty("message").GetString());
                if (kind == "switch") lines.Add("Configured " + (item.GetProperty("configuredWritable").GetBoolean() ? "writable" : "read-only") + "; operational write capability is checked separately." + (item.GetProperty("health").GetProperty("writeUncertain").GetBoolean() ? " Retained uncertain write." : ""));
                if (kind == "switch") lines.Add(PollingSummary(item.GetProperty("health")));
                else foreach (var health in item.GetProperty("sources").EnumerateArray()) lines.Add(PollingSummary(health));
            }
        }
        lines.Add("Observed cache only. No equipment connection or safety confirmation is started."); return string.Join("\n", lines);
    }
    private static string PollingSummary(JsonElement health)
    {
        var p = health.GetProperty("polling"); var reason = p.GetProperty("reason"); var next = p.GetProperty("nextPollAfterSeconds"); var exhausted = p.GetProperty("lastCycleExhausted");
        string Number(JsonElement value, string format) => value.GetDouble().ToString(format, CultureInfo.InvariantCulture);
        return "Source " + health.GetProperty("source").GetString() + ": polling " + p.GetProperty("phase").GetString() + (reason.ValueKind == JsonValueKind.Null ? "" : " · " + reason.GetString()) +
            ". Host observation " + Number(p.GetProperty("observedSeconds"), "0.0") + " s; " + (next.ValueKind == JsonValueKind.Null ? "no scheduled wait reported" : "next poll scheduled in " + Number(next, "0.000") + " s at that observation; actor work may delay it") +
            ". Read attempts started " + p.GetProperty("attemptsStarted") + "/" + p.GetProperty("attemptsPerCycle") + "; last " + p.GetProperty("lastAttempt") +
            (exhausted.ValueKind == JsonValueKind.Null ? "" : exhausted.GetBoolean() ? " (cycle complete)" : " (cycle pending)") + "; backoff failures " + p.GetProperty("backoffFailures") + ".";
    }
}
