using System.Text.Json;
using System.Windows;
using System.Windows.Controls;

namespace Regain.Hub;

public sealed partial class HubConfigurationWindow
{
    private readonly StackPanel simulation = new();
    private readonly StackPanel simulationFields = new();
    private readonly List<SimulationInput> simulationInputs = new();
    private ComboBox? simulationSource;
    private Button? simulationApply;
    private sealed class SimulationInput(HubSimulationControl control, CheckBox include, FrameworkElement input, CheckBox? absent)
    {
        internal HubSimulationControl Control { get; } = control;
        internal CheckBox Include { get; } = include;
        internal FrameworkElement Input { get; } = input;
        internal CheckBox? Absent { get; } = absent;
    }
    private void RenderSimulation()
    {
        simulation.Children.Clear(); simulationInputs.Clear(); simulationFields.Children.Clear();
        var description = session!.SimulationDescription;
        simulation.Children.Add(new TextBlock { Text = description.GetProperty("persistence").GetString() + " " + description.GetProperty("uncertainWrites").GetString(), TextWrapping = TextWrapping.Wrap, Margin = new Thickness(4) });
        simulation.Children.Add(new TextBlock { Text = "These controls change explicitly simulated sources immediately for every connected frontend. Select only the fields to change. Read current state to reset this form; the initial values are defaults for a new runtime.", TextWrapping = TextWrapping.Wrap, Margin = new Thickness(4) });
        simulationSource = new ComboBox { MinWidth = 300, Margin = new Thickness(4), HorizontalAlignment = HorizontalAlignment.Left, Tag = "simulation-source" };
        foreach (var source in session.SavedConfiguration!.Value.GetProperty("sources").EnumerateArray())
            if (description.GetProperty("sourceKinds").EnumerateArray().Any(k => k.GetString() == source.GetProperty("backend").GetProperty("kind").GetString()))
                simulationSource.Items.Add(new ComboBoxItem { Content = source.GetProperty("label").GetString(), Tag = source.GetProperty("id").GetGuid() });
        simulationSource.SelectionChanged += (_, _) => { SimulationFields(null); Controls(); };
        simulation.Children.Add(simulationSource);
        simulation.Children.Add(SourceButton("Read current simulation and reset form", async () => await Run(async () => {
            if (simulationSource.SelectedItem is not ComboBoxItem source) throw new InvalidOperationException("Select an explicitly simulated source");
            var result = await session.SourceStatusAsync((Guid)source.Tag, lifetime.Token);
            SimulationFields(result.GetProperty("simulation"));
            status.Text = "Current shared simulation state loaded. No equipment connection was opened. Select the fields to change.";
        })));
        simulation.Children.Add(simulationFields);
        simulationApply = SourceButton("Apply selected simulation changes", async () => await Run(async () => {
            if (simulationSource.SelectedItem is not ComboBoxItem source) throw new InvalidOperationException("Select an explicitly simulated source");
            var patch = HubEditorSession.SimulationPatch(simulationInputs.Where(i => i.Include.IsChecked == true).Select(i =>
                new KeyValuePair<HubSimulationControl, JsonElement>(i.Control, i.Control.Parse(SimulationText(i), i.Absent?.IsChecked == true))));
            JsonElement result;
            try { result = await session.UpdateSimulationAsync((Guid)source.Tag, patch, lifetime.Token); }
            finally { if (session.State != HubEditorState.Reviewed) preview.Clear(); }
            SimulationFields(result.GetProperty("simulation"));
            status.Text = "Selected simulation changes applied to the shared runtime. Configuration is unchanged. Safety uses normal polling and confirmation; uncertain writes still require lease reconciliation.";
        }));
        simulation.Children.Add(simulationApply);
        simulationSource.SelectedIndex = simulationSource.Items.Count == 0 ? -1 : 0;
    }
    private void SimulationFields(JsonElement? current)
    {
        simulationFields.Children.Clear(); simulationInputs.Clear();
        if (simulationSource?.SelectedItem is not ComboBoxItem source) return;
        foreach (var control in session!.SimulationControls((Guid)source.Tag)) {
            var panel = new StackPanel { Margin = new Thickness(4, 8, 4, 4) };
            var include = new CheckBox { Content = "Change " + control.Label, Tag = "simulation-change-" + string.Join("-", control.Path) };
            var value = current.HasValue ? control.Read(current.Value) : control.Default;
            FrameworkElement input;
            if (control.Type == "boolean") input = new CheckBox { Content = "True", IsChecked = value.ValueKind == JsonValueKind.True };
            else if (control.Type == "string") {
                var choice = new ComboBox { MinWidth = 240, HorizontalAlignment = HorizontalAlignment.Left };
                foreach (var option in control.Descriptor.GetProperty("enum").EnumerateArray()) choice.Items.Add(option.GetString()!);
                choice.SelectedItem = value.GetString(); input = choice;
            } else input = new TextBox { MinWidth = 240, HorizontalAlignment = HorizontalAlignment.Left, Text = value.ValueKind == JsonValueKind.Null ? "" : value.GetRawText() };
            input.Tag = "simulation-value-" + string.Join("-", control.Path);
            CheckBox? absent = control.Descriptor.TryGetProperty("nullable", out var nullable) && nullable.ValueKind == JsonValueKind.True
                ? new() { Content = "Sensor absent", IsChecked = value.ValueKind == JsonValueKind.Null } : null;
            void Enabled() { input.IsEnabled = include.IsChecked == true && absent?.IsChecked != true; if (absent is not null) absent.IsEnabled = include.IsChecked == true; Controls(); }
            include.Click += (_, _) => Enabled(); if (absent is not null) absent.Click += (_, _) => Enabled();
            input.IsEnabled = false; if (absent is not null) absent.IsEnabled = false;
            panel.Children.Add(include); panel.Children.Add(new TextBlock { Text = control.Descriptor.GetProperty("description").GetString(), TextWrapping = TextWrapping.Wrap });
            var bounds = new List<string>();
            if (control.Descriptor.TryGetProperty("minimum", out var minimum)) bounds.Add("Minimum " + minimum.GetRawText());
            if (control.Descriptor.TryGetProperty("exclusiveMinimum", out var exclusive)) bounds.Add("Greater than " + exclusive.GetRawText());
            if (control.Descriptor.TryGetProperty("maximum", out var maximum)) bounds.Add("Maximum " + maximum.GetRawText());
            if (control.Descriptor.TryGetProperty("step", out var step)) bounds.Add("Step " + step.GetRawText());
            if (control.Descriptor.TryGetProperty("minItems", out var minItems)) bounds.Add("Minimum items " + minItems.GetRawText());
            if (control.Descriptor.TryGetProperty("maxItems", out var maxItems)) bounds.Add("Maximum items " + maxItems.GetRawText());
            if (control.Descriptor.TryGetProperty("maxUtf8Bytes", out var textBytes)) bounds.Add("Maximum UTF-8 bytes " + textBytes.GetRawText());
            if (bounds.Count > 0) panel.Children.Add(new TextBlock { Text = string.Join(" · ",bounds),TextWrapping = TextWrapping.Wrap });
            panel.Children.Add(input); if (absent is not null) panel.Children.Add(absent);
            simulationInputs.Add(new(control, include, input, absent)); simulationFields.Children.Add(panel);
        }
    }
    private static string SimulationText(SimulationInput input) => input.Input switch {
        CheckBox flag => (flag.IsChecked == true).ToString(), ComboBox choice => (string?)choice.SelectedItem ?? "", TextBox number => number.Text, _ => ""
    };
    private void SimulationControls(bool editable)
    {
        simulation.IsEnabled = !busy && session is not null && session.State != HubEditorState.Uncertain;
        if (simulationApply is not null) simulationApply.IsEnabled = !busy && editable && simulationInputs.Any(i => i.Include.IsChecked == true);
    }
}
