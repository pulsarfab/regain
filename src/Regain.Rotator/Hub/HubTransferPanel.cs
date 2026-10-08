using System.IO;
using System.Text;
using System.Windows;
using System.Windows.Controls;
using Microsoft.Win32;

namespace Regain.Hub;

public sealed partial class HubConfigurationWindow
{
    private readonly StackPanel transferPanel = new();
    private void RenderTransfer()
    {
        transferPanel.Children.Clear(); var d = session!.TransferDescription;
        var sources = session.SavedConfiguration!.Value.GetProperty("sources").EnumerateArray().ToArray();
        if (sources.Length > 0 && sources.All(source => source.GetProperty("backend").GetProperty("kind").GetString() == "simulated"))
            transferPanel.Children.Add(new TextBlock { Text = d.GetProperty("simulationNotice").GetString(), FontWeight = FontWeights.Bold, TextWrapping = TextWrapping.Wrap, Margin = new Thickness(4) });
        foreach (var key in new[] { "redaction", "limits", "review" }) transferPanel.Children.Add(new TextBlock { Text = d.GetProperty(key).GetString(), TextWrapping = TextWrapping.Wrap, Margin = new Thickness(4) });
        var export = new Button { Content = d.GetProperty("exportLabel").GetString(), Margin = new Thickness(4), Tag = "configuration-export" }; transferPanel.Children.Add(export);
        var mode = new ComboBox { Margin = new Thickness(4), Tag = "configuration-import-mode" };
        foreach (var choice in d.GetProperty("modes").EnumerateArray()) mode.Items.Add(new ComboBoxItem { Content = choice.GetProperty("label").GetString(), Tag = choice.GetProperty("value").GetString(), ToolTip = choice.GetProperty("description").GetString() });
        mode.SelectedIndex = 0; transferPanel.Children.Add(mode);
        var import = new Button { Content = d.GetProperty("importLabel").GetString(), Margin = new Thickness(4), Tag = "configuration-import" }; transferPanel.Children.Add(import);
        var result = new TextBox { IsReadOnly = true, TextWrapping = TextWrapping.Wrap, Margin = new Thickness(4), MinHeight = 100, Tag = "configuration-import-result" }; transferPanel.Children.Add(result);
        export.Click += async (_, _) => await Run(async () => {
            var dialog = new SaveFileDialog { Filter = "JSON configuration (*.json)|*.json", FileName = "regain-hub-configuration.json" };
            if (dialog.ShowDialog(this) != true) return;
            var text = await session.ExportConfigurationAsync(lifetime.Token);
            var bytes = new UTF8Encoding(false).GetBytes(text);
            using (var file = new FileStream(dialog.FileName, FileMode.Create, FileAccess.Write, FileShare.None, 4096, true))
                await file.WriteAsync(bytes, 0, bytes.Length, lifetime.Token);
            status.Text = "Exported the saved configuration with credential bindings omitted. Unsaved draft changes are excluded.";
        });
        import.Click += async (_, _) => await Run(async () => {
            var dialog = new OpenFileDialog { Filter = "JSON configuration (*.json)|*.json", CheckFileExists = true };
            if (dialog.ShowDialog(this) != true) return;
            var maximum = d.GetProperty("maximumDocumentBytes").GetInt32();
            var bytes = await ReadConfigurationFile(dialog.FileName, maximum, lifetime.Token);
            var text = new UTF8Encoding(false, true).GetString(bytes);
            await ImportConfigurationDraft(text, (string)((ComboBoxItem)mode.SelectedItem).Tag);
        });
    }
    internal async Task ImportConfigurationDraft(string text, string mode)
    {
        var prepared = await session!.PrepareImportAsync(text, mode, lifetime.Token);
        form!.Render(); preview.Clear(); errors.Text = "";
        transferPanel.Children.OfType<TextBox>().Single().Text = HubEditorSession.ImportSummary(prepared);
        status.Text = "Imported settings into the draft. Review addresses, identity changes and credential bindings before applying.";
        Controls();
    }
    internal static async Task<byte[]> ReadConfigurationFile(string path, int maximum, CancellationToken token)
    {
        using var file = new FileStream(path, FileMode.Open, FileAccess.Read, FileShare.Read, 4096, true);
        if (file.Length > maximum) throw new InvalidOperationException("Configuration file exceeds the document limit");
        using var buffer = new MemoryStream(); var bytes = new byte[4096];
        int count;
        while ((count = await file.ReadAsync(bytes, 0, bytes.Length, token)) > 0) {
            if (buffer.Length + count > maximum) throw new InvalidOperationException("Configuration file exceeds the document limit");
            buffer.Write(bytes, 0, count);
        }
        return buffer.ToArray();
    }
}
