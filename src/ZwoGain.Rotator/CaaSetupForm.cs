using System.Globalization;
using System.Runtime.InteropServices;
using System.Text.Json;
using System.Windows.Forms;

namespace ZwoGain.Rotator;

public sealed class CaaSetupForm : Form
{
    [DllImport("user32.dll")]
    private static extern IntPtr SetThreadDpiAwarenessContext(IntPtr context);
    [DllImport("user32.dll")]
    private static extern uint GetDpiForWindow(IntPtr window);
    private readonly System.Windows.Forms.Timer timer = new() { Interval = 750 };
    private bool refreshing;
    private int pendingActions;
    public CaaSetupForm(CaaSession session)
    {
        SuspendLayout();
        AutoScaleMode = AutoScaleMode.None;
        Font = new System.Drawing.Font("Segoe UI", 9, System.Drawing.FontStyle.Regular, System.Drawing.GraphicsUnit.Point);
        Text = "ZWOgain CAA Rotator"; Width = 620; Height = 570;
        StartPosition = FormStartPosition.CenterScreen; MinimumSize = new(580, 530);
        var tabs = new TabControl { Dock = DockStyle.Fill };
        var footer = new Label { Dock = DockStyle.Bottom, Height = 54, Padding = new(12), Text = "Choose a device, then connect." };
        Controls.Add(tabs); Controls.Add(footer);
        FlowLayoutPanel Tab(string title) {
            var page = new TabPage(title); tabs.TabPages.Add(page);
            var body = new FlowLayoutPanel { Dock = DockStyle.Fill, FlowDirection = FlowDirection.TopDown,
                WrapContents = false, AutoScroll = true, Padding = new(12) };
            page.Controls.Add(body); return body;
        }
        void Label(FlowLayoutPanel parent, string text) => parent.Controls.Add(new Label { Text = text, Width = 540, AutoSize = false, Height = 40 });
        Button Button(FlowLayoutPanel parent, string text, Func<Task> action) {
            var button = new Button { Text = text, AutoSize = true, MinimumSize = new(160, 32) };
            parent.Controls.Add(button);
            button.Click += async (_, _) => {
                button.Enabled = false; pendingActions++;
                try { await action(); footer.Text = "Done"; }
                catch (Exception e) { footer.Text = e.GetBaseException().Message; }
                finally { pendingActions--; if (!button.IsDisposed) button.Enabled = true; }
            };
            return button;
        }
        NumericUpDown Number(FlowLayoutPanel parent, decimal min, decimal max, decimal value, int decimals = 2) {
            var number = new NumericUpDown { Minimum = min, Maximum = max, Value = value, DecimalPlaces = decimals, Width = 150 };
            parent.Controls.Add(number); return number;
        }
        Task Run(Action action) => Task.Run(action);
        var device = Tab("Device");
        Label(device, "CAA (USB HID; no ZWO SDK required)");
        var picker = new ComboBox { Width = 510, DropDownStyle = ComboBoxStyle.DropDownList };
        if (!string.IsNullOrEmpty(session.Profile.Serial)) {
            picker.Items.Add(new CaaChoice { Serial = session.Profile.Serial, Label = "CAA — " + session.Profile.Serial + " (saved)" }); picker.SelectedIndex = 0;
        }
        device.Controls.Add(picker);
        Button(device, "Refresh devices", async () => {
            var choices = await Task.Run(session.Discover); picker.Items.Clear();
            foreach (var choice in choices) picker.Items.Add(choice);
            if (choices.Count > 0) picker.SelectedIndex = Math.Max(0, choices.FindIndex(c => c.Serial == session.Profile.Serial));
            if (choices.Count == 0) throw new IOException("No available CAA. Close other controllers and check USB.");
        });
        Button(device, "Save selection", () => { session.Select((picker.SelectedItem as CaaChoice)?.Serial ?? ""); return Task.CompletedTask; });
        Button(device, "Connect / disconnect", () => Run(() => { if (session.Connected) session.Disconnect(); else session.Connect(); }));
        var identity = new Label { Width = 530, Height = 80 }; device.Controls.Add(identity);
        Button(device, "Read identity", async () => {
            var value = await Task.Run(() => session.Request(new { command = "identity" }));
            identity.Text = value.GetRawText();
        });
        var motion = Tab("Motion");
        Label(motion, "Absolute mechanical position (degrees)"); var absolute = Number(motion, 0, 361, 0);
        Button(motion, "Move mechanical", () => { double value = (double)absolute.Value; return Run(() => session.Command("move-mechanical", value)); });
        Label(motion, "Relative logical move (degrees)"); var relative = Number(motion, -360, 360, 1);
        Button(motion, "Move relative", () => { double value = (double)relative.Value; return Run(() => session.Command("move-relative", value)); });
        Button(motion, "Halt", () => Run(session.Halt));
        var settings = Tab("Settings");
        var beep = new CheckBox { Text = "Beep", AutoSize = true }; settings.Controls.Add(beep);
        var reverse = new CheckBox { Text = "Reverse logical direction", AutoSize = true }; settings.Controls.Add(reverse);
        Label(settings, "Device alias (up to eight ASCII characters)"); var alias = new TextBox { Width = 300, MaxLength = 8 }; settings.Controls.Add(alias);
        Button(settings, "Read settings", async () => {
            var value = await Task.Run(() => session.Request(new { command = "settings" }));
            beep.Checked = value.GetProperty("beep").GetBoolean(); reverse.Checked = value.GetProperty("reverse").GetBoolean();
            var id = await Task.Run(() => session.Request(new { command = "identity" })); alias.Text = id.GetProperty("alias").GetString();
        });
        Button(settings, "Apply settings", async () => {
            bool b = beep.Checked, r = reverse.Checked; string a = alias.Text;
            await Run(() => { session.Request(new { command = "beep", enabled = b }); session.Request(new { command = "reverse", enabled = r });
                session.RememberCoordinates(); session.Request(new { command = "alias", text = a }); });
        });
        var reference = Tab("Reference / limits");
        Label(reference, "Relabel the current position without moving. This changes the origin of the travel limit.");
        Button(reference, "Set current position to mechanical 0°", () => Run(() => session.Action("ZwoGain.CAA.ResetOrigin", "")));
        var origin = Number(reference, 0, 360, 0);
        Button(reference, "Set mechanical reference", () => { string value = JsonSerializer.Serialize(new { degrees = (double)origin.Value }); return Run(() => session.Action(CaaSession.Actions[3], value)); });
        Label(reference, "Maximum mechanical angle. 360° is standard; 361° is experimental.");
        var limit = Number(reference, 1, 361, 360, 0);
        Button(reference, "Set travel limit", () => { string value = JsonSerializer.Serialize(new { degrees = (int)limit.Value }); return Run(() => session.Action(CaaSession.Actions[4], value)); });
        Label(reference, "Sky angle for logical sync (does not change mechanical zero)"); var sky = Number(reference, 0, 359.99m, 0);
        Button(reference, "Sync sky angle", () => { double value = (double)sky.Value; return Run(() => session.Sync(value)); });
        var extended = Tab("Multi-turn");
        Label(extended, "Moves in 90° segments and resets the mechanical reference. Positive/negative selects physical direction; Reverse does not affect it.");
        Label(extended, "This bypasses cable-wrap limits. Provide clearance and cable slack for the full move. Maximum per command: ±450°.");
        var travel = Number(extended, -450, 450, 90);
        var ready = new CheckBox { Text = "Clearance and cable slack checked", AutoSize = true }; extended.Controls.Add(ready);
        Button(extended, "Start explicit travel", async () => {
            if (!ready.Checked) throw new InvalidOperationException("Check clearance and cable slack first");
            double degrees = (double)travel.Value; ready.Checked = false;
            await Run(() => session.Action(CaaSession.Actions[7], JsonSerializer.Serialize(new { degrees })));
        });
        Button(extended, "Halt", () => Run(session.Halt));
        timer.Tick += async (_, _) => {
            if (refreshing || !session.Connected) return;
            refreshing = true;
            try {
                var status = await Task.Run(session.Status);
                if (IsDisposed) return;
                footer.Text = status.MotionError ?? (status.Error != 0 ? "CAA fault " + status.Error :
                    string.Format(CultureInfo.InvariantCulture, "Mechanical {0:F2}°   Sky {1:F2}°   Limit {2}°   {3}", status.Mechanical, status.Logical, status.Limit, status.Moving ? "Moving" : "Idle"));
            } catch (Exception e) { footer.Text = e.GetBaseException().Message; }
            finally { refreshing = false; }
        };
        timer.Start();
        FormClosing += (_, e) => {
            // A queued Connect must finish before the owner disposes its session.
            if (pendingActions > 0) { e.Cancel = true; footer.Text = "Waiting for the current command."; }
        };
        FormClosed += (_, _) => timer.Dispose();
        ResumeLayout(false);
        Load += (_, _) => {
            // A WPF host can initialize WinForms with a cached 96-DPI value.
            // Read the actual window DPI and scale the completed layout once.
            uint dpi = 96;
            try { dpi = GetDpiForWindow(Handle); } catch (EntryPointNotFoundException) { }
            if (dpi > 96) Scale(new System.Drawing.SizeF(dpi / 96f, dpi / 96f));
        };
    }
    public static void ShowModal(CaaSession session)
    {
        Exception? error = null;
        var thread = new Thread(() => {
            IntPtr previous = IntPtr.Zero;
            try {
                // Isolate WinForms from the host's WPF/COM DPI configuration.
                // Build the complete 96-DPI layout before scaling it on this thread.
                try { previous = SetThreadDpiAwarenessContext(new IntPtr(-4)); } catch (EntryPointNotFoundException) { }
                using var form = new CaaSetupForm(session); form.ShowDialog();
            } catch (Exception e) { error = e; }
            finally { if (previous != IntPtr.Zero) SetThreadDpiAwarenessContext(previous); }
        });
        thread.SetApartmentState(ApartmentState.STA); thread.Start(); thread.Join();
        if (error is not null) throw error;
    }
}
