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
        Assert.Equal(15,JsonSerializer.SerializeToElement(old).EnumerateObject().Count());
        sparse.Validate();
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
