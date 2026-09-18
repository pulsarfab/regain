using System.Drawing;
using System.Globalization;
using System.Runtime.InteropServices;
using System.Text.Json;
using System.Windows.Forms;

namespace ZwoGain.Rotator;

public sealed class CaaSetupForm : Form
{
    [DllImport("user32.dll")]
    private static extern IntPtr SetThreadDpiAwarenessContext(IntPtr context);
    private readonly System.Windows.Forms.Timer timer = new() { Interval = 750 };
    private readonly List<Control> offline = new(), connected = new(), idle = new();
    private bool busy, polling, moving, closing;

    public CaaSetupForm(CaaSession session)
    {
        bool hostConnected = session.Connected;
        SuspendLayout();
        AutoScaleMode = AutoScaleMode.None;
        Font = new Font("Segoe UI", 10F);
        Text = "ZWOgain CAA Setup";
        ClientSize = new Size(650, 440);
        MinimumSize = new Size(610, 440);
        StartPosition = FormStartPosition.CenterScreen;
        BackColor = Color.FromArgb(245, 247, 248);
        ForeColor = Color.FromArgb(32, 47, 53);
        MaximizeBox = false;
        var root = new TableLayoutPanel { Dock = DockStyle.Fill, ColumnCount = 1, RowCount = 3, Padding = new Padding(16) };
        root.ColumnStyles.Add(new ColumnStyle(SizeType.Percent, 100));
        root.RowStyles.Add(new RowStyle(SizeType.AutoSize));
        root.RowStyles.Add(new RowStyle(SizeType.Percent, 100));
        root.RowStyles.Add(new RowStyle(SizeType.AutoSize));
        Controls.Add(root);
        var heading = new Label { Text = "CAA rotator", Font = new Font(Font.FontFamily, 15, FontStyle.Bold), AutoSize = true, Margin = new Padding(0, 0, 0, 14) };
        root.Controls.Add(heading, 0, 0);
        var tabs = new TabControl { Dock = DockStyle.Fill, Padding = new Point(14, 7), Margin = Padding.Empty };
        root.Controls.Add(tabs, 0, 1);
        var footer = new TableLayoutPanel { AutoSize = true, Dock = DockStyle.Fill, ColumnCount = 2, Padding = new Padding(0, 12, 0, 0), Margin = Padding.Empty };
        footer.ColumnStyles.Add(new ColumnStyle(SizeType.Percent, 100)); footer.ColumnStyles.Add(new ColumnStyle(SizeType.AutoSize));
        root.Controls.Add(footer, 0, 2);
        var statusText = new Label { Text = "Disconnected", AutoSize = true, Dock = DockStyle.Fill, Margin = new Padding(0, 8, 12, 4) };
        var feedback = new Label { AutoSize = true, Dock = DockStyle.Fill, ForeColor = Color.FromArgb(155, 45, 30), Margin = new Padding(0, 4, 0, 0) };
        footer.Controls.Add(statusText, 0, 0);
        footer.Controls.Add(feedback, 0, 1); footer.SetColumnSpan(feedback, 2);
        var bottom = new FlowLayoutPanel { AutoSize = true, WrapContents = false, Margin = Padding.Empty };
        footer.Controls.Add(bottom, 1, 0);
        Button MakeButton(string text) => new() { Text = text, AutoSize = true, AutoSizeMode = AutoSizeMode.GrowAndShrink,
            MinimumSize = new Size(90, 32), Padding = new Padding(12, 4, 12, 4), Margin = new Padding(0, 0, 8, 0),
            FlatStyle = FlatStyle.Flat, FlatAppearance = { BorderSize = 0 },
            BackColor = Color.FromArgb(229, 237, 239), UseVisualStyleBackColor = false };
        var halt = MakeButton("Halt"); bottom.Controls.Add(halt);
        var close = MakeButton("Close"); close.Margin = Padding.Empty; bottom.Controls.Add(close);
        void UpdateControls() {
            bool isConnected = session.Connected;
            foreach (var c in offline) c.Enabled = !isConnected && !busy;
            foreach (var c in connected) c.Enabled = isConnected && !busy;
            foreach (var c in idle) c.Enabled = isConnected && !busy && !moving;
            halt.Enabled = isConnected;
            close.Enabled = !busy;
        }
        async Task Run(Func<Task> action) {
            if (busy) return;
            busy = true; feedback.Text = ""; UpdateControls();
            try { await action(); }
            catch (Exception e) { if (!closing) feedback.Text = e.GetBaseException().Message; }
            finally { busy = false; if (!closing) UpdateControls(); }
        }
        TableLayoutPanel Tab(string title, string description) {
            var page = new TabPage(title) { BackColor = Color.White, Padding = new Padding(14), AutoScroll = true };
            tabs.TabPages.Add(page);
            var body = new TableLayoutPanel { Dock = DockStyle.Top, AutoSize = true, AutoSizeMode = AutoSizeMode.GrowAndShrink, ColumnCount = 3, Margin = Padding.Empty };
            body.ColumnStyles.Add(new ColumnStyle(SizeType.Percent, 100));
            body.ColumnStyles.Add(new ColumnStyle(SizeType.Absolute, 115));
            body.ColumnStyles.Add(new ColumnStyle(SizeType.AutoSize));
            page.Controls.Add(body);
            var note = new Label { Text = description, AutoSize = true, Dock = DockStyle.Fill, Margin = new Padding(0, 0, 0, 18) };
            body.Controls.Add(note, 0, 0); body.SetColumnSpan(note, 3); body.RowCount = 1;
            body.RowStyles.Add(new RowStyle(SizeType.AutoSize));
            return body;
        }
        int Row(TableLayoutPanel body) { int row = body.RowCount++; body.RowStyles.Add(new RowStyle(SizeType.AutoSize)); return row; }
        void Wide(TableLayoutPanel body, Control control) { int row = Row(body); body.Controls.Add(control, 0, row); body.SetColumnSpan(control, 3); }
        Button Command(TableLayoutPanel body, string title, Func<Task> action, List<Control> group) {
            var button = MakeButton(title); button.Margin = new Padding(0, 0, 0, 12);
            button.Click += async (_, _) => await Run(action); group.Add(button); return button;
        }
        NumericUpDown Field(TableLayoutPanel body, string label, decimal min, decimal max, decimal value, string caption, Func<double, Task> action, int decimals = 2) {
            int row = Row(body);
            body.Controls.Add(new Label { Text = label, AutoSize = true, Dock = DockStyle.Fill, TextAlign = ContentAlignment.MiddleLeft, Margin = new Padding(0, 0, 12, 12) }, 0, row);
            var number = new NumericUpDown { Minimum = min, Maximum = max, Value = value, DecimalPlaces = decimals,
                Dock = DockStyle.Fill, Margin = new Padding(0, 4, 12, 12), AccessibleName = label };
            body.Controls.Add(number, 1, row); idle.Add(number);
            body.Controls.Add(Command(body, caption, () => action((double)number.Value), idle), 2, row);
            return number;
        }
        Task Action(string name, object args) => Task.Run(() => session.Action("ZwoGain.CAA." + name, JsonSerializer.Serialize(args)));
        void Display(CaaStatus value) {
            moving = value.Moving;
            statusText.Text = string.Format(CultureInfo.InvariantCulture, "Mechanical {0:F2}°   Sky {1:F2}°\n{2}", value.Mechanical, value.Logical, moving ? "Moving" : "Connected · Idle");
            if (value.MotionError is not null || value.Error != 0) feedback.Text = value.MotionError ?? "CAA fault " + value.Error;
        }
        var device = Tab("Device", "Choose the rotator to use. Your selection is saved automatically.");
        var picker = new ComboBox { Dock = DockStyle.Fill, DropDownStyle = ComboBoxStyle.DropDownList, Margin = new Padding(0, 0, 0, 12), AccessibleName = "CAA rotator" };
        Wide(device, picker); offline.Add(picker);
        if (!string.IsNullOrEmpty(session.Profile.Serial)) {
            picker.Items.Add(new CaaChoice { Serial = session.Profile.Serial, Label = "CAA — " + session.Profile.Serial + " (saved)" }); picker.SelectedIndex = 0;
        }
        void SaveSelection() { if (!session.Connected && picker.SelectedItem is CaaChoice choice) session.Select(choice.Serial); }
        picker.SelectionChangeCommitted += (_, _) => { try { SaveSelection(); feedback.Text = ""; } catch (Exception e) { feedback.Text = e.Message; } };
        async Task Discover() {
            var choices = await Task.Run(session.Discover);
            var saved = picker.SelectedItem as CaaChoice;
            if (saved is not null && choices.All(c => c.Serial != saved.Serial)) choices.Insert(0, saved);
            picker.Items.Clear(); foreach (var choice in choices) picker.Items.Add(choice);
            picker.SelectedItem = choices.FirstOrDefault(c => c.Serial == saved?.Serial);
            if (picker.SelectedIndex < 0 && choices.Count == 1) picker.SelectedIndex = 0;
            SaveSelection();
            if (choices.Count == 0) feedback.Text = "No CAA found. Check USB and close other rotator controllers.";
        }
        var deviceButtons = new FlowLayoutPanel { AutoSize = true, Dock = DockStyle.Fill, WrapContents = false, Margin = Padding.Empty };
        Wide(device, deviceButtons);
        var refresh = Command(device, "Refresh", Discover, offline); refresh.Margin = new Padding(0, 0, 8, 12); deviceButtons.Controls.Add(refresh);
        var identity = new Label { AutoSize = true, Dock = DockStyle.Fill, Margin = new Padding(0, 12, 0, 12) };
        Wide(device, identity);

        var motion = Tab("Motion", "Move within the configured travel limit.");
        var absolute = Field(motion, "Mechanical angle (°)", 0, 361, 0, "Move", value => Task.Run(() => session.Command("move-mechanical", value)));
        Field(motion, "Relative sky angle (°)", -360, 360, 1, "Move", value => Task.Run(() => session.Command("move-relative", value)));
        var sky = Field(motion, "Sky angle (°)", 0, 359.99m, 0, "Sync", value => Task.Run(() => session.Sync(value)));
        Wide(motion, new Label { Text = "Sync changes sky coordinates without moving the rotator.", AutoSize = true, Dock = DockStyle.Fill, Margin = new Padding(0, 8, 0, 0) });
        var settings = Tab("Settings", "Settings are read from the rotator when connected.");
        var beep = new CheckBox { Text = "Beep", AutoSize = true, Margin = new Padding(0, 0, 0, 14) }; Wide(settings, beep); idle.Add(beep);
        var reverse = new CheckBox { Text = "Reverse sky direction", AutoSize = true, Margin = new Padding(0, 0, 0, 18) }; Wide(settings, reverse); idle.Add(reverse);
        int aliasRow = Row(settings);
        settings.Controls.Add(new Label { Text = "Device alias", AutoSize = true, Anchor = AnchorStyles.Left }, 0, aliasRow);
        var alias = new TextBox { Dock = DockStyle.Fill, MaxLength = 8, AccessibleName = "Device alias", Margin = new Padding(0, 0, 0, 12) };
        settings.Controls.Add(alias, 1, aliasRow); settings.SetColumnSpan(alias, 2); idle.Add(alias);
        Wide(settings, Command(settings, "Apply settings", async () => {
            bool b = beep.Checked, r = reverse.Checked; string name = alias.Text;
            if (name.Any(c => c < 32 || c > 126)) throw new ArgumentException("Use up to eight printable ASCII characters for the alias.");
            await Task.Run(() => { session.Request(new { command = "beep", enabled = b }); session.Request(new { command = "reverse", enabled = r });
                session.RememberCoordinates(); session.Request(new { command = "alias", text = name }); });
        }, idle));
        var reference = Tab("Reference", "Change the mechanical reference without moving. This shifts the travel limits.");
        Wide(reference, Command(reference, "Set current position to mechanical 0°", () => Task.Run(() => session.Action("ZwoGain.CAA.ResetOrigin", "")), idle));
        var origin = Field(reference, "Mechanical reference (°)", 0, 360, 0, "Set", value => Action("SetReference", new { degrees = value }));
        var limit = Field(reference, "Travel limit (°)", 1, 361, 360, "Set", value => Action("SetLimit", new { degrees = (int)value }), 0);
        Wide(reference, new Label { Text = "360° is standard. 361° is experimental.", AutoSize = true, Dock = DockStyle.Fill });
        var multi = Tab("Multi-turn", "Travel in segments of up to 90°, resetting the reference between segments.\nThis bypasses cable-wrap protection. Allow clearance for the whole move.");
        var ready = new CheckBox { Text = "Clearance and cable slack checked", AutoSize = true, Margin = new Padding(0, 0, 0, 18) };
        Wide(multi, ready); idle.Add(ready);
        Field(multi, "Physical travel (°)", -450, 450, 90, "Start", value => {
            if (value == 0) throw new ArgumentException("Enter a nonzero travel angle.");
            if (!ready.Checked) throw new InvalidOperationException("Check clearance and cable slack first.");
            ready.Checked = false; return Action("RotateUnwrapped", new { degrees = value });
        });
        async Task ReadDetails() {
            var details = await Task.Run(() => (Id: session.Request(new { command = "identity" }), Settings: session.Request(new { command = "settings" }), Status: session.Status()));
            identity.Text = details.Id.GetProperty("model").GetString() + " · Firmware " + string.Join(".", details.Id.GetProperty("firmware").EnumerateArray().Select(v => v.GetInt32())) + "\nSerial " + details.Id.GetProperty("serial").GetString();
            beep.Checked = details.Settings.GetProperty("beep").GetBoolean(); reverse.Checked = details.Settings.GetProperty("reverse").GetBoolean(); alias.Text = details.Id.GetProperty("alias").GetString();
            absolute.Value = origin.Value = Math.Max(0, Math.Min(360, (decimal)details.Status.Mechanical));
            sky.Value = Math.Max(0, Math.Min(359.99m, (decimal)details.Status.Logical)); limit.Value = details.Status.Limit;
            Display(details.Status);
        }
        if (!hostConnected) {
            var connect = Command(device, "Connect", async () => { SaveSelection(); await Task.Run(session.Connect); await ReadDetails(); }, offline);
            connect.BackColor = Color.FromArgb(0, 112, 104); connect.ForeColor = Color.White; connect.Margin = new Padding(0, 0, 8, 12); deviceButtons.Controls.Add(connect);
            var disconnect = Command(device, "Disconnect", async () => { await Task.Run(session.Disconnect); moving = false; statusText.Text = "Disconnected"; }, connected);
            deviceButtons.Controls.Add(disconnect);
        } else Wide(device, new Label { Text = "Connected through your application. Disconnect there when finished.", AutoSize = true, Dock = DockStyle.Fill });
        Wide(device, Command(device, "Read device", ReadDetails, connected));
        // Halt stays available while a command is waiting for the worker.
        halt.Click += async (_, _) => { try { await Task.Run(session.Halt); } catch (Exception e) { if (!closing) feedback.Text = e.GetBaseException().Message; } };
        close.Click += (_, _) => Close(); CancelButton = close;
        timer.Tick += async (_, _) => {
            if (closing || polling || busy) return;
            UpdateControls();
            if (!session.Connected) return;
            polling = true;
            try { var value = await Task.Run(session.Status); if (!closing) Display(value); }
            catch (Exception e) { if (!closing) feedback.Text = e.GetBaseException().Message; }
            finally { polling = false; if (!closing) UpdateControls(); }
        };
        FormClosing += (_, e) => {
            if (busy) { e.Cancel = true; feedback.Text = "Waiting for the current command."; return; }
            try { SaveSelection(); } catch (Exception error) { e.Cancel = true; feedback.Text = error.Message; }
        };
        FormClosed += (_, _) => { closing = true; timer.Dispose(); };
        Shown += async (_, _) => { await Run(session.Connected ? ReadDetails : Discover); if (!closing) timer.Start(); };
        UpdateControls(); ResumeLayout(true);
    }
    public static void ShowModal(CaaSession session)
    {
        Exception? error = null;
        var thread = new Thread(() => {
            IntPtr previous = IntPtr.Zero;
            try {
                // Let Windows scale this isolated GDI dialog consistently, including
                // when a WPF host has already initialized WinForms at another DPI.
                try { previous = SetThreadDpiAwarenessContext(new IntPtr(-5)); } catch (EntryPointNotFoundException) { }
                Application.EnableVisualStyles();
                using var form = new CaaSetupForm(session); form.ShowDialog();
            } catch (Exception e) { error = e; }
            finally { if (previous != IntPtr.Zero) SetThreadDpiAwarenessContext(previous); }
        });
        thread.SetApartmentState(ApartmentState.STA); thread.Start(); thread.Join();
        if (error is not null) throw error;
    }
}
