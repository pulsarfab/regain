using System.Globalization;
using System.Windows;
using System.Windows.Automation;
using System.Windows.Controls;
using System.Windows.Threading;

namespace Regain.Rotator;

/// Shared NINA/ASCOM setup surface. Construction and refreshing are entirely inert.
/// The toggle saves policy immediately; helper approval is scoped to the selected camera.
public sealed class UsbRecoveryHelperPanel : StackPanel, IDisposable
{
    private readonly UsbRecoveryHelper helper;
    private readonly Func<UsbRecoverySelection> selected;
    private readonly Func<int, Task> persistResetAfterFailures;
    private readonly CancellationTokenSource lifetime = new();
    private readonly DispatcherTimer timer = new() { Interval = TimeSpan.FromSeconds(1) };
    // NINA skins CheckBox as a toggle and replaces its content with ON/OFF.
    private readonly CheckBox enabled = new() { HorizontalAlignment = HorizontalAlignment.Left, Margin = new Thickness(0, 2, 0, 8) };
    private readonly TextBox threshold = new() { MinHeight = 28, Margin = new Thickness(0, 4, 0, 8) };
    private readonly Button prepare = ActionButton("Enable helper", "usb-helper-prepare");
    private readonly Button check = ActionButton("Check status", "usb-helper-check");
    private readonly TextBlock status = new() { TextWrapping = TextWrapping.Wrap, Margin = new Thickness(0, 6, 0, 0) };
    private string selectionIdentity = "";
    private bool busy, disposed, ready;
    private int resetAfterFailures;
    public bool IsBusy => busy;
    public int ResetAfterFailures => resetAfterFailures > 0 ? ReadThreshold() : 0;

    public UsbRecoveryHelperPanel(UsbRecoveryHelper helper, Func<UsbRecoverySelection> selected,
        int initialResetAfterFailures, Func<int, Task> persistResetAfterFailures)
    {
        if (initialResetAfterFailures is < 0 or > 20) throw new ArgumentOutOfRangeException(nameof(initialResetAfterFailures));
        this.helper = helper; this.selected = selected; this.persistResetAfterFailures = persistResetAfterFailures;
        resetAfterFailures = initialResetAfterFailures;
        threshold.Text = (initialResetAfterFailures == 0 ? 2 : initialResetAfterFailures).ToString(CultureInfo.InvariantCulture);
        enabled.IsChecked = resetAfterFailures > 0;
        Margin = new Thickness(0, 0, 0, 20);
        Children.Add(new TextBlock { Text = "Automatic USB reset", FontWeight = FontWeights.SemiBold, Margin = new Thickness(0, 0, 0, 6) });
        AutomationProperties.SetAutomationId(enabled, "usb-reset-enabled");
        AutomationProperties.SetName(enabled, "Automatic USB reset"); Children.Add(enabled);
        Children.Add(new TextBlock { Text = "Enable while disconnected. Windows asks for administrator approval once to start a separate helper. It stays available through camera worker and supervisor restarts until disabled, Windows sign-out or restart. Imaging never opens an administrator prompt. This option saves immediately.", TextWrapping = TextWrapping.Wrap });
        Children.Add(new TextBlock { Text = "A USB reset or port cycle may disconnect the camera's internal USB hub and any attached filter wheel, focuser or other peripherals.", TextWrapping = TextWrapping.Wrap, Margin = new Thickness(0, 6, 0, 6) });
        Children.Add(new TextBlock { Text = "Recoverable failures before USB reset (1–20)", Margin = new Thickness(0, 4, 0, 0) });
        AutomationProperties.SetAutomationId(threshold, "usb-reset-failure-threshold");
        AutomationProperties.SetName(threshold, "Recoverable failures before USB reset");
        threshold.ToolTip = "One permits reset after the first recoverable failure. Two tries reconnecting first. At most one reset is allowed per outage and capture.";
        Children.Add(threshold);
        Children.Add(new TextBlock { Text = "After signing in again, use Enable helper before imaging if this saved option is on. Disabling removes this camera's approval and leaves other approved cameras available.", TextWrapping = TextWrapping.Wrap });
        var actions = new WrapPanel { Margin = new Thickness(0, 8, 0, 0) };
        actions.Children.Add(prepare); actions.Children.Add(check); Children.Add(actions);
        AutomationProperties.SetAutomationId(status, "usb-helper-status"); Children.Add(status);
        enabled.Click += async (_, _) => await SetEnabledAsync(enabled.IsChecked == true);
        prepare.Click += async (_, _) => await PrepareAsync();
        check.Click += async (_, _) => await CheckAsync();
        threshold.LostKeyboardFocus += async (_, _) => await SaveThresholdAsync();
        timer.Tick += (_, _) => RefreshAvailability();
        Loaded += async (_, _) => { if (!disposed) { RefreshAvailability(); timer.Start(); await CheckAsync(); } };
        Unloaded += (_, _) => timer.Stop();
        RefreshAvailability();
    }

    private static Button ActionButton(string text, string id)
    {
        var button = new Button { Content = text, Padding = new Thickness(10, 5, 10, 5), Margin = new Thickness(0, 0, 8, 4) };
        AutomationProperties.SetAutomationId(button, id); return button;
    }

    public void RefreshAvailability()
    {
        if (disposed) return;
        enabled.IsChecked = resetAfterFailures > 0;
        try {
            var selection = selected();
            string identity = selection.Name + "/" + selection.Serial + "/" + selection.Simulated;
            if (identity != selectionIdentity) { selectionIdentity = identity; status.Text = ""; ready = false; }
            string? unavailable = UsbRecoveryHelper.PreparationUnavailable(selection);
            // Policy is applied on connect. Never suggest that a connected supervisor was reconfigured.
            enabled.IsEnabled = !busy && (resetAfterFailures > 0 ? !selection.Connected : unavailable is null);
            enabled.ToolTip = selection.Connected ? "Disconnect the camera before changing automatic USB reset." : unavailable;
            threshold.IsEnabled = !busy && resetAfterFailures > 0 && !selection.Connected;
            prepare.IsEnabled = !busy && resetAfterFailures > 0 && unavailable is null;
            prepare.Visibility = resetAfterFailures > 0 && !ready ? Visibility.Visible : Visibility.Collapsed;
            prepare.ToolTip = unavailable ?? "Approve the selected camera for this Windows sign-in session.";
            check.IsEnabled = !busy;
            if (!busy && status.Text.Length == 0) status.Text = resetAfterFailures == 0
                ? "Automatic USB reset is off. Enabling does not reset USB."
                : "Automatic USB reset is enabled. Helper readiness has not been checked.";
        } catch (Exception error) {
            enabled.IsEnabled = threshold.IsEnabled = prepare.IsEnabled = false;
            check.IsEnabled = !busy;
            status.Text = "Camera state is unavailable: " + error.GetBaseException().Message;
        }
    }

    private int ReadThreshold()
    {
        if (!int.TryParse(threshold.Text.Trim(), NumberStyles.Integer, CultureInfo.InvariantCulture, out int failures) || failures is < 1 or > 20)
            throw new ArgumentException("USB reset failure threshold must be a whole number from 1 to 20.");
        return failures;
    }

    internal Task SetEnabledAsync(bool value) => Run(async () => {
        if (selected().Connected) throw new InvalidOperationException("Disconnect the camera before changing automatic USB reset.");
        if (value) {
            int failures = ReadThreshold();
            var selection = selected();
            status.Text = "Binding the selected camera, then waiting for Windows administrator approval…";
            var result = await helper.PrepareAsync(selected, lifetime.Token);
            ValidateCurrentSelection(selection);
            await persistResetAfterFailures(failures);
            resetAfterFailures = failures; ready = true; status.Text = result.Summary;
        } else {
            var selection = selected();
            await persistResetAfterFailures(0);
            resetAfterFailures = 0; ready = false;
            // Save off first: a failed cleanup must not leave automatic recovery enabled.
            try {
                await helper.DisableAsync(selection, lifetime.Token);
                status.Text = "Automatic USB reset is off. This camera's helper approval was removed.";
            } catch (Exception error) {
                status.Text = "Automatic USB reset is off. Could not remove helper approval: " + error.GetBaseException().Message;
            }
        }
    });
    internal Task PrepareAsync() => Run(async () => {
        if (resetAfterFailures == 0) throw new InvalidOperationException("Enable automatic USB reset first.");
        var selection = selected();
        status.Text = "Binding the selected camera, then waiting for Windows administrator approval…";
        var result = await helper.PrepareAsync(selected, lifetime.Token);
        ValidateCurrentSelection(selection);
        ready = true; status.Text = result.Summary;
    });
    internal Task CheckAsync() => Run(async () => {
        var result = await helper.CheckAsync(selected(), lifetime.Token);
        ready = result.IsReady;
        status.Text = (resetAfterFailures == 0 ? "Automatic USB reset is off. " : "Automatic USB reset is enabled. ") + result.Summary;
    });
    internal Task SaveThresholdAsync() => Run(async () => {
        if (resetAfterFailures == 0) return;
        if (selected().Connected) throw new InvalidOperationException("Disconnect the camera before changing automatic USB reset.");
        int failures = ReadThreshold();
        if (failures == resetAfterFailures) return;
        await persistResetAfterFailures(failures);
        resetAfterFailures = failures;
        status.Text = "USB reset failure threshold saved. " + (ready ? "Helper is ready for this camera." : "Check helper readiness before imaging.");
    });
    private void ValidateCurrentSelection(UsbRecoverySelection approved)
    {
        var current = selected();
        if (current.Connected || current.Simulated || current.Name != approved.Name || current.Serial != approved.Serial)
            throw new InvalidOperationException("The camera selection changed. Enable USB recovery again for the current selection.");
    }
    private async Task Run(Func<Task> action)
    {
        if (busy || disposed) return;
        busy = true; RefreshAvailability();
        try { await action(); }
        catch (OperationCanceledException) when (disposed) { }
        catch (Exception error) { ready = false; status.Text = error.GetBaseException().Message; }
        finally { busy = false; RefreshAvailability(); }
    }
    public void Dispose() { if (disposed) return; disposed = true; timer.Stop(); lifetime.Cancel(); lifetime.Dispose(); }
}
