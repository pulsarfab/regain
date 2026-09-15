using System.Diagnostics;
using System.Net.Http;
using System.Text.Json;
using System.Windows.Forms;

namespace ZwoGain.Ascom;

public sealed class ServerSettings
{
    public string Address { get; set; } = "127.0.0.1";
    public int Port { get; set; } = 11111;
    public bool StartLocalServer { get; set; } = true;
    private static string Path => Environment.GetEnvironmentVariable("ZWOGAIN_ASCOM_SETTINGS") ?? System.IO.Path.Combine(Environment.GetFolderPath(Environment.SpecialFolder.LocalApplicationData), "ZwoGain", "ASCOM", "server.json");
    internal string Url => $"http://{Address}:{Port}";
    public static ServerSettings Load() => File.Exists(Path) ? JsonSerializer.Deserialize<ServerSettings>(File.ReadAllText(Path))! : new();
    internal void Validate()
    {
        if (!System.Net.IPAddress.TryParse(Address, out var address) || address.AddressFamily != System.Net.Sockets.AddressFamily.InterNetwork || Port is < 1 or > 65535)
            throw new ArgumentException("Enter an IPv4 address and a port between 1 and 65535");
        if (StartLocalServer && !System.Net.IPAddress.IsLoopback(address)) throw new ArgumentException("Automatic startup is only available for a local server");
    }
    public void Save() { Validate(); Directory.CreateDirectory(System.IO.Path.GetDirectoryName(Path)!); string temporary = Path + ".tmp"; File.WriteAllText(temporary, JsonSerializer.Serialize(this)); if (File.Exists(Path)) File.Replace(temporary, Path, null); else File.Move(temporary, Path); }
    internal void EnsureServer()
    {
        Validate();
        using var gate = new Mutex(false, "Local\\ZwoGain-Alpaca-" + Port);
        bool acquired = false;
        try
        {
            try { acquired = gate.WaitOne(TimeSpan.FromSeconds(20)); } catch (AbandonedMutexException) { acquired = true; }
            if (!acquired) throw new TimeoutException("Timed out waiting for the local camera server");
            if (Probe()) return;
            if (!StartLocalServer) throw new IOException("The configured Alpaca server is unavailable");
            string directory = System.IO.Path.GetDirectoryName(typeof(ServerSettings).Assembly.Location)!;
            string executable = System.IO.Path.Combine(directory, "zwogain-alpaca.exe");
            if (!File.Exists(executable)) throw new FileNotFoundException("Install the Rust Alpaca server beside the ASCOM frontend", executable);
            Process.Start(new ProcessStartInfo(executable, "--port " + Port) { UseShellExecute = false, CreateNoWindow = true, WindowStyle = ProcessWindowStyle.Hidden, WorkingDirectory = directory })?.Dispose();
            var clock = Stopwatch.StartNew();
            while (clock.Elapsed.TotalSeconds < 15) { if (Probe()) return; Thread.Sleep(100); }
            throw new IOException("ZWOgain did not start. Check its log or select a different port.");
        }
        finally { if (acquired) gate.ReleaseMutex(); }
    }
    private bool Probe()
    {
        try
        {
            using var http = new HttpClient { Timeout = TimeSpan.FromSeconds(1) };
            using var json = JsonDocument.Parse(http.GetStringAsync(Url + "/management/v1/description").GetAwaiter().GetResult());
            if (json.RootElement.GetProperty("Value").GetProperty("ServerName").GetString() != "ZWOgain") throw new InvalidOperationException("This address belongs to a different Alpaca server");
            return true;
        }
        catch (Exception e) when (e is HttpRequestException or TaskCanceledException) { return false; }
    }
    internal void OpenCameraSetup(int slot)
    {
        EnsureServer();
        using var http = new HttpClient { Timeout = TimeSpan.FromSeconds(5) };
        using var state = JsonDocument.Parse(http.GetStringAsync(Url + "/setup/api/state").GetAwaiter().GetResult());
        for (int count = state.RootElement.GetProperty("cameras").GetArrayLength(); count < 4; count++)
        {
            using var response = http.PostAsync(Url + "/setup/api/slots", new StringContent("{}", System.Text.Encoding.UTF8, "application/json")).GetAwaiter().GetResult();
            response.EnsureSuccessStatusCode();
        }
        Process.Start(new ProcessStartInfo(Url + $"/setup/v1/camera/{slot}/setup") { UseShellExecute = true });
    }
}

internal sealed class SetupForm : Form
{
    internal SetupForm(ServerSettings settings, int slot)
    {
        Text = $"ZWOgain Camera {slot + 1}"; Width = 480; Height = 275; FormBorderStyle = FormBorderStyle.FixedDialog; MaximizeBox = MinimizeBox = false; StartPosition = FormStartPosition.CenterScreen;
        var label = new Label { Text = "Alpaca server address", Left = 20, Top = 20, AutoSize = true };
        var address = new TextBox { Text = settings.Address, Left = 20, Top = 45, Width = 290 };
        var port = new NumericUpDown { Minimum = 1, Maximum = 65535, Value = settings.Port, Left = 325, Top = 45, Width = 110 };
        var auto = new CheckBox { Text = "Start the local Rust server when needed", Checked = settings.StartLocalServer, Left = 20, Top = 85, Width = 410 };
        var camera = new Button { Text = "Camera settings…", Left = 20, Top = 125, Width = 180, Height = 32 };
        var save = new Button { Text = "Save", Left = 245, Top = 185, Width = 90 }; var cancel = new Button { Text = "Cancel", Left = 345, Top = 185, Width = 90, DialogResult = DialogResult.Cancel };
        void Read() { settings.Address = address.Text.Trim(); settings.Port = (int)port.Value; settings.StartLocalServer = auto.Checked; settings.Validate(); }
        camera.Click += (_, _) => { try { Read(); settings.OpenCameraSetup(slot); } catch (Exception e) { MessageBox.Show(this, e.Message, "ZWOgain", MessageBoxButtons.OK, MessageBoxIcon.Warning); } };
        save.Click += (_, _) => { try { Read(); DialogResult = DialogResult.OK; } catch (Exception e) { MessageBox.Show(this, e.Message, "ZWOgain", MessageBoxButtons.OK, MessageBoxIcon.Warning); } };
        Controls.AddRange([label, address, port, auto, camera, save, cancel]); AcceptButton = save; CancelButton = cancel;
    }
}
