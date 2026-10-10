using Regain.Core;
using Xunit;

namespace Regain.Tests;

public class CameraRetryStatusTests
{
    [Fact]
    public void DriverInfoIsBoundedWithoutDiscardingDiagnosticDetails()
    {
        var failure = "download: ASI error 11\r\n" + new string('x', 2000);
        var status = new CameraRetryStatus(1, 2, 3, failure);
        var info = status.DriverInfo(new string('y', 2000));
        Assert.True(info.Length < 100);
        Assert.Contains("retries: 6", info);
        Assert.Contains("last: download: ASI error 11", info);
        Assert.DoesNotContain('\r', info);
        Assert.DoesNotContain('\n', info);
        Assert.Equal(failure, status.LastFailure);
        Assert.Equal(1, status.Recaptures);
        Assert.Equal(2, status.Downloads);
        Assert.Equal(3, status.UsbReads);
    }
}
