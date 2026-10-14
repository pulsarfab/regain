namespace Regain.Core;

public sealed record MountRecoveryOptions
{
    public double LostTrackingSeconds { get; init; } = 5;
    public double MotionTimeoutSeconds { get; init; } = 180;
    public double CoordinateJumpDegrees { get; init; } = 5;
    public bool GreenSwampAutoHome { get; init; } = true;
    public int GreenSwampSearchDegrees { get; init; } = 100;
    public int GreenSwampDeclinationOffsetDegrees { get; init; }
    public void Validate()
    {
        if (!double.IsFinite(LostTrackingSeconds) || LostTrackingSeconds is < 1 or > 60 ||
            !double.IsFinite(MotionTimeoutSeconds) || MotionTimeoutSeconds is < 10 or > 900 ||
            !double.IsFinite(CoordinateJumpDegrees) || CoordinateJumpDegrees is <= 0 or > 90)
            throw new ArgumentOutOfRangeException(nameof(MountRecoveryOptions), "Use 1–60 seconds for detection, 10–900 seconds for recovery, and a 0–90 degree jump threshold.");
        if (GreenSwampSearchDegrees is < 20 or > 179 || GreenSwampDeclinationOffsetDegrees is < -90 or > 90)
            throw new ArgumentOutOfRangeException(nameof(MountRecoveryOptions), "Green Swamp AutoHome requires a 20–179 degree search limit and a −90–90 degree declination offset.");
    }
}

public sealed record MountObservation(bool Connected, bool Tracking, bool Slewing, double RightAscensionHours, double DeclinationDegrees);

/// <summary>Observations never authorize motion. Only the sequencer may consume a fault.</summary>
public sealed class MountRecoveryMonitor(TimeProvider? clock = null)
{
    private readonly object gate = new();
    private readonly TimeProvider time = clock ?? TimeProvider.System;
    private MountRecoveryOptions options = new();
    private MountObservation? good;
    private long? suspectSince;
    private bool expectedTracking;
    private string? fault;
    public MountRecoveryOptions Options { get { lock (gate) return options; } set { value.Validate(); lock (gate) options = value; } }
    public bool ExpectedTracking { get { lock (gate) return expectedTracking; } }
    public string? Fault { get { lock (gate) return fault; } }
    public void Intent(bool tracking)
    {
        lock (gate) { expectedTracking = tracking; good = null; suspectSince = null; fault = null; }
    }
    public void Reset() { lock (gate) { good = null; suspectSince = null; fault = null; } }
    public void ResetPositionBaseline() { lock (gate) { good = null; suspectSince = null; } }
    public bool Observe(MountObservation value, bool commandedMotion = false)
    {
        lock (gate) {
            if (fault is not null) return false; // Re-enabling tracking alone cannot reconcile lost pointing.
            if (value.Connected && (commandedMotion || value.Slewing)) { suspectSince = null; good = null; return false; }
            bool finite = double.IsFinite(value.RightAscensionHours) && value.RightAscensionHours is >= 0 and < 24 &&
                double.IsFinite(value.DeclinationDegrees) && value.DeclinationDegrees is >= -90 and <= 90;
            string? reason = !value.Connected ? "Mount disconnected" : !finite ? "Mount position unavailable" :
                expectedTracking && !value.Tracking ? "Tracking stopped unexpectedly" :
                expectedTracking && good is not null && Separation(good, value) > options.CoordinateJumpDegrees ? "Mount position jumped without a commanded slew" : null;
            if (!expectedTracking) reason = null;
            if (reason is not null) {
                suspectSince ??= time.GetTimestamp();
                if (time.GetElapsedTime(suspectSince.Value).TotalSeconds >= options.LostTrackingSeconds) fault = reason;
                return false;
            }
            suspectSince = null;
            if (value.Connected && value.Tracking && finite) { expectedTracking = true; good = value; return true; }
            return false;
        }
    }
    private static double Separation(MountObservation a, MountObservation b)
    {
        double radians = Math.PI / 180, d1 = a.DeclinationDegrees * radians, d2 = b.DeclinationDegrees * radians;
        return Math.Acos(Math.Clamp(Math.Sin(d1) * Math.Sin(d2) + Math.Cos(d1) * Math.Cos(d2) *
            Math.Cos((a.RightAscensionHours - b.RightAscensionHours) * 15 * radians), -1, 1)) / radians;
    }
}
