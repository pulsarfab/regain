using System.Text.Json;
using Regain.Core;
using Xunit;

namespace Regain.Tests;

public sealed class RecoveryConfigurationTests
{
    [Fact]
    public void GeneratedOptionsKeepTheShippedDefaultsAndSparsePascalCaseFileFormat()
    {
        var old = JsonSerializer.Deserialize<RecoveryOptions>("""
            {"MaxRetries":3,"MaximumRetryExposureSeconds":30,"ReconnectDelaySeconds":5,
             "CommandTimeoutSeconds":15,"DownloadTimeoutSeconds":60,"ExposureGraceSeconds":30,
             "CoolingTimeoutSeconds":300,"TemperatureToleranceC":2,"CoolingStableSamples":3,
             "CoolingSampleSeconds":2,"ReadyFrameDownloadRetries":2,"DirectReadRetries":2,
             "UsbResetAfterFailures":0,"UsbPortCycle":false}
            """)!;
        Assert.Equal(new RecoveryOptions(),old);
        var sparse=JsonSerializer.Deserialize<RecoveryOptions>("""{"MaxRetries":0,"UsbPortCycle":true,"legacyExtension":1}""")!;
        Assert.Equal(old with {MaxRetries=0,UsbPortCycle=true},sparse);
        Assert.Equal(3,old.PreExposureReconnectAttempts);
        var serialized=JsonSerializer.SerializeToElement(old);
        Assert.Equal(18,serialized.EnumerateObject().Count());
        Assert.Equal(3,serialized.GetProperty("PreExposureReconnectAttempts").GetInt32());
        var explicitAttempts=JsonSerializer.Deserialize<RecoveryOptions>("""{"MaxRetries":0,"PreExposureReconnectAttempts":5}""")!;
        Assert.Equal(0,explicitAttempts.MaxRetries);
        Assert.Equal(5,explicitAttempts.PreExposureReconnectAttempts);
        sparse.Validate();
    }

    [Theory]
    [InlineData(1,true)] [InlineData(3,true)] [InlineData(10,true)]
    [InlineData(0,false)] [InlineData(11,false)] [InlineData(-1,false)]
    public void PreExposureReconnectAttemptsHaveAnIndependentBoundedBudget(int attempts,bool valid)
    {
        var options=new RecoveryOptions {MaxRetries=0,PreExposureReconnectAttempts=attempts};
        if(valid) options.Validate(); else Assert.Throws<ArgumentOutOfRangeException>(options.Validate);
    }

    [Theory]
    [InlineData(1,true)] [InlineData(25,true)] [InlineData(100,true)]
    [InlineData(0,false)] [InlineData(100.01,false)]
    [InlineData(double.NaN,false)] [InlineData(double.PositiveInfinity,false)]
    public void CoolerPowerChangeUsesFinitePercentagePointBounds(double value,bool valid)
    {
        var options=new RecoveryOptions {CoolerMaxPowerChangePer5Seconds=value,TransferLogMode="off"};
        if(valid) options.Validate(); else Assert.Throws<ArgumentOutOfRangeException>(options.Validate);
    }

    [Theory]
    [InlineData(0.000001,true)] [InlineData(3600,true)]
    [InlineData(0,false)] [InlineData(3600.000001,false)]
    [InlineData(double.NaN,false)] [InlineData(double.PositiveInfinity,false)]
    public void GeneratedValidationKeepsPositiveTimeoutBounds(double value,bool valid)
    {
        var options=new RecoveryOptions {ReconnectDelaySeconds=value};
        if(valid) options.Validate(); else Assert.Throws<ArgumentOutOfRangeException>(options.Validate);
    }
}
