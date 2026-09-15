namespace ZwoGain.Core;

/// <summary>Recognizes sustained regulation when cooldown power is no longer needed.</summary>
internal sealed class CoolingSetpointHold(double target, double tolerance, double sampleSeconds)
{
    internal const double RequiredSeconds = 30;
    private double? since;
    private double firstTemperature;
    private double? lastSample;
    public double HeldSeconds { get; private set; }

    public bool Observe(double? temperature, long? power, double elapsedSeconds)
    {
        bool valid = temperature.HasValue && double.IsFinite(temperature.Value) && power > 0 &&
            Math.Abs(temperature.Value - target) <= Math.Min(tolerance, 1.0);
        if (!valid)
        {
            since = null;
            lastSample = null;
            HeldSeconds = 0;
            return false;
        }
        // Do not count a telemetry gap or a steadily warming sensor as a hold.
        if (since is null || elapsedSeconds < lastSample ||
            elapsedSeconds - lastSample > Math.Max(5, sampleSeconds * 2) ||
            temperature!.Value > firstTemperature + 0.2)
        {
            since = elapsedSeconds;
            firstTemperature = temperature!.Value;
        }
        lastSample = elapsedSeconds;
        HeldSeconds = elapsedSeconds - since.Value;
        return HeldSeconds >= RequiredSeconds;
    }
}
