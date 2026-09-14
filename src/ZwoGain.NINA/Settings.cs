using System.Text.Json;
using System.Windows;
using System.Windows.Controls;
using ZwoGain.Core;

namespace ZwoGain.NINA;

internal static class Settings
{
    private static readonly string Folder = Path.Combine(Environment.GetFolderPath(Environment.SpecialFolder.LocalApplicationData), "ZwoGain");
    private static readonly string FilePath = Path.Combine(Folder, "recovery.json");
    public static RecoveryOptions Load()
    {
        var options = File.Exists(FilePath) ? JsonSerializer.Deserialize<RecoveryOptions>(File.ReadAllText(FilePath)) ?? new() : new RecoveryOptions();
        options.Validate();
        return options;
    }
    public static void Show()
    {
        var panel = new StackPanel { Margin = new Thickness(16) };
        panel.Children.Add(new TextBlock { Text = "Recovery settings apply on the next connection.", Margin = new Thickness(0, 0, 0, 12) });
        var entries = new Dictionary<string, TextBox>();
        var current = Load();
        var labels = new Dictionary<string, string>
        {
            [nameof(RecoveryOptions.MaxRetries)] = "Recovery retries after the initial attempt",
            [nameof(RecoveryOptions.ReconnectDelaySeconds)] = "USB reconnect delay (seconds)",
            [nameof(RecoveryOptions.CommandTimeoutSeconds)] = "SDK command timeout (seconds)",
            [nameof(RecoveryOptions.DownloadTimeoutSeconds)] = "Download watchdog (seconds)",
            [nameof(RecoveryOptions.ExposureGraceSeconds)] = "Exposure completion grace (seconds)",
            [nameof(RecoveryOptions.CoolingTimeoutSeconds)] = "Cooling recovery deadline (seconds)",
            [nameof(RecoveryOptions.TemperatureToleranceC)] = "Prior temperature tolerance (C)",
            [nameof(RecoveryOptions.CoolingStableSamples)] = "Consecutive cooling samples",
            [nameof(RecoveryOptions.CoolingSampleSeconds)] = "Cooling sample interval (seconds)",
            [nameof(RecoveryOptions.ReadyFrameDownloadRetries)] = "Experimental re-downloads while SDK reports success (default 0)"
        };
        foreach (var property in typeof(RecoveryOptions).GetProperties())
        {
            panel.Children.Add(new TextBlock { Text = labels[property.Name] });
            var field = new TextBox { Text = Convert.ToString(property.GetValue(current), System.Globalization.CultureInfo.InvariantCulture), Margin = new Thickness(0, 2, 0, 8) };
            entries[property.Name] = field;
            panel.Children.Add(field);
        }
        var status = new TextBlock { TextWrapping = TextWrapping.Wrap };
        panel.Children.Add(status);
        var button = new Button { Content = "Save", Padding = new Thickness(16, 6, 16, 6) };
        panel.Children.Add(button);
        var window = new Window { Title = "ZwoGain recovery", Width = 510, SizeToContent = SizeToContent.Height, Content = panel, WindowStartupLocation = WindowStartupLocation.CenterScreen };
        button.Click += (_, _) =>
        {
            try
            {
                var values = entries.ToDictionary(k => k.Key, k => double.Parse(k.Value.Text, System.Globalization.CultureInfo.InvariantCulture));
                var options = JsonSerializer.Deserialize<RecoveryOptions>(JsonSerializer.Serialize(values))!;
                options.Validate();
                Directory.CreateDirectory(Folder);
                string temp = FilePath + "." + Guid.NewGuid().ToString("N") + ".tmp";
                File.WriteAllText(temp, JsonSerializer.Serialize(options, new JsonSerializerOptions { WriteIndented = true }));
                File.Move(temp, FilePath, true);
                window.Close();
            }
            catch (Exception e) { status.Text = e.Message; }
        };
        window.ShowDialog();
    }
}
