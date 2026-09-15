using ZwoGain.Core;
using Xunit;

namespace ZwoGain.Tests;

public class CoolingSetpointHoldTests
{
    [Fact]
    public void P25SdkCanHoldTargetAtLessThanThePreviousCooldownOutput()
    {
        var hold = new CoolingSetpointHold(20, 2, 2);
        // Hardware recovery held 19.4–20.9 C at 6–9%, below the prior 25% demand.
        for (int t = 0; t < 30; t += 2)
            Assert.False(hold.Observe(20.9 - t * 0.05, 9, t));
        Assert.True(hold.Observe(19.4, 6, 30));
    }

    [Fact]
    public void ThermalInertiaAtPriorTemperatureDoesNotSatisfyColderTarget()
    {
        var hold = new CoolingSetpointHold(-10, 2, 2);
        for (int t = 0; t <= 90; t += 2)
            Assert.False(hold.Observe(3.2, 1, t));
    }

    [Fact]
    public void WarmingInsideTargetBandDoesNotAccumulateAHold()
    {
        var hold = new CoolingSetpointHold(20, 2, 2);
        for (int t = 0; t <= 60; t += 2)
            Assert.False(hold.Observe(19.4 + t * 0.015, 1, t));
    }

    [Fact]
    public void MissingTelemetryAndZeroPowerResetTheHold()
    {
        var hold = new CoolingSetpointHold(20, 2, 2);
        for (int t = 0; t < 30; t += 2) Assert.False(hold.Observe(20, 9, t));
        Assert.False(hold.Observe(null, null, 30));
        for (int t = 32; t < 62; t += 2) Assert.False(hold.Observe(20, 9, t));
        Assert.False(hold.Observe(20, 0, 62));
        Assert.False(hold.Observe(20, 9, 64));
    }

    [Fact]
    public void DelayedSampleDoesNotProveTemperatureWasHeldDuringTheGap()
    {
        var hold = new CoolingSetpointHold(20, 2, 2);
        Assert.False(hold.Observe(20, 9, 0));
        Assert.False(hold.Observe(20, 9, 60));
        for (int t = 62; t < 90; t += 2) Assert.False(hold.Observe(20, 9, t));
        Assert.True(hold.Observe(20, 9, 90));
    }

    [Fact]
    public void UserToleranceAndOneDegreeMaximumAreBothRespected()
    {
        var strict = new CoolingSetpointHold(20, 0.1, 2);
        var broad = new CoolingSetpointHold(20, 5, 2);
        for (int t = 0; t <= 60; t += 2)
        {
            Assert.False(strict.Observe(20.2, 9, t));
            Assert.False(broad.Observe(21.1, 9, t));
        }
    }
}
