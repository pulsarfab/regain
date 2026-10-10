using System.Globalization;
using System.IO;
using System.Text.Json;
using System.Windows;
using System.Windows.Controls;
using Microsoft.Win32;

namespace Regain.Hub;

public sealed partial class HubConfigurationWindow
{
    private ComboBox? inspectionSource;
    private Button? inspect, nextInspection;
    private readonly Dictionary<string, TextBox> inspectionParameters = new(StringComparer.Ordinal);
    private uint? nextChannel;
    private void RenderInspection()
    {
        sources.Children.Clear(); inspectionParameters.Clear(); nextChannel = null;
        var host = SourceButton("Saved host status", () => {
            diagnostics.Text = Pretty(session!.HostStatus!.Value);
            status.Text = "Host status observed at the last reload. Reload to obtain a new status.";
        }); sources.Children.Add(host);
        inspectionSource = new ComboBox { MinWidth = 300, MaxWidth = 650, Margin = new Thickness(4), HorizontalAlignment = HorizontalAlignment.Left, Tag = "inspection-source" };
        foreach (var source in session!.SavedConfiguration!.Value.GetProperty("sources").EnumerateArray())
            inspectionSource.Items.Add(new ComboBoxItem { Content = source.GetProperty("label").GetString() + " (" + source.GetProperty("id").GetString() + ")", Tag = source.GetProperty("id").GetGuid() });
        inspectionSource.SelectedIndex = inspectionSource.Items.Count == 0 ? -1 : 0;
        inspectionSource.SelectionChanged += (_, _) => { nextChannel = null; diagnostics.Clear(); Controls(); };
        sources.Children.Add(inspectionSource);
        sources.Children.Add(SourceButton("Read cached source health", async () => {
            if (inspectionSource.SelectedItem is ComboBoxItem selected) await Run(async () => {
                diagnostics.Text = Pretty(await session.SourceStatusAsync((Guid)selected.Tag, lifetime.Token));
                status.Text = "Cached source health. This does not open an equipment connection.";
            });
        }));
        var description = session.InspectionDescription;
        sources.Children.Add(new TextBlock { Text = "Inspect explicitly opens a shared source connection and releases its temporary lease afterward. It does not write equipment or establish a safety vote. Saved source settings are used; apply and reload to inspect new settings.", TextWrapping = TextWrapping.Wrap, Margin = new Thickness(4) });
        var row = new StackPanel { Orientation = Orientation.Horizontal };
        foreach (var field in description.GetProperty("parameters").EnumerateObject()) {
            var column = new StackPanel { Margin = new Thickness(4) };
            column.Children.Add(new TextBlock { Text = field.Value.GetProperty("label").GetString() });
            var input = new TextBox { Text = field.Value.GetProperty("default").GetRawText(), MinWidth = 130,
                Tag = "inspection-" + field.Name, ToolTip = field.Value.GetProperty("description").GetString() +
                    " (" + field.Value.GetProperty("minimum").GetRawText() + "–" + field.Value.GetProperty("maximum").GetRawText() + ")" };
            input.TextChanged += (_, _) => { nextChannel = null; Controls(); };
            inspectionParameters.Add(field.Name, input); column.Children.Add(input); row.Children.Add(column);
        }
        sources.Children.Add(row);
        inspect = SourceButton("Inspect saved source", async () => await Run(() => ReadInspection(null)));
        nextInspection = SourceButton("Inspect next channels", async () => await Run(() => ReadInspection(nextChannel)));
        var buttons = new StackPanel { Orientation = Orientation.Horizontal }; buttons.Children.Add(inspect); buttons.Children.Add(nextInspection); sources.Children.Add(buttons);
        sources.Children.Add(SourceButton("Export setup diagnostics", async () => await Run(() => {
            var snapshot = session.DiagnosticSnapshot();
            var dialog = new SaveFileDialog { Title = "Export observed hub setup diagnostics", FileName = "regain-hub-diagnostics.json", Filter = "JSON diagnostics|*.json", AddExtension = true };
            if (dialog.ShowDialog(this) == true) {
                File.WriteAllText(dialog.FileName, Pretty(snapshot));
                status.Text = "Exported public host status and completed source/output observations. Configuration and credential values are excluded.";
            }
            return Task.CompletedTask;
        })));
    }
    private async Task ReadInspection(uint? next)
    {
        if (inspectionSource?.SelectedItem is not ComboBoxItem selected) throw new InvalidOperationException("Select a saved source");
        var start = next.HasValue ? checked((int)next.Value) : session!.InspectionParameter("start", inspectionParameters["start"].Text);
        var limit = session!.InspectionParameter("limit", inspectionParameters["limit"].Text);
        nextChannel = null; diagnostics.Clear();
        JsonElement result;
        try { result = await session.InspectSourceAsync((Guid)selected.Tag, start, limit, lifetime.Token); }
        finally { if (session.State != HubEditorState.Reviewed) preview.Clear(); }
        diagnostics.Text = Pretty(result);
        var capabilities = result.GetProperty("capabilities");
        if (capabilities.TryGetProperty("nextStart", out var remaining) && remaining.ValueKind == JsonValueKind.Number)
            nextChannel = remaining.GetUInt32();
        // Setting the first channel normally invalidates pagination, so restore
        // the next cursor only after updating the successful page's display.
        var cursor = nextChannel;
        inspectionParameters["start"].Text = start.ToString(CultureInfo.InvariantCulture); nextChannel = cursor;
        status.Text = "Setup inspection completed. Simulation and per-property failures are shown in the result. It does not establish live safety permission. Review configuration again before Apply.";
    }
    private void InspectionControls(bool editable)
    {
        var selected = inspectionSource?.SelectedItem is ComboBoxItem;
        if (inspect is not null) inspect.IsEnabled = !busy && editable && selected;
        if (nextInspection is not null) nextInspection.IsEnabled = !busy && editable && selected && nextChannel.HasValue;
    }
    private Button SourceButton(string label, Action action)
    {
        var button = new Button { Content = label, Margin = new Thickness(4), Padding = new Thickness(10, 6, 10, 6), HorizontalAlignment = HorizontalAlignment.Left };
        button.Click += (_, _) => action(); return button;
    }
}
