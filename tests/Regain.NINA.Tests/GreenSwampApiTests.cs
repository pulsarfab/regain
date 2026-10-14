using System.Runtime.InteropServices;
using System.Windows;
using System.Windows.Controls;
using System.Windows.Controls.Primitives;
using System.Windows.Data;
using System.Windows.Threading;
using Moq;
using NINA.Equipment.Interfaces.Mediator;
using Regain.NINA;
using Xunit;

namespace Regain.NINA.Tests;

[CollectionDefinition("GSS settings UI", DisableParallelization = true)]
public sealed class GreenSwampUiCollection { }

// BAML template loading and another STA's first ItemsControl initialization can
// deadlock inside WPF's shared schema-context lock. Run this UI fixture outside
// the parallel Hub UI collections, as NINA itself loads templates on one UI thread.
[Collection("GSS settings UI")]
public sealed class GreenSwampApiTests
{
    // Exercise the production late-bound member names and argument dispatch,
    // without registering a replacement GS.SkyApi or starting a real mount.
    public sealed class Dispatch
    {
        public bool AscomOn { get; set; } = true;
        public bool IsMountRunning => true;
        public bool IsConnected => true;
        public bool IsParked => false;
        public bool CanHomeSensors => true;
        public bool IsAutoHomeRunning { get; private set; }
        public Exception? Error;
        public string LastAutoHomeError => Error is null ? "" : throw Error;
        public int Limit, Offset;
        public bool IsFullStop(int axis) => axis is 1 or 2 && !IsAutoHomeRunning;
        public void AutoHomeStart(int degreeLimit, int offsetDec) { Limit = degreeLimit; Offset = offsetDec; IsAutoHomeRunning = true; }
        public void AutoHomeStop() => IsAutoHomeRunning = false;
        public void AxisStop(int axis) { Assert.Contains(axis, new[] { 1, 2 }); }
    }
    [Fact] public void LateBoundApiUsesDocumentedMembersAndParameters()
    {
        var server = new Dispatch(); using var api = new GreenSwampApi(server);
        Assert.True(api.AscomOn); Assert.True(api.IsMountRunning); Assert.True(api.IsConnected); Assert.False(api.IsParked); Assert.True(api.CanHomeSensors);
        api.AscomOn = false; Assert.False(server.AscomOn);
        api.AutoHomeStart(120, 90); Assert.Equal(120, server.Limit); Assert.Equal(90, server.Offset);
        Assert.True(api.IsAutoHomeRunning); Assert.False(api.IsFullStop(1));
        api.AutoHomeStop(); api.AxisStop(1); api.AxisStop(2); Assert.False(api.IsAutoHomeRunning); Assert.True(api.IsFullStop(2)); Assert.Equal("", api.LastAutoHomeError);
    }
    [Theory] [InlineData(false)] [InlineData(true)]
    public void UnreadableStatusIncludingTheNullErrorGetterBugMustFail(bool com)
    {
        var server = new Dispatch { Error = com ? new COMException("null field", unchecked((int)0x80004003)) : new NullReferenceException() };
        using var api = new GreenSwampApi(server); Assert.Throws<IOException>(() => api.LastAutoHomeError);
        server.Error = new COMException("RPC unavailable", unchecked((int)0x800706BA));
        Assert.Equal(unchecked((int)0x800706BA), Assert.Throws<COMException>(() => api.LastAutoHomeError).HResult);
        server.Error = new IOException("Unreadable error state"); Assert.Throws<IOException>(() => api.LastAutoHomeError);
    }
    [Fact] public async Task SequenceControlsEditTheSharedSensorHomeOptions()
    {
        var completed = new TaskCompletionSource(TaskCreationOptions.RunContinuationsAsynchronously);
        string stage = "starting STA";
        var thread = new Thread(() => {
            var dispatcher = Dispatcher.CurrentDispatcher;
            SynchronizationContext.SetSynchronizationContext(new DispatcherSynchronizationContext(dispatcher));
            dispatcher.BeginInvoke(new Action(async () => {
            try {
                stage = "loading template";
                var service = new MountRecoveryService(Mock.Of<ITelescopeMediator>(), Mock.Of<IGuiderMediator>(), () => true, () => new object(), (_,_,_) => Task.CompletedTask);
                var action = new RecoverMount(service);
                var resources = new MountRecoveryTemplates();
                var panel = Assert.IsType<StackPanel>(((DataTemplate)resources["MountRecoverySettings"]).LoadContent());
                panel.DataContext = action;
                var window = new Window { Content = panel, Width = 1100, Height = 500, Left = -10000, Top = -10000, ShowInTaskbar = false };
                try {
                    stage = "showing settings";
                    window.Show();
                    stage = "resolving bindings";
                    await window.Dispatcher.InvokeAsync(() => { }, DispatcherPriority.DataBind);
                    stage = "editing checkbox";
                    var checkbox = panel.Children.OfType<CheckBox>().Single();
                    checkbox.GetBindingExpression(ToggleButton.IsCheckedProperty)!.UpdateTarget(); Assert.True(checkbox.IsChecked);
                    checkbox.SetCurrentValue(ToggleButton.IsCheckedProperty, false);
                    checkbox.GetBindingExpression(ToggleButton.IsCheckedProperty)!.UpdateSource(); Assert.False(action.Options.GreenSwampAutoHome);
                    var settings = panel.Children.OfType<WrapPanel>().Last().Children.OfType<TextBox>().ToArray();
                    stage = "editing limits";
                    settings[0].SetCurrentValue(TextBox.TextProperty, "120"); settings[0].GetBindingExpression(TextBox.TextProperty)!.UpdateSource();
                    settings[1].SetCurrentValue(TextBox.TextProperty, "-90"); settings[1].GetBindingExpression(TextBox.TextProperty)!.UpdateSource();
                    Assert.Equal(120, action.Options.GreenSwampSearchDegrees); Assert.Equal(-90, action.Options.GreenSwampDeclinationOffsetDegrees);
                } finally { stage = "closing settings"; window.Close(); }
                completed.SetResult();
            } catch (Exception error) { completed.SetException(error); }
            finally { dispatcher.BeginInvokeShutdown(DispatcherPriority.Background); }
            }));
            Dispatcher.Run();
        }) { IsBackground = true };
        thread.SetApartmentState(ApartmentState.STA); thread.Start();
        try { await completed.Task.WaitAsync(TimeSpan.FromSeconds(10)); }
        catch (TimeoutException error) { throw new TimeoutException("GSS settings fixture stalled while " + stage, error); }
    }
}
