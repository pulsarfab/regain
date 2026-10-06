using System.Text.Json;
using System.Windows;
using System.Windows.Controls;
using System.Windows.Media;
using Regain.Rotator;

namespace Regain.Hub;

/// Shared native editor for NINA and ASCOM. A setup client has no equipment
/// lease, and all durable validation/commit decisions remain in the Rust host.
public sealed partial class HubConfigurationWindow : Window
{
    private readonly string executable, configPath;
    private readonly Guid instance;
    private readonly CancellationTokenSource lifetime = new();
    private readonly ContentControl configuration = new();
    private readonly TextBox preview = new() { IsReadOnly = true, TextWrapping = TextWrapping.Wrap, AcceptsReturn = true,
        VerticalScrollBarVisibility = ScrollBarVisibility.Auto, FontFamily = new FontFamily("Consolas"), FontSize = 12 };
    private readonly TextBox diagnostics = new() { IsReadOnly = true, TextWrapping = TextWrapping.Wrap, AcceptsReturn = true,
        VerticalScrollBarVisibility = ScrollBarVisibility.Auto };
    private readonly StackPanel sources = new();
    private readonly TextBlock status = new() { TextWrapping = TextWrapping.Wrap, Margin = new Thickness(8) };
    private readonly TextBlock errors = new() { Foreground = Brushes.Firebrick, TextWrapping = TextWrapping.Wrap, Margin = new Thickness(8) };
    private readonly Button reload = new() { Content = "Reload saved configuration", Padding = new Thickness(12, 8, 12, 8), Margin = new Thickness(4) };
    private readonly Button review = new() { Content = "Review changes", Padding = new Thickness(12, 8, 12, 8), Margin = new Thickness(4), IsEnabled = false };
    private readonly Button apply = new() { Content = "Apply reviewed configuration", Padding = new Thickness(12, 8, 12, 8), Margin = new Thickness(4), IsEnabled = false };
    private readonly TabControl tabs = new();
    private HubEditorSession? session;
    private HubConfigurationForm? form;
    private bool busy, closed;
    internal HubConfigurationWindow(string executable, string configPath, Guid instance)
    {
        this.executable = executable; this.configPath = configPath; this.instance = instance;
        Title = "PulsarFab regain — hub setup"; Width = 880; Height = 820; MinWidth = 700; MinHeight = 540;
        WindowStartupLocation = WindowStartupLocation.CenterOwner; SetupTheme.Apply(this);
        var panel = new DockPanel { Margin = new Thickness(16) }; Content = panel;
        var bottom = new StackPanel(); DockPanel.SetDock(bottom, Dock.Bottom); panel.Children.Add(bottom);
        var actions = new StackPanel { Orientation = Orientation.Horizontal }; actions.Children.Add(reload); actions.Children.Add(review); actions.Children.Add(apply);
        bottom.Children.Add(errors); bottom.Children.Add(actions); bottom.Children.Add(status);
        var heading = new StackPanel { Margin = new Thickness(8, 0, 8, 12) }; DockPanel.SetDock(heading, Dock.Top); panel.Children.Add(heading);
        heading.Children.Add(new TextBlock { Text = "Configure shared hub devices", FontSize = 24 });
        heading.Children.Add(new TextBlock { Text = "Changes stay in this draft until reviewed and applied. Disconnect all output clients before Apply. Source settings and safety policies are shared across NINA, Alpaca and ASCOM.", TextWrapping = TextWrapping.Wrap });
        tabs.Items.Add(new TabItem { Header = "Configuration", Content = new ScrollViewer { Content = configuration, VerticalScrollBarVisibility = ScrollBarVisibility.Auto } });
        tabs.Items.Add(new TabItem { Header = "Review", Content = preview });
        var health = new DockPanel();
        var sourceScroll = new ScrollViewer { Content = sources, VerticalScrollBarVisibility = ScrollBarVisibility.Auto, MaxHeight = 310 };
        DockPanel.SetDock(sourceScroll, Dock.Top); health.Children.Add(sourceScroll); health.Children.Add(diagnostics);
        tabs.Items.Add(new TabItem { Header = "Source health", Content = health }); panel.Children.Add(tabs);
        tabs.Items.Add(new TabItem { Header = "Credentials", Content = new ScrollViewer { Content = credentials, VerticalScrollBarVisibility = ScrollBarVisibility.Auto } });
        tabs.Items.Add(new TabItem { Header = "Simulation", Content = new ScrollViewer { Content = simulation, VerticalScrollBarVisibility = ScrollBarVisibility.Auto } });
        reload.Click += async (_, _) => {
            if ((session?.Draft?.Dirty == true || form?.Errors.Count > 0) && MessageBox.Show(this, "Discard the unsaved draft and reload?", "Reload hub configuration", MessageBoxButton.YesNo) != MessageBoxResult.Yes) return;
            await Run(Load);
        };
        review.Click += async (_, _) => await Run(async () => {
            if (form?.Errors.Count > 0) throw new InvalidOperationException("Correct the highlighted inputs before reviewing");
            var valid = await session!.ReviewAsync(lifetime.Token);
            preview.Text = session.Draft!.Preview(); ShowErrors(session.Errors); tabs.SelectedIndex = 1;
            status.Text = valid ? "Review the configuration above. Validation does not connect equipment. Disconnect output clients, then Apply." : "Correct the listed validation errors before applying.";
        });
        apply.Click += async (_, _) => await Run(async () => {
            var result = await session!.ApplyAsync(lifetime.Token);
            await session.ReloadAsync(lifetime.Token); Render();
            var appliedRevision = result.GetProperty("configurationRevision").GetGuid();
            if (session.Draft!.Revision != appliedRevision) {
                status.Text = "Saved revision " + appliedRevision + ". Another editor changed the configuration afterward. Showing current saved revision " + session.Draft.Revision + ". Review this version before making further changes.";
                return;
            }
            status.Text = result.GetProperty("ready").GetBoolean() ? "Saved revision " + result.GetProperty("configurationRevision").GetString() +
                (result.TryGetProperty("persistenceWarning", out var warning) ? ". " + warning.GetString() : ".") :
                "The configuration was saved, but source cleanup blocked the host. Inspect host status before connecting equipment.";
        });
        Loaded += async (_, _) => await Run(Load);
        Closed += (_, _) => { closed = true; authorization?.Clear(); lifetime.Cancel(); session?.Dispose(); if (!busy) lifetime.Dispose(); };
    }
    private async Task Load()
    {
        authorization?.Clear();
        // Reload uses a freshly authenticated client: a lost transport never
        // causes a hidden retry of Apply, and a replacement host is explicit.
        session?.Dispose(); session = null;
        var opened = await HubEditorSession.AttachAsync(executable, configPath, instance, lifetime.Token);
        if (closed) { opened.Dispose(); lifetime.Token.ThrowIfCancellationRequested(); }
        session = opened; await session.ReloadAsync(lifetime.Token); Render();
        status.Text = session.State == HubEditorState.Editing ? "Saved revision " + session.Draft!.Revision + ". Expand sources and outputs to edit their settings." :
            "Host is not ready. Inspect source health and host status before making another change.";
    }
    private void Render()
    {
        form = new(session!.Draft!, () => {
            session.Changed(); preview.Clear(); errors.Text = ""; status.Text = "Unsaved changes. Review before applying."; Controls();
        }, message => status.Text = message);
        form.Render(); configuration.Content = form.Root; preview.Clear(); diagnostics.Clear(); errors.Text = "";
        RenderInspection();
        RenderCredentials();
        RenderSimulation();
    }
    private async Task Run(Func<Task> action)
    {
        if (busy || closed) return;
        busy = true; Controls(); errors.Text = "";
        try { await action(); }
        catch (OperationCanceledException) { if (!closed) status.Text = Uncertain(); }
        catch (HubException problem) {
            if (!closed) {
                if (problem.Remote is not null) ShowErrors(problem.Remote.Fields);
                status.Text = session?.State == HubEditorState.Uncertain ? Uncertain() : problem.Remote?.Code switch {
                    "connected" => "Disconnect all hub output clients and finish pending operations, then review again.",
                    "invalidConfig" => "Correct the listed configuration errors and review again.",
                    "revisionConflict" => "The saved configuration changed. Reload before editing.",
                    "busy" => "Another configuration operation is in progress. Review again when it finishes.",
                    "persistence" => "The save failed; the previous configuration was retained.",
                    _ => problem.Message
                };
            }
        }
        catch (InvalidOperationException problem) { if (!closed) status.Text = problem.Message; }
        catch { if (!closed) status.Text = "Could not complete hub setup. Reload to inspect the saved configuration and host status."; }
        finally { busy = false; Controls(); if (closed) lifetime.Dispose(); }
    }
    private void Controls()
    {
        var editable = session?.State is HubEditorState.Editing or HubEditorState.Reviewed;
        configuration.IsEnabled = !busy && editable; sources.IsEnabled = !busy && session is not null;
        reload.IsEnabled = !busy; review.IsEnabled = !busy && editable && form?.Errors.Count == 0;
        apply.IsEnabled = !busy && session?.State == HubEditorState.Reviewed && form?.Errors.Count == 0;
        CredentialControls(editable);
        InspectionControls(editable);
        SimulationControls(editable);
    }
    private void ShowErrors(JsonElement fields) => errors.Text = string.Join("\n", fields.EnumerateArray().Select(field => field.GetProperty("path").GetString() + ": " + field.GetProperty("message").GetString()));
    private static string Pretty(JsonElement value) => JsonSerializer.Serialize(value, new JsonSerializerOptions { WriteIndented = true });
    private static string Uncertain() => "The outcome is unknown or the host changed. Reload the saved configuration and inspect host status before another change. Check a retained credential reference with Read credential status. Do not repeat the write.";
    public static void Show(Window? owner, string executable, string configPath, Guid instance) => new HubConfigurationWindow(executable, configPath, instance) { Owner = owner }.ShowDialog();
}
