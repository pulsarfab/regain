using Regain.TestFixtures;
using Xunit;

namespace Regain.NINA.Tests;

public sealed class HubTimingTests
{
    [Fact] public Task OnlyCameraAcknowledgementsGainNegotiatedTime() => HubTimingFixture.Semantics();
    public static IEnumerable<object[]> Faults => HubTimingFixture.Faults.Select(value => new object[] { value });
    [Theory, MemberData(nameof(Faults))]
    public Task TimingDescriptorsRequireExactIdentityAndFiniteBound(string fault) => HubTimingFixture.Malformed(fault);
    [Fact] public Task CaptureTimingKeepsDurationAndCompletionBounds() => HubTimingFixture.CaptureSemantics();
    public static IEnumerable<object[]> CaptureFaults => HubTimingFixture.CaptureFaults.Select(value => new object[] { value });
    [Theory, MemberData(nameof(CaptureFaults))]
    public Task CaptureTimingRejectsMismatchedIdentityDurationAndInvalidBounds(string fault) => HubTimingFixture.MalformedCapture(fault);
}
