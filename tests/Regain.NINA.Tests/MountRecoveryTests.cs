using System.ComponentModel.Composition;
using Moq;
using Newtonsoft.Json;
using NINA.Astrometry;
using NINA.Astrometry.Interfaces;
using NINA.Core.Model;
using NINA.Core.Utility.WindowService;
using NINA.Equipment.Interfaces;
using NINA.Equipment.Interfaces.Mediator;
using NINA.Sequencer.Container;
using NINA.Sequencer.Interfaces;
using NINA.Sequencer;
using NINA.Sequencer.Serialization;
using NINA.Sequencer.SequenceItem;
using NINA.Sequencer.Trigger;
using NINA.PlateSolving;
using NINA.PlateSolving.Interfaces;
using NINA.Profile.Interfaces;
using NINA.WPF.Base.Interfaces.Mediator;
using Regain.Core;
using Xunit;

namespace Regain.NINA.Tests;

public sealed class MountRecoveryTests
{
    private sealed class InvokeRecovery(RecoverMount action) : SequenceItem
    {
        public bool Entered;
        public override Task Execute(IProgress<ApplicationStatus> progress, CancellationToken token)
        { Entered = true; return action.Execute(progress, token); }
        public override object Clone() => new InvokeRecovery(action);
    }
    private sealed class SolverFixture : MountCentering
    {
        private readonly bool success;
        public SolverFixture(IGuiderMediator guider, bool success) : base(Mock.Of<IProfileService>(), Mock.Of<ITelescopeMediator>(),
            Mock.Of<IImagingMediator>(), Mock.Of<IFilterWheelMediator>(), guider, Mock.Of<IDomeMediator>(), Mock.Of<IDomeFollower>(),
            Mock.Of<IPlateSolverFactory>(), Mock.Of<IWindowServiceFactory>()) { this.success = success; }
        protected override Task<PlateSolveResult> DoCenter(IProgress<ApplicationStatus> progress, CancellationToken token) =>
            Task.FromResult(new PlateSolveResult { Success = success });
    }
    private sealed class Clock : TimeProvider
    {
        private long ticks;
        public override long TimestampFrequency => TimeSpan.TicksPerSecond;
        public override long GetTimestamp() => ticks;
        public void Advance(double seconds) => ticks += (long)(seconds * TimeSpan.TicksPerSecond);
    }
    private sealed class Rig : IDisposable
    {
        public readonly Mock<ITelescope> Upstream = new();
        public readonly Mock<ITelescopeMediator> Telescope = new();
        public readonly Mock<IGuiderMediator> Guider = new();
        public readonly Mock<IGuider> Guide = new();
        public readonly Clock Clock = new();
        public readonly List<string> Calls = [];
        public readonly RegainMount Mount;
        public readonly MountRecoveryService Service;
        public bool Tracking = true, Home, Slewing = false, Safe = true, CenterFails, RejectSlew, Guiding = true;
        public double Ra = 2.36, Dec = 62.48;
        public TrackingRate Rate = new() { TrackingMode = TrackingMode.Sidereal };
        public Coordinates? SlewTarget;
        public Func<CancellationToken, Task>? OnHome;
        public Rig(MountRecoveryJournal? journal = null, Func<IGreenSwampApi>? greenSwampApi = null)
        {
            Upstream.SetupGet(m => m.Id).Returns(greenSwampApi is null ? "ASCOM.Fixture.Telescope" : "ASCOM.GS.Sky.Telescope");
            Upstream.SetupGet(m => m.Name).Returns("Fixture AM7");
            Upstream.SetupGet(m => m.Connected).Returns(true);
            Upstream.SetupGet(m => m.CanFindHome).Returns(true);
            Upstream.SetupGet(m => m.CanSlew).Returns(true);
            Upstream.SetupGet(m => m.CanSetTrackingEnabled).Returns(true);
            Upstream.SetupGet(m => m.TrackingEnabled).Returns(() => Tracking);
            Upstream.SetupSet(m => m.TrackingEnabled = It.IsAny<bool>()).Callback<bool>(v => { Tracking = v; Calls.Add("tracking:" + v); });
            Upstream.SetupGet(m => m.TrackingRate).Returns(() => Rate);
            Upstream.SetupGet(m => m.TrackingMode).Returns(() => Rate.TrackingMode);
            Upstream.SetupSet(m => m.TrackingMode = It.IsAny<TrackingMode>()).Callback<TrackingMode>(v => { Rate = new() { TrackingMode = v }; Calls.Add("rate:" + v); });
            Upstream.Setup(m => m.SetCustomTrackingRate(It.IsAny<double>(), It.IsAny<double>())).Callback<double,double>((ra,dec) => Rate = new() { TrackingMode = TrackingMode.Custom, CustomRightAscensionRate = ra, CustomDeclinationRate = dec });
            Upstream.SetupGet(m => m.RightAscension).Returns(() => Ra); Upstream.SetupGet(m => m.Declination).Returns(() => Dec);
            Upstream.SetupGet(m => m.AtHome).Returns(() => Home); Upstream.SetupGet(m => m.Slewing).Returns(() => Slewing);
            Upstream.Setup(m => m.FindHome(It.IsAny<CancellationToken>())).Returns(async (CancellationToken t) => { Calls.Add("home"); if (OnHome is not null) await OnHome(t); Home = true; Rate = new() { TrackingMode = TrackingMode.Sidereal }; });
            Upstream.Setup(m => m.SlewToCoordinates(It.IsAny<Coordinates>(),It.IsAny<CancellationToken>())).Returns((Coordinates c, CancellationToken _) => { Calls.Add("slew"); SlewTarget = c; Home = false; return Task.FromResult(!RejectSlew); });
            Mount = new(Upstream.Object, Clock);
            Telescope.Setup(m => m.GetDevice()).Returns(Mount);
            Telescope.Setup(m => m.FindHome(It.IsAny<IProgress<ApplicationStatus>>(),It.IsAny<CancellationToken>())).Returns(async (IProgress<ApplicationStatus> _, CancellationToken t) => { await Mount.FindHome(t); return true; });
            Telescope.Setup(m => m.SlewToCoordinatesAsync(It.IsAny<Coordinates>(),It.IsAny<CancellationToken>())).Returns((Coordinates c,CancellationToken t) => Mount.SlewToCoordinates(c,t));
            Guide.SetupGet(g => g.Connected).Returns(true); Guide.SetupGet(g => g.State).Returns(() => Guiding ? "Guiding" : "Stopped");
            Guider.Setup(g => g.GetDevice()).Returns(Guide.Object);
            Guider.Setup(g => g.StopGuiding(It.IsAny<CancellationToken>())).Returns((CancellationToken _) => { Calls.Add("guide:stop"); Guiding = false; return Task.FromResult(true); });
            Guider.Setup(g => g.StartGuiding(false,It.IsAny<IProgress<ApplicationStatus>>(),It.IsAny<CancellationToken>())).Returns((bool _,IProgress<ApplicationStatus> _,CancellationToken _) => { Calls.Add("guide:start"); Guiding = true; return Task.FromResult(true); });
            object profile = new();
            Service = new(Telescope.Object, Guider.Object, () => Safe, () => profile, (_,_,_) => { Calls.Add("center"); if (CenterFails) throw new IOException("No solution"); return Task.CompletedTask; }, journal, greenSwampApi);
            Mount.Observe();
        }
        public void Fault() { Tracking = false; Mount.Observe(); Clock.Advance(5); Mount.Observe(); Assert.NotNull(Mount.Health.Fault); }
        public void Dispose() { Assert.False(Mount.Recovering); Mount.Dispose(); }
    }
    private static Coordinates Target => new(3.5, 45, Epoch.J2000, Coordinates.RAType.Hours);
    private sealed class SensorHome : IGreenSwampApi
    {
        public Rig Rig = null!;
        public bool AscomOn { get; set; } = true;
        public bool IsMountRunning { get; set; } = true;
        public bool IsConnected { get; set; } = true;
        public bool IsParked { get; set; }
        public bool CanHomeSensors { get; set; } = true;
        public string LastAutoHomeError { get; set; } = "";
        public volatile bool Running, Disposed;
        public bool AutoComplete = true, Acknowledge = true, StartThrows, HomeOnComplete = true;
        public volatile bool StopRequested;
        public int Starts, Stops, Reads, DegreeLimit, Offset, ThreadId;
        public ApartmentState Apartment;
        public readonly TaskCompletionSource Entered = new(TaskCreationOptions.RunContinuationsAsynchronously);
        public Action? OnComplete;
        public bool IsAutoHomeRunning {
            get {
                if (Running && AutoComplete && ++Reads >= 3) {
                    Running = false; Rig.Home = HomeOnComplete; OnComplete?.Invoke();
                }
                return Running;
            }
        }
        public bool IsFullStop(int axis) => !Rig.Tracking && !Running;
        public void AutoHomeStart(int degreeLimit, int offsetDec) {
            Assert.False(AscomOn); Assert.False(Rig.Tracking); Assert.False(Rig.Guiding);
            Starts++; DegreeLimit = degreeLimit; Offset = offsetDec;
            ThreadId = Environment.CurrentManagedThreadId; Apartment = Thread.CurrentThread.GetApartmentState();
            Rig.Calls.Add("sensor:home"); Running = Acknowledge; Entered.TrySetResult();
            if (StartThrows) throw new IOException("COM return failed after start");
        }
        public void AutoHomeStop() { Stops++; StopRequested = true; }
        public void Dispose() { Disposed = true; }
    }
    [Fact] public async Task GreenSwampUsesSensorHomeThenTheExistingFullRecoveryPipeline()
    {
        var api = new SensorHome(); int connectionThread = 0;
        using var rig = new Rig(greenSwampApi: () => { connectionThread = Environment.CurrentManagedThreadId; return api; });
        api.Rig = rig; rig.Fault(); rig.Upstream.SetupGet(m => m.CanFindHome).Returns(false);
        await rig.Service.RecoverAsync(Target, new() { GreenSwampSearchDegrees = 75, GreenSwampDeclinationOffsetDegrees = -90 }, new Progress<ApplicationStatus>(), default);
        Assert.Equal(1, api.Starts); Assert.Equal(0, api.Stops); Assert.Equal(75, api.DegreeLimit); Assert.Equal(-90, api.Offset);
        Assert.True(api.AscomOn); Assert.True(api.Disposed); Assert.Equal(ApartmentState.STA, api.Apartment); Assert.Equal(connectionThread, api.ThreadId);
        Assert.DoesNotContain("home", rig.Calls); Assert.True(rig.Calls.IndexOf("guide:stop") < rig.Calls.IndexOf("sensor:home"));
        Assert.True(rig.Calls.IndexOf("sensor:home") < rig.Calls.IndexOf("slew"));
        Assert.True(rig.Calls.IndexOf("center") < rig.Calls.IndexOf("guide:start"));
        Assert.Equal(3.5, rig.SlewTarget!.RA); Assert.True(rig.Tracking); Assert.True(rig.Guiding); Assert.Null(rig.Mount.Health.Fault);
    }
    [Theory] [InlineData("sensors")] [InlineData("busy")] [InlineData("gate")] [InlineData("disconnected")] [InlineData("parked")]
    public async Task GreenSwampPreflightRefusesUnsupportedOrAlreadyOwnedServer(string failure)
    {
        var api = new SensorHome(); using var rig = new Rig(greenSwampApi: () => api); api.Rig = rig; rig.Fault();
        if (failure == "sensors") api.CanHomeSensors = false;
        if (failure == "busy") api.Running = true;
        if (failure == "gate") api.AscomOn = false;
        if (failure == "disconnected") api.IsConnected = false;
        if (failure == "parked") api.IsParked = true;
        api.AutoComplete = false;
        await Assert.ThrowsAsync<IOException>(() => rig.Service.RecoverAsync(Target, new(), new Progress<ApplicationStatus>(), default));
        Assert.Equal(0, api.Starts); Assert.Equal(0, api.Stops); Assert.Equal(failure != "gate", api.AscomOn);
        Assert.DoesNotContain("home", rig.Calls); Assert.DoesNotContain("slew", rig.Calls); Assert.DoesNotContain("guide:start", rig.Calls);
        Assert.True(api.Disposed);
    }
    [Fact] public async Task GreenSwampCoordinateHomeRequiresExplicitOptOutAndDoesNotOpenSensorApi()
    {
        using var rig = new Rig(greenSwampApi: () => throw new Exception("Should not open API")); rig.Fault();
        await rig.Service.RecoverAsync(Target, new() { GreenSwampAutoHome = false }, new Progress<ApplicationStatus>(), default);
        Assert.Contains("home", rig.Calls); Assert.Contains("center", rig.Calls);
    }
    [Fact] public async Task GreenSwampNameAloneCannotSelectTheGlobalServerApi()
    {
        using var rig = new Rig(greenSwampApi: () => throw new Exception("Wrong driver"));
        rig.Upstream.SetupGet(m => m.Id).Returns("ASCOM.Other.Telescope"); rig.Upstream.SetupGet(m => m.Name).Returns("Green Swamp"); rig.Fault();
        await rig.Service.RecoverAsync(Target, new(), new Progress<ApplicationStatus>(), default);
        Assert.Contains("home", rig.Calls);
    }
    [Theory] [InlineData("error")] [InlineData("no-ack")] [InlineData("not-home")]
    public async Task GreenSwampUncertainCompletionCannotSlewOrResumeAndKeepsDurableFence(string failure)
    {
        string directory = Path.Combine(Path.GetTempPath(), "regain-gss-" + Guid.NewGuid());
        try {
            var api = new SensorHome(); using var rig = new Rig(new MountRecoveryJournal(directory), () => api); api.Rig = rig; rig.Fault();
            if (failure == "error") api.LastAutoHomeError = "Incomplete: RA home sensor not found";
            if (failure == "no-ack") api.Acknowledge = false;
            if (failure == "not-home") api.HomeOnComplete = false;
            await Assert.ThrowsAsync<IOException>(() => rig.Service.RecoverAsync(Target, new(), new Progress<ApplicationStatus>(), default));
            Assert.Equal(1, api.Starts); Assert.Equal(1, api.Stops); Assert.True(api.AscomOn); Assert.True(api.Disposed);
            Assert.DoesNotContain("home", rig.Calls); Assert.DoesNotContain("slew", rig.Calls); Assert.DoesNotContain("guide:start", rig.Calls);
            Assert.True(rig.Service.RequiresInspection); Assert.True(rig.Service.ClearInspection());
        } finally { Directory.Delete(directory, recursive: true); }
    }
    [Theory] [InlineData(false,false)] [InlineData(true,false)] [InlineData(false,true)]
    public async Task GreenSwampCancellationDeadlineAndAmbiguousStartRetainOwnershipUntilActualServerStop(bool ambiguousStart, bool deadline)
    {
        var api = new SensorHome { AutoComplete = false, StartThrows = ambiguousStart };
        using var rig = new Rig(greenSwampApi: () => api); api.Rig = rig; rig.Fault();
        using var cancellation = new CancellationTokenSource();
        var recovery = rig.Service.RecoverAsync(Target, new() { MotionTimeoutSeconds = deadline ? 10 : 180 }, new Progress<ApplicationStatus>(), cancellation.Token);
        await api.Entered.Task.WaitAsync(TimeSpan.FromSeconds(5));
        // For an ambiguous start return, cleanup is already waiting. For a
        // normal start this exercises cancellation of the remote async homing.
        if (ambiguousStart) {
            using var stopLimit = new CancellationTokenSource(TimeSpan.FromSeconds(5));
            while (!api.StopRequested) await Task.Delay(10, stopLimit.Token);
        }
        if (deadline) await Assert.ThrowsAsync<TimeoutException>(() => recovery);
        else { cancellation.Cancel(); await Assert.ThrowsAnyAsync<OperationCanceledException>(() => recovery); }
        using var limit = new CancellationTokenSource(TimeSpan.FromSeconds(5));
        while (!api.StopRequested) await Task.Delay(10, limit.Token);
        Assert.True(rig.Mount.Recovering); Assert.False(api.AscomOn); Assert.False(api.Disposed);
        Assert.False(rig.Service.ClearInspection()); Assert.Throws<InvalidOperationException>(() => rig.Mount.BeginRecovery());
        api.Running = false;
        while (rig.Mount.Recovering) await Task.Delay(10, limit.Token);
        Assert.True(api.AscomOn); Assert.True(api.Disposed); Assert.Equal(1, api.Stops);
        Assert.DoesNotContain("slew", rig.Calls); Assert.DoesNotContain("guide:start", rig.Calls);
    }
    [Fact] public async Task GreenSwampCannotStartSensorSearchWhileTrackingStopIsUnconfirmed()
    {
        var api = new SensorHome(); using var rig = new Rig(greenSwampApi: () => api); api.Rig = rig;
        rig.Upstream.SetupSet(m => m.TrackingEnabled = It.IsAny<bool>()); // Driver ignores the stop write.
        await Assert.ThrowsAsync<IOException>(() => rig.Service.RecoverAsync(Target, new(), new Progress<ApplicationStatus>(), default));
        Assert.Equal(0, api.Starts); Assert.Equal(0, api.Stops); Assert.True(api.AscomOn);
        Assert.DoesNotContain("home", rig.Calls); Assert.DoesNotContain("slew", rig.Calls);
    }
    [Fact] public async Task GreenSwampSafetyChangeAfterSensorSearchPreventsReturnSlew()
    {
        var api = new SensorHome(); using var rig = new Rig(greenSwampApi: () => api); api.Rig = rig; rig.Fault();
        api.OnComplete = () => rig.Safe = false;
        await Assert.ThrowsAsync<IOException>(() => rig.Service.RecoverAsync(Target, new(), new Progress<ApplicationStatus>(), default));
        Assert.True(api.AscomOn); Assert.Equal(1, api.Stops); Assert.DoesNotContain("slew", rig.Calls);
    }
    private static Mock<IDeepSkyObjectContainer> TargetContext()
    {
        var parent = new Mock<IDeepSkyObjectContainer>();
        // The recovery contract needs a target, not native Moon/horizon math.
        // Keep NINA's real InputTarget/context path and stub only that sky model.
        var input = new InputTarget(Angle.ByDegree(31), Angle.ByDegree(-99), null) {
            DeepSkyObject = Mock.Of<IDeepSkyObject>(), InputCoordinates = new(Target)
        };
        parent.SetupGet(p => p.Target).Returns(input); parent.Setup(p => p.Interrupt()).Returns(Task.CompletedTask); return parent;
    }
    [Theory] [InlineData(TrackingMode.Sidereal)] [InlineData(TrackingMode.Lunar)] [InlineData(TrackingMode.Custom)]
    public async Task CompleteRecoveryUsesSavedTargetAndRateDespiteResetTelemetry(TrackingMode mode)
    {
        using var rig = new Rig(); rig.Rate = new() { TrackingMode = mode, CustomRightAscensionRate = mode == TrackingMode.Custom ? .1 : null, CustomDeclinationRate = mode == TrackingMode.Custom ? -.2 : null };
        rig.Mount.Observe(); rig.Fault(); rig.Dec = 90; rig.Ra = 0;
        await rig.Service.RecoverAsync(Target, new(), new Progress<ApplicationStatus>(), default);
        Assert.Equal(3.5, rig.SlewTarget!.RA); Assert.Equal(45, rig.SlewTarget.Dec);
        Assert.Equal(mode, rig.Rate.TrackingMode); Assert.True(rig.Tracking); Assert.True(rig.Guiding); Assert.Null(rig.Mount.Health.Fault);
        Assert.True(rig.Calls.IndexOf("guide:stop") < rig.Calls.IndexOf("home"));
        Assert.True(rig.Calls.IndexOf("home") < rig.Calls.IndexOf("slew"));
        Assert.True(rig.Calls.IndexOf("slew") < rig.Calls.IndexOf("center"));
        Assert.True(rig.Calls.IndexOf("center") < rig.Calls.IndexOf("guide:start"));
    }
    [Fact] public async Task CachedTrackingTelemetryCanCatchUpWithoutReplayingCommands()
    {
        using var rig = new Rig(); rig.Fault(); int staleReads = 2;
        rig.Upstream.SetupGet(m => m.TrackingEnabled).Returns(() => rig.Tracking && Interlocked.Decrement(ref staleReads) < 0);
        await rig.Service.RecoverAsync(Target, new(), new Progress<ApplicationStatus>(), default);
        Assert.Equal(1, rig.Calls.Count(c => c == "home")); Assert.Equal(1, rig.Calls.Count(c => c == "slew"));
        Assert.Null(rig.Mount.Health.Fault); Assert.True(rig.Guiding);
    }
    [Fact] public async Task IntentionallyDisabledTrackingNeverEnablesTrackingOrMoves()
    {
        using var rig = new Rig(); rig.Mount.TrackingEnabled = false; rig.Calls.Clear();
        await rig.Service.RecoverAsync(Target,new(),new Progress<ApplicationStatus>(),default);
        Assert.Empty(rig.Calls); Assert.False(rig.Tracking);
    }
    [Fact] public async Task HomingAndParkingByTheClientDoNotBecomeUnexpectedStops()
    {
        using var rig = new Rig(); await rig.Mount.FindHome(default); rig.Tracking = false;
        rig.Mount.Observe(); rig.Clock.Advance(20); rig.Mount.Observe(); Assert.Null(rig.Mount.Health.Fault);
        Assert.False(rig.Mount.Health.ExpectedTracking);
    }
    [Fact] public async Task SlewFailureCannotStartCenteringOrGuiding()
    {
        using var rig = new Rig(); rig.Fault(); rig.RejectSlew = true;
        await Assert.ThrowsAsync<IOException>(() => rig.Service.RecoverAsync(Target,new(),new Progress<ApplicationStatus>(),default));
        Assert.Equal(1,rig.Calls.Count(c=>c=="home")); Assert.Equal(1,rig.Calls.Count(c=>c=="slew"));
        Assert.DoesNotContain("center",rig.Calls); Assert.DoesNotContain("guide:start",rig.Calls);
    }
    [Fact] public async Task UnsafeStateDuringHomingBlocksEveryLaterPhase()
    {
        using var rig = new Rig(); rig.Fault(); rig.OnHome = _ => { rig.Safe = false; return Task.CompletedTask; };
        await Assert.ThrowsAsync<IOException>(() => rig.Service.RecoverAsync(Target,new(),new Progress<ApplicationStatus>(),default));
        Assert.DoesNotContain("slew",rig.Calls); Assert.DoesNotContain("guide:start",rig.Calls);
    }
    [Fact] public async Task CancellationOfUncooperativeHomingRetainsOwnershipAndNeverStartsLateSlew()
    {
        using var rig = new Rig(); rig.Fault(); var entered = new TaskCompletionSource(); var release = new TaskCompletionSource();
        rig.OnHome = _ => { entered.SetResult(); return release.Task; };
        using var cancellation = new CancellationTokenSource();
        var recovery = rig.Service.RecoverAsync(Target,new(),new Progress<ApplicationStatus>(),cancellation.Token);
        await entered.Task; cancellation.Cancel();
        await Assert.ThrowsAnyAsync<OperationCanceledException>(()=>recovery);
        Assert.True(rig.Mount.Recovering); Assert.Throws<InvalidOperationException>(()=>rig.Mount.BeginRecovery());
        release.SetResult();
        using var limit = new CancellationTokenSource(TimeSpan.FromSeconds(5));
        while (rig.Mount.Recovering) await Task.Delay(10,limit.Token);
        Assert.DoesNotContain("slew",rig.Calls); Assert.DoesNotContain("guide:start",rig.Calls);
    }
    [Fact] public async Task TriggerOnlyRunsBeforeLightFramesAndFailureInterruptsAndPersistsFence()
    {
        using var rig = new Rig(); var trigger = new RecoverMountOnTrackingLoss(rig.Service);
        var parent = TargetContext(); trigger.AttachNewParent(parent.Object);
        var next = new Mock<IExposureItem>(); next.SetupGet(e=>e.ImageType).Returns("LIGHT");
        Assert.False(trigger.ShouldTrigger(null!,next.Object)); rig.Fault();
        Assert.True(trigger.ShouldTrigger(null!,next.Object)); next.SetupGet(e=>e.ImageType).Returns("DARK");
        Assert.False(trigger.ShouldTrigger(null!,next.Object)); rig.CenterFails = true;
        await Assert.ThrowsAsync<IOException>(()=>trigger.Execute(parent.Object,new Progress<ApplicationStatus>(),default));
        Assert.True(trigger.ReconciliationRequired); parent.Verify(p=>p.Interrupt(),Times.Once);
        Assert.DoesNotContain("guide:start",rig.Calls);
        Assert.True(((RecoverMountOnTrackingLoss)trigger.Clone()).ReconciliationRequired);
        Assert.Contains("ReconciliationRequired",JsonConvert.SerializeObject(trigger.Clone()));
        int prior=rig.Calls.Count;
        await Assert.ThrowsAsync<IOException>(()=>trigger.Execute(parent.Object,new Progress<ApplicationStatus>(),default));
        Assert.Equal(prior,rig.Calls.Count);
    }
    [Fact] public async Task FailureInsideNinasRealContainerCancelsWithoutAwaitingItself()
    {
        using var rig = new Rig();
        var container = new SequenceRootContainer();
        var action = new RecoverMount(rig.Service) { ReconciliationRequired = true };
        action.AttachNewParent(container);
        // Invoke Execute inside a real running container, bypassing NINA's
        // preflight target validation so the failure handler is actually reached.
        var invoke = new InvokeRecovery(action); invoke.AttachNewParent(container); container.Items.Add(invoke);
        var run = container.Execute(new Progress<ApplicationStatus>(), default);
        try { await run.WaitAsync(TimeSpan.FromSeconds(5)); } catch (OperationCanceledException) { }
        Assert.True(invoke.Entered); Assert.True(run.IsCompleted); Assert.True(action.ReconciliationRequired); Assert.Empty(rig.Calls);
    }
    [Fact] public void InvalidTriggerStopsItsContainerInsteadOfFallingThroughToImaging()
    {
        using var rig = new Rig(); var parent = TargetContext();
        var trigger = new RecoverMountOnTrackingLoss(rig.Service) { Options = new() { MotionTimeoutSeconds = 0 } };
        trigger.AttachNewParent(parent.Object);
        var light = new Mock<IExposureItem>(); light.SetupGet(e => e.ImageType).Returns("LIGHT");
        Assert.True(trigger.ShouldTrigger(null!, light.Object)); Assert.False(trigger.Validate());
        parent.Verify(p => p.Interrupt(), Times.Once);
    }
    [Fact] public async Task ChangingSelectionCannotClearFenceWhileOldMountOperationIsPending()
    {
        using var rig = new Rig(); rig.Fault();
        var entered = new TaskCompletionSource(); var release = new TaskCompletionSource();
        rig.OnHome = _ => { entered.SetResult(); return release.Task; };
        using var cancellation = new CancellationTokenSource();
        var recovery = rig.Service.RecoverAsync(Target, new(), new Progress<ApplicationStatus>(), cancellation.Token);
        await entered.Task; cancellation.Cancel();
        await Assert.ThrowsAnyAsync<OperationCanceledException>(() => recovery);
        rig.Telescope.Setup(t => t.GetDevice()).Returns((ITelescope)null!);
        var action = new RecoverMount(rig.Service) { ReconciliationRequired = true };
        Assert.False(action.AllowAfterInspection()); Assert.True(action.ReconciliationRequired);
        release.SetResult();
        using var limit = new CancellationTokenSource(TimeSpan.FromSeconds(5));
        while (rig.Mount.Recovering) await Task.Delay(10, limit.Token);
        Assert.True(action.AllowAfterInspection());
    }
    [Fact] public async Task LoadingAnOlderSequenceCannotBypassAnUncertainRecoveryJournal()
    {
        string directory = Path.Combine(Path.GetTempPath(), "regain-mount-" + Guid.NewGuid());
        try {
            using var rig = new Rig(new MountRecoveryJournal(directory)); rig.Fault(); rig.CenterFails = true;
            await Assert.ThrowsAsync<IOException>(() => rig.Service.RecoverAsync(Target, new(), new Progress<ApplicationStatus>(), default));
            var reloaded = new RecoverMount(rig.Service) { ReconciliationRequired = false };
            Assert.True(reloaded.ReconciliationRequired); int calls = rig.Calls.Count;
            await Assert.ThrowsAsync<IOException>(() => reloaded.Execute(new Progress<ApplicationStatus>(), default));
            Assert.Equal(calls, rig.Calls.Count); Assert.True(reloaded.AllowAfterInspection());
            Assert.False(reloaded.ReconciliationRequired);
        } finally { Directory.Delete(directory, recursive: true); }
    }
    [Fact] public void ExportsActionTriggerAndMountProvider()
    {
        Assert.Contains(typeof(MountProvider).GetCustomAttributes(typeof(ExportAttribute),false).Cast<ExportAttribute>(),a=>a.ContractType==typeof(global::NINA.Equipment.Interfaces.ViewModel.IEquipmentProvider));
        Assert.Contains(typeof(RecoverMount).GetCustomAttributes(typeof(ExportAttribute),false).Cast<ExportAttribute>(),a=>a.ContractType==typeof(ISequenceItem));
        Assert.Contains(typeof(RecoverMountOnTrackingLoss).GetCustomAttributes(typeof(ExportAttribute),false).Cast<ExportAttribute>(),a=>a.ContractType==typeof(ISequenceTrigger));
    }
    [Fact] public void NinasActualTriggerConverterRestoresOptionsAndNoReplayFence()
    {
        using var rig = new Rig();
        var source = new RecoverMountOnTrackingLoss(rig.Service) {
            ReconciliationRequired = true, Options = new() { LostTrackingSeconds = 9, MotionTimeoutSeconds = 240, CoordinateJumpDegrees = 2,
                GreenSwampAutoHome = false, GreenSwampSearchDegrees = 120, GreenSwampDeclinationOffsetDegrees = 90 }
        };
        var factory = new Mock<ISequencerFactory>();
        factory.Setup(f => f.GetTrigger<RecoverMountOnTrackingLoss>()).Returns(() => new(rig.Service));
        var json = JsonConvert.SerializeObject(source, new JsonSerializerSettings { TypeNameHandling = TypeNameHandling.All });
        var restored = Assert.IsType<RecoverMountOnTrackingLoss>(JsonConvert.DeserializeObject<ISequenceTrigger>(json,
            new JsonSerializerSettings { Converters = [new SequenceTriggerCreationConverter(factory.Object)] }));
        Assert.True(restored.ReconciliationRequired); Assert.Equal(source.Options, restored.Options);
    }
    [Fact] public async Task ExplicitTrackingIntentWithoutAHealthyRateCannotAuthorizeHoming()
    {
        using var rig = new Rig();
        using var cold = new RegainMount(rig.Upstream.Object);
        cold.TrackingEnabled = true; rig.Calls.Clear();
        rig.Telescope.Setup(t => t.GetDevice()).Returns(cold);
        await Assert.ThrowsAsync<IOException>(() => rig.Service.RecoverAsync(Target,new(),new Progress<ApplicationStatus>(),default));
        Assert.Empty(rig.Calls);
    }
    [Fact] public async Task GetterCompletingAfterDeadlineCannotSendTrackingOrSlewCommands()
    {
        using var rig = new Rig(); rig.Fault();
        var entered = new TaskCompletionSource(TaskCreationOptions.RunContinuationsAsynchronously);
        using var release = new ManualResetEventSlim();
        rig.Upstream.SetupGet(t => t.AtHome).Returns(() => { entered.TrySetResult(); release.Wait(); return true; });
        try {
            var recovery = rig.Service.RecoverAsync(Target,new() { MotionTimeoutSeconds = 10 },new Progress<ApplicationStatus>(),default);
            await entered.Task.WaitAsync(TimeSpan.FromSeconds(5));
            await Assert.ThrowsAsync<TimeoutException>(() => recovery);
            Assert.True(rig.Mount.Recovering);
        } finally {
            release.Set();
            using var limit = new CancellationTokenSource(TimeSpan.FromSeconds(5));
            while (rig.Mount.Recovering) await Task.Delay(10,limit.Token);
        }
        Assert.DoesNotContain("slew",rig.Calls);
        Assert.DoesNotContain(rig.Calls,c => c.StartsWith("tracking:") || c.StartsWith("rate:"));
    }
    [Theory] [InlineData(false)] [InlineData(true)]
    public async Task ProductionCenteringAdapterNeverRestartsGuidingItself(bool success)
    {
        var guider = new Mock<IGuiderMediator>(); var centering = new SolverFixture(guider.Object,success);
        if (success) await centering.CenterVerifiedAsync(Target,new Progress<ApplicationStatus>(),default);
        else await Assert.ThrowsAsync<IOException>(() => centering.CenterVerifiedAsync(Target,new Progress<ApplicationStatus>(),default));
        guider.Verify(g => g.StartGuiding(It.IsAny<bool>(),It.IsAny<IProgress<ApplicationStatus>>(),It.IsAny<CancellationToken>()),Times.Never);
        guider.Verify(g => g.StopGuiding(It.IsAny<CancellationToken>()),Times.Never);
    }
}
